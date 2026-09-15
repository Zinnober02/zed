//! OHOS input-method bridge (native C API).
//!
//! The IME drives a TextEditorProxy from a non-UI thread, so callbacks only
//! enqueue commands or read a cursor-context snapshot; OhosPlatform::tick
//! drains the queue on the UI thread and pushes text/selection updates back to
//! the input method, which is what makes it enter composing mode.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

pub(crate) enum ImeCommand {
    Commit(String),
    Backspace(usize),
    DeleteForward(usize),
    Preview { text: String, start: i32, end: i32 },
    ClearPreview,
    MoveCursor(i32),
}

static QUEUE: Mutex<Vec<ImeCommand>> = Mutex::new(Vec::new());
static ATTACHED: Mutex<bool> = Mutex::new(false);
/// What the input method is doing, and for which window.
///
/// One client serves the whole process and the system binds it to whichever window
/// holds focus, so "asked for" and "showing" are genuinely different states: a
/// request made while another window had focus cannot be served until its own
/// window is focused, which is what the retry below waits for.
#[derive(Clone, PartialEq)]
enum ImeSession {
    /// Nobody asked, or the window that had is gone.
    Idle,
    /// A window's field asked; the input method is not showing for it yet.
    Wanted { owner: String },
    /// Showing for this window.
    Showing { owner: String },
}

static SESSION: Mutex<ImeSession> = Mutex::new(ImeSession::Idle);

impl ImeSession {
    fn owner(&self) -> Option<&str> {
        match self {
            ImeSession::Idle => None,
            ImeSession::Wanted { owner } | ImeSession::Showing { owner } => Some(owner),
        }
    }
}
/// Frames left before the next attempt, so a refusal is retried without asking
/// (and logging) on every single frame.
static RETRY_IN: AtomicU32 = AtomicU32::new(0);
const RETRY_FRAMES: u32 = 30;
/// (full text, caret index in UTF-16, absolute caret rect) mirrored from the
/// focused editor.
static IME_CONTEXT: Mutex<(String, usize, (f64, f64, f64, f64))> =
    Mutex::new((String::new(), 0, (0.0, 0.0, 2.0, 20.0)));
static PROXY: AtomicUsize = AtomicUsize::new(0);
static SET_INPUT_TYPE: OnceLock<unsafe extern "C" fn(*mut c_void, i32) -> i32> = OnceLock::new();
static NOTIFY_SELECTION: OnceLock<
    unsafe extern "C" fn(*mut c_void, *mut u16, usize, i32, i32) -> i32,
> = OnceLock::new();
static NOTIFY_CURSOR: OnceLock<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32> =
    OnceLock::new();
static CURSOR_CREATE: OnceLock<unsafe extern "C" fn(f64, f64, f64, f64) -> *mut c_void> =
    OnceLock::new();
static SHOW_TEXT_INPUT: OnceLock<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32> =
    OnceLock::new();
static HIDE_TEXT_INPUT: OnceLock<unsafe extern "C" fn(*mut c_void) -> i32> = OnceLock::new();
static OPTIONS: AtomicUsize = AtomicUsize::new(0);

fn utf16_to_string(text: *const u16, length: usize) -> String {
    if text.is_null() || length == 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(text, length) };
    String::from_utf16_lossy(slice)
}

unsafe extern "C" fn on_insert(_proxy: *mut c_void, text: *const u16, length: usize) {
    let value = utf16_to_string(text, length);
    if !value.is_empty() {
        super::vk::log(&format!(
            "[gpui_ohos] ime insert: {} char(s)",
            value.chars().count()
        ));
        QUEUE.lock().unwrap().push(ImeCommand::Commit(value));
    }
}

unsafe extern "C" fn on_delete_backward(_proxy: *mut c_void, length: i32) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::Backspace(length.max(1) as usize));
}

unsafe extern "C" fn on_delete_forward(_proxy: *mut c_void, length: i32) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::DeleteForward(length.max(1) as usize));
}

unsafe extern "C" fn on_preview(
    _proxy: *mut c_void,
    text: *const u16,
    length: usize,
    start: i32,
    end: i32,
) -> i32 {
    let composing = utf16_to_string(text, length);
    *PREVIEW.lock().unwrap() = composing.clone();
    QUEUE.lock().unwrap().push(ImeCommand::Preview {
        text: composing,
        start,
        end,
    });
    0
}

unsafe extern "C" fn on_finish_preview(_proxy: *mut c_void) {
    PREVIEW.lock().unwrap().clear();
    QUEUE.lock().unwrap().push(ImeCommand::ClearPreview);
}

unsafe extern "C" fn on_get_text_config(_proxy: *mut c_void, config: *mut c_void) {
    if let Some(set) = SET_INPUT_TYPE.get() {
        // SAFETY: the host passes a valid config object and the setter is the
        // one registered by the input method framework.
        // 1 = IME_TEXT_INPUT_TYPE_MULTILINE
        unsafe { set(config, 1) };
    }
}

unsafe extern "C" fn on_text_index_at_cursor(_proxy: *mut c_void) -> i32 {
    IME_CONTEXT.lock().unwrap().1 as i32
}

unsafe extern "C" fn write_text_slice(number: i32, text: *mut u16, length: *mut usize, left: bool) {
    let context = IME_CONTEXT.lock().unwrap();
    let chars: Vec<u16> = context.0.encode_utf16().collect();
    let caret = context.1.min(chars.len());
    let count = number.max(0) as usize;
    let slice = if left {
        &chars[caret.saturating_sub(count)..caret]
    } else {
        &chars[caret..(caret + count).min(chars.len())]
    };
    if !text.is_null() && !length.is_null() {
        // SAFETY: both pointers are non-null and the capacity came from the
        // caller, which owns a buffer of that size.
        unsafe {
            let capacity = *length;
            let copied = slice.len().min(capacity);
            std::ptr::copy_nonoverlapping(slice.as_ptr(), text, copied);
            *length = copied;
        }
    }
}

unsafe extern "C" fn on_get_left_text(
    _proxy: *mut c_void,
    number: i32,
    text: *mut u16,
    length: *mut usize,
) {
    // SAFETY: the pointers come straight from the input method.
    unsafe { write_text_slice(number, text, length, true) };
}

unsafe extern "C" fn on_get_right_text(
    _proxy: *mut c_void,
    number: i32,
    text: *mut u16,
    length: *mut usize,
) {
    // SAFETY: the pointers come straight from the input method.
    unsafe { write_text_slice(number, text, length, false) };
}

/// The text the input method is composing right now.
///
/// Pressing enter confirms that composition, and the input method reports the
/// text only through the preview callbacks - the enter callback says nothing
/// about it. Committing a bare newline instead made the composition vanish in a
/// single-line field without ever being inserted.
static PREVIEW: Mutex<String> = Mutex::new(String::new());

unsafe extern "C" fn on_enter_key(_proxy: *mut c_void, _kind: i32) {
    let composing = std::mem::take(&mut *PREVIEW.lock().unwrap());
    super::vk::log(&format!(
        "[gpui_ohos] ime enter: composing {} char(s)",
        composing.chars().count()
    ));
    if composing.is_empty() {
        // Nothing was being composed: this is an ordinary enter, and the
        // application gets it through the ordinary key path.
        return;
    }
    QUEUE.lock().unwrap().push(ImeCommand::Commit(composing));
}

unsafe extern "C" fn on_move_cursor(_proxy: *mut c_void, direction: i32) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::MoveCursor(direction));
}

unsafe extern "C" fn on_noop(_proxy: *mut c_void) {}
unsafe extern "C" fn on_noop_i32(_proxy: *mut c_void, _value: i32) {}
unsafe extern "C" fn on_noop_two(_proxy: *mut c_void, _a: i32, _b: i32) {}
unsafe extern "C" fn on_private_command(
    _proxy: *mut c_void,
    _command: *mut c_void,
    _size: usize,
) -> i32 {
    0
}

/// Attach the application to the system input method. Safe to call repeatedly.
pub(crate) fn attach() {
    let mut attached = ATTACHED.lock().unwrap();
    if *attached {
        return;
    }
    unsafe {
        let lib = dlopen(
            CString::new("libohinputmethod.so").unwrap().as_ptr(),
            RTLD_NOW,
        );
        if lib.is_null() {
            super::vk::log("[gpui_ohos] inputmethod library not found");
            return;
        }
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let p = dlsym(lib, CString::new($name).unwrap().as_ptr());
                if p.is_null() {
                    super::vk::log(concat!("[gpui_ohos] missing IME symbol: ", $name));
                    return;
                }
                std::mem::transmute::<*mut c_void, $ty>(p)
            }};
        }

        type ProxyCreate = unsafe extern "C" fn() -> *mut c_void;
        type ProxySet = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type SetInputType = unsafe extern "C" fn(*mut c_void, i32) -> i32;
        type OptionsCreate = unsafe extern "C" fn(bool) -> *mut c_void;
        type Attach = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;
        type ShowTextInput = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
        type HideTextInput = unsafe extern "C" fn(*mut c_void) -> i32;
        type NotifySelection = unsafe extern "C" fn(*mut c_void, *mut u16, usize, i32, i32) -> i32;
        type NotifyCursor = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
        type CursorCreate = unsafe extern "C" fn(f64, f64, f64, f64) -> *mut c_void;

        let proxy_create: ProxyCreate = sym!("OH_TextEditorProxy_Create", ProxyCreate);
        let set_insert: ProxySet = sym!("OH_TextEditorProxy_SetInsertTextFunc", ProxySet);
        let set_backward: ProxySet = sym!("OH_TextEditorProxy_SetDeleteBackwardFunc", ProxySet);
        let set_forward: ProxySet = sym!("OH_TextEditorProxy_SetDeleteForwardFunc", ProxySet);
        let set_preview: ProxySet = sym!("OH_TextEditorProxy_SetSetPreviewTextFunc", ProxySet);
        let set_finish: ProxySet = sym!("OH_TextEditorProxy_SetFinishTextPreviewFunc", ProxySet);
        let set_config: ProxySet = sym!("OH_TextEditorProxy_SetGetTextConfigFunc", ProxySet);
        let set_status: ProxySet = sym!("OH_TextEditorProxy_SetSendKeyboardStatusFunc", ProxySet);
        let set_enter: ProxySet = sym!("OH_TextEditorProxy_SetSendEnterKeyFunc", ProxySet);
        let set_move: ProxySet = sym!("OH_TextEditorProxy_SetMoveCursorFunc", ProxySet);
        let set_selection: ProxySet =
            sym!("OH_TextEditorProxy_SetHandleSetSelectionFunc", ProxySet);
        let set_extend: ProxySet = sym!("OH_TextEditorProxy_SetHandleExtendActionFunc", ProxySet);
        let set_left: ProxySet = sym!("OH_TextEditorProxy_SetGetLeftTextOfCursorFunc", ProxySet);
        let set_right: ProxySet = sym!("OH_TextEditorProxy_SetGetRightTextOfCursorFunc", ProxySet);
        let set_index: ProxySet = sym!("OH_TextEditorProxy_SetGetTextIndexAtCursorFunc", ProxySet);
        let set_private: ProxySet =
            sym!("OH_TextEditorProxy_SetReceivePrivateCommandFunc", ProxySet);
        let set_input_type: SetInputType = sym!("OH_TextConfig_SetInputType", SetInputType);
        let options_create: OptionsCreate = sym!("OH_AttachOptions_Create", OptionsCreate);
        let attach: Attach = sym!("OH_InputMethodController_Attach", Attach);
        let show_text_input: ShowTextInput =
            sym!("OH_InputMethodProxy_ShowTextInput", ShowTextInput);
        // Optional: older system images do not export it, and a missing hide
        // must not abort the whole attach.
        let hide_raw = dlsym(
            lib,
            CString::new("OH_InputMethodProxy_HideKeyboard")
                .unwrap()
                .as_ptr(),
        );
        let hide_text_input: Option<HideTextInput> = if hide_raw.is_null() {
            super::vk::log("[gpui_ohos] missing IME symbol: OH_InputMethodProxy_HideKeyboard");
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, HideTextInput>(hide_raw))
        };
        let notify_selection: NotifySelection =
            sym!("OH_InputMethodProxy_NotifySelectionChange", NotifySelection);
        let notify_cursor: NotifyCursor =
            sym!("OH_InputMethodProxy_NotifyCursorUpdate", NotifyCursor);
        let cursor_create: CursorCreate = sym!("OH_CursorInfo_Create", CursorCreate);
        let _ = SET_INPUT_TYPE.set(set_input_type);
        let _ = NOTIFY_SELECTION.set(notify_selection);
        let _ = NOTIFY_CURSOR.set(notify_cursor);
        let _ = CURSOR_CREATE.set(cursor_create);
        let _ = SHOW_TEXT_INPUT.set(show_text_input);
        if let Some(hide) = hide_text_input {
            let _ = HIDE_TEXT_INPUT.set(hide);
        }

        let proxy = proxy_create();
        if proxy.is_null() {
            super::vk::log("[gpui_ohos] OH_TextEditorProxy_Create failed");
            return;
        }
        set_insert(proxy, on_insert as *const c_void);
        set_backward(proxy, on_delete_backward as *const c_void);
        set_forward(proxy, on_delete_forward as *const c_void);
        set_preview(proxy, on_preview as *const c_void);
        set_finish(proxy, on_finish_preview as *const c_void);
        set_config(proxy, on_get_text_config as *const c_void);
        set_status(proxy, on_noop_i32 as *const c_void);
        set_enter(proxy, on_enter_key as *const c_void);
        set_move(proxy, on_move_cursor as *const c_void);
        set_selection(proxy, on_noop_two as *const c_void);
        set_extend(proxy, on_noop_i32 as *const c_void);
        set_left(proxy, on_get_left_text as *const c_void);
        set_right(proxy, on_get_right_text as *const c_void);
        set_index(proxy, on_text_index_at_cursor as *const c_void);
        set_private(proxy, on_private_command as *const c_void);

        let options = options_create(true);
        OPTIONS.store(options as usize, Ordering::Relaxed);
        let mut proxy_out: *mut c_void = std::ptr::null_mut();
        let code = attach(proxy, options, &mut proxy_out);
        super::vk::log(&format!("[gpui_ohos] IME attach code={code}"));
        if code == 0 && !proxy_out.is_null() {
            PROXY.store(proxy_out as usize, Ordering::Relaxed);
            let show_code = show_text_input(proxy_out, options);
            super::vk::log(&format!("[gpui_ohos] IME showTextInput code={show_code}"));
            // Only a client the system accepted may count as attached: it refuses
            // while the window it would bind to is not focused yet (12800003), and
            // remembering that refusal as success stopped every later attempt.
            *attached = true;
        }
        let _ = on_noop as *const c_void;
    }
}

/// Re-enter the editing state so the input method accepts keys again.
///
/// The IME leaves the editing state whenever its panel hides (window blur,
/// focus moving elsewhere), and only ShowTextInput restores it. Attaching
/// once at startup is not enough: the client must show again once the editor
/// actually holds focus, otherwise every key is dropped with "keyEvent is not
/// consumed by ime".
fn try_show() -> i32 {
    let proxy = PROXY.load(Ordering::Relaxed) as *mut c_void;
    let options = OPTIONS.load(Ordering::Relaxed) as *mut c_void;
    if proxy.is_null() || options.is_null() {
        return -1;
    }
    match SHOW_TEXT_INPUT.get() {
        Some(show) => unsafe { show(proxy, options) },
        None => -1,
    }
}

/// Move to a new state and say so.
///
/// The transitions are the whole story of a session, and the ones that go wrong
/// are the ones worth reading: a window that stays "wanted" is one the system has
/// not accepted yet.
fn set_session(next: ImeSession) {
    let mut session = SESSION.lock().unwrap();
    if *session == next {
        return;
    }
    let described = match &next {
        ImeSession::Idle => "idle".to_string(),
        ImeSession::Wanted { owner } => format!("wanted for {owner}"),
        ImeSession::Showing { owner } => format!("showing for {owner}"),
    };
    let showing = matches!(next, ImeSession::Showing { .. });
    *session = next;
    if showing {
        super::vk::log(&format!("[gpui_ohos] IME showing for {described}"));
    } else {
        super::vk::log(&format!("[gpui_ohos] IME session: {described}"));
    }
}

fn session() -> ImeSession {
    SESSION.lock().unwrap().clone()
}

/// A window's text field asked for the input method.
pub fn show(owner: &str) {
    set_session(ImeSession::Wanted {
        owner: owner.to_string(),
    });
    RETRY_IN.store(0, Ordering::Relaxed);
}

/// A window's text field gave the input method up.
///
/// Only the owner may end the session. A window that merely lost focus reported
/// this as well, and hiding there ended the session of the window that had just
/// taken focus over - the panel stayed, and everything typed went nowhere.
pub fn hide(owner: &str) {
    let current = session();
    if current.owner() != Some(owner) {
        return;
    }
    let was_showing = matches!(current, ImeSession::Showing { .. });
    set_session(ImeSession::Idle);
    if !was_showing {
        return;
    }
    let proxy = PROXY.load(Ordering::Relaxed) as *mut c_void;
    if proxy.is_null() {
        return;
    }
    if let Some(hide) = HIDE_TEXT_INPUT.get() {
        let code = unsafe { hide(proxy) };
        super::vk::log(&format!("[gpui_ohos] IME hide code={code}"));
    }
}

/// The window the input method is bound to, if one asked for it.
pub(crate) fn owner() -> Option<String> {
    session().owner().map(|owner| owner.to_string())
}

/// Forget a window that is gone: a dead owner would otherwise keep the retry
/// below asking the system about a window that no longer exists.
pub(crate) fn forget(id: &str) {
    {
        let session = SESSION.lock().unwrap();
        if session.owner() != Some(id) {
            return;
        }
    }
    super::vk::log(&format!(
        "[gpui_ohos] IME session: idle, window {id} is gone"
    ));
    *SESSION.lock().unwrap() = ImeSession::Idle;
}

/// Replay a pending request once its window holds focus, and keep retrying.
///
/// The system binds the client to the focused window only: asking while another
/// window has focus answers 12800003 IME_ERR_IMCLIENT ("the edit box is not
/// focused"), and it detaches the client whenever focus moves (12800009). The
/// application asks exactly once per focus change, so a single refusal used to
/// leave that window without an input method for good.
pub(crate) fn apply(active: Option<&str>) {
    let owner = match session() {
        ImeSession::Wanted { owner } => owner,
        // A session that is idle has nothing to replay, and one that is already
        // showing must not be asked again: repeating show resets a composition.
        _ => return,
    };
    if active != Some(owner.as_str()) {
        return;
    }
    if RETRY_IN.load(Ordering::Relaxed) > 0 {
        RETRY_IN.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let code = show_now();
    if code == 0 {
        set_session(ImeSession::Showing { owner });
    } else {
        RETRY_IN.store(RETRY_FRAMES, Ordering::Relaxed);
        log_refusal(code, &owner);
    }
}

/// Ask for the input method, attaching first when the system had detached the
/// client. Returns the code the system answered with.
fn show_now() -> i32 {
    let code = try_show();
    if code == 0 {
        return 0;
    }
    // 12800009 IME_ERR_DETACHED: the system unbinds the client when its window
    // loses focus, and a detached client rejects ShowTextInput. Bind again.
    *ATTACHED.lock().unwrap() = false;
    *IME_CONTEXT.lock().unwrap() = (String::new(), 0, (0.0, 0.0, 2.0, 20.0));
    attach();
    try_show()
}

/// Report a repeating refusal once per code, the way the context notifications do.
fn log_refusal(code: i32, owner: &str) {
    static LAST_REFUSAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    if LAST_REFUSAL.swap(code, Ordering::Relaxed) == code {
        return;
    }
    super::vk::log(&format!(
        "[gpui_ohos] IME refused for {owner}: code={code}; retrying"
    ));
}

pub(crate) fn drain() -> Vec<ImeCommand> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}

/// Most UTF-16 units one notification may carry. The input method replaces its
/// text with whatever it is given and rejects anything longer, so sending the
/// whole document silently disabled composition in every larger file.
const IME_TEXT_LIMIT: usize = 8192;

/// Mirror the focused editor's text and caret to the input method. The IME
/// needs these notifications to start composing, and only ever sees a window
/// around the caret.
pub(crate) fn update_context(text: &str, caret: usize, cursor: (f64, f64, f64, f64)) {
    {
        let context = IME_CONTEXT.lock().unwrap();
        if context.0 == text && context.1 == caret && context.2 == cursor {
            return;
        }
    }
    let proxy = PROXY.load(Ordering::Relaxed) as *mut c_void;
    if proxy.is_null() {
        return;
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let caret_units = caret.min(utf16.len());
    // Only the window around the caret is sent, with the offsets made relative
    // to it: the input method reads them against the text it was handed.
    let half = IME_TEXT_LIMIT / 2;
    let start = caret_units
        .saturating_sub(half)
        .min(utf16.len().saturating_sub(IME_TEXT_LIMIT));
    let end = (start + IME_TEXT_LIMIT).min(utf16.len());
    let window = &mut utf16[start..end];
    let caret_in_window = caret_units - start;
    let length = window.len();
    let mut accepted = false;
    unsafe {
        if let Some(notify) = NOTIFY_SELECTION.get() {
            // An empty text is rejected too, so a document that is empty (or a
            // window that collapsed) only gets the cursor update below.
            if length > 0 {
                let code = notify(
                    proxy,
                    window.as_mut_ptr(),
                    length,
                    caret_in_window as i32,
                    caret_in_window as i32,
                );
                accepted = code == 0;
                if !accepted {
                    log_notify_failure(code, length, caret_in_window, start);
                }
            }
        }
        if let (Some(create), Some(notify)) = (CURSOR_CREATE.get(), NOTIFY_CURSOR.get()) {
            let info = create(cursor.0, cursor.1, cursor.2, cursor.3);
            if !info.is_null() {
                notify(proxy, info);
            }
        }
    }
    // Recorded only once the input method took the notification: committing it
    // first meant a rejection was never retried and the editor's idea of what
    // the input method knows stayed wrong for good.
    if length == 0 || accepted {
        let mut context = IME_CONTEXT.lock().unwrap();
        context.0 = text.to_string();
        context.1 = caret;
        context.2 = cursor;
    }
}

/// Report a rejected notification without letting a repeating failure flood the
/// log, which is what an unchanged refusal on every frame would do.
fn log_notify_failure(code: i32, length: usize, caret: usize, window_start: usize) {
    static LAST_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    if LAST_CODE.swap(code, Ordering::Relaxed) == code {
        return;
    }
    crate::log_line(&format!(
        "input method rejected the context update: code={code} length={length} caret={caret} window_start={window_start}"
    ));
}

pub fn commit_text(text: &str) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::Commit(text.to_string()));
}

pub fn delete_backward(length: usize) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::Backspace(length.max(1)));
}

pub fn delete_forward(length: usize) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::DeleteForward(length.max(1)));
}

pub fn preview_text(text: &str) {
    QUEUE.lock().unwrap().push(ImeCommand::Preview {
        text: text.to_string(),
        start: -1,
        end: -1,
    });
}

pub fn finish_preview() {
    QUEUE.lock().unwrap().push(ImeCommand::ClearPreview);
}
