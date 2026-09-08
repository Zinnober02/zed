//! OHOS input-method bridge (native C API).
//!
//! The IME drives a TextEditorProxy from a non-UI thread, so callbacks only
//! enqueue commands or read a cursor-context snapshot; OhosPlatform::tick
//! drains the queue on the UI thread and pushes text/selection updates back to
//! the input method, which is what makes it enter composing mode.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    Preview(String),
    ClearPreview,
    MoveCursor(i32),
}

static QUEUE: Mutex<Vec<ImeCommand>> = Mutex::new(Vec::new());
static ATTACHED: Mutex<bool> = Mutex::new(false);
/// (full text, caret index in UTF-16) mirrored from the focused editor.
static IME_CONTEXT: Mutex<(String, usize)> = Mutex::new((String::new(), 0));
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
    _start: i32,
    _end: i32,
) -> i32 {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::Preview(utf16_to_string(text, length)));
    0
}

unsafe extern "C" fn on_finish_preview(_proxy: *mut c_void) {
    QUEUE.lock().unwrap().push(ImeCommand::ClearPreview);
}

unsafe extern "C" fn on_get_text_config(_proxy: *mut c_void, config: *mut c_void) {
    if let Some(set) = SET_INPUT_TYPE.get() {
        // 1 = IME_TEXT_INPUT_TYPE_MULTILINE
        set(config, 1);
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
        let capacity = *length;
        let copied = slice.len().min(capacity);
        std::ptr::copy_nonoverlapping(slice.as_ptr(), text, copied);
        *length = copied;
    }
}

unsafe extern "C" fn on_get_left_text(
    _proxy: *mut c_void,
    number: i32,
    text: *mut u16,
    length: *mut usize,
) {
    write_text_slice(number, text, length, true);
}

unsafe extern "C" fn on_get_right_text(
    _proxy: *mut c_void,
    number: i32,
    text: *mut u16,
    length: *mut usize,
) {
    write_text_slice(number, text, length, false);
}

unsafe extern "C" fn on_enter_key(_proxy: *mut c_void, _kind: i32) {
    QUEUE.lock().unwrap().push(ImeCommand::Commit("\n".to_string()));
}

unsafe extern "C" fn on_move_cursor(_proxy: *mut c_void, direction: i32) {
    QUEUE.lock().unwrap().push(ImeCommand::MoveCursor(direction));
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
        type Attach =
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;
        type ShowTextInput = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
        type HideTextInput = unsafe extern "C" fn(*mut c_void) -> i32;
        type NotifySelection =
            unsafe extern "C" fn(*mut c_void, *mut u16, usize, i32, i32) -> i32;
        type NotifyCursor = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
        type CursorCreate = unsafe extern "C" fn(f64, f64, f64, f64) -> *mut c_void;

        let proxy_create: ProxyCreate = sym!("OH_TextEditorProxy_Create", ProxyCreate);
        let set_insert: ProxySet = sym!("OH_TextEditorProxy_SetInsertTextFunc", ProxySet);
        let set_backward: ProxySet =
            sym!("OH_TextEditorProxy_SetDeleteBackwardFunc", ProxySet);
        let set_forward: ProxySet = sym!("OH_TextEditorProxy_SetDeleteForwardFunc", ProxySet);
        let set_preview: ProxySet =
            sym!("OH_TextEditorProxy_SetSetPreviewTextFunc", ProxySet);
        let set_finish: ProxySet =
            sym!("OH_TextEditorProxy_SetFinishTextPreviewFunc", ProxySet);
        let set_config: ProxySet =
            sym!("OH_TextEditorProxy_SetGetTextConfigFunc", ProxySet);
        let set_status: ProxySet =
            sym!("OH_TextEditorProxy_SetSendKeyboardStatusFunc", ProxySet);
        let set_enter: ProxySet = sym!("OH_TextEditorProxy_SetSendEnterKeyFunc", ProxySet);
        let set_move: ProxySet = sym!("OH_TextEditorProxy_SetMoveCursorFunc", ProxySet);
        let set_selection: ProxySet =
            sym!("OH_TextEditorProxy_SetHandleSetSelectionFunc", ProxySet);
        let set_extend: ProxySet =
            sym!("OH_TextEditorProxy_SetHandleExtendActionFunc", ProxySet);
        let set_left: ProxySet =
            sym!("OH_TextEditorProxy_SetGetLeftTextOfCursorFunc", ProxySet);
        let set_right: ProxySet =
            sym!("OH_TextEditorProxy_SetGetRightTextOfCursorFunc", ProxySet);
        let set_index: ProxySet =
            sym!("OH_TextEditorProxy_SetGetTextIndexAtCursorFunc", ProxySet);
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
            CString::new("OH_InputMethodProxy_HideKeyboard").unwrap().as_ptr(),
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
        }
        let _ = on_noop as *const c_void;
        *attached = true;
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

pub fn show() {
    let code = try_show();
    if code == 0 {
        return;
    }
    // 12800009 IME_ERR_DETACHED: the system unbinds the client when the window
    // loses focus, and a detached client rejects ShowTextInput. Bind again.
    super::vk::log(&format!("[gpui_ohos] IME show failed code={code}; re-attaching"));
    *ATTACHED.lock().unwrap() = false;
    *IME_CONTEXT.lock().unwrap() = (String::new(), 0);
    attach();
    let code = try_show();
    super::vk::log(&format!("[gpui_ohos] IME show after re-attach code={code}"));
}

/// Leave the editing state. Pairs with show().
pub fn hide() {
    let proxy = PROXY.load(Ordering::Relaxed) as *mut c_void;
    if proxy.is_null() {
        return;
    }
    if let Some(hide) = HIDE_TEXT_INPUT.get() {
        let code = unsafe { hide(proxy) };
        super::vk::log(&format!("[gpui_ohos] IME hide code={code}"));
    }
}

pub(crate) fn drain() -> Vec<ImeCommand> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}

/// Mirror the focused editor's text and caret to the input method. The IME
/// needs these notifications to start composing.
pub(crate) fn update_context(text: &str, caret: usize) {
    {
        let mut context = IME_CONTEXT.lock().unwrap();
        if context.0 == text && context.1 == caret {
            return;
        }
        context.0 = text.to_string();
        context.1 = caret;
    }
    let proxy = PROXY.load(Ordering::Relaxed) as *mut c_void;
    if proxy.is_null() {
        return;
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        if let Some(notify) = NOTIFY_SELECTION.get() {
            notify(
                proxy,
                utf16.as_mut_ptr(),
                utf16.len(),
                caret as i32,
                caret as i32,
            );
        }
        if let (Some(create), Some(notify)) = (CURSOR_CREATE.get(), NOTIFY_CURSOR.get()) {
            let info = create(0.0, 0.0, 2.0, 20.0);
            if !info.is_null() {
                notify(proxy, info);
            }
        }
    }
}

pub fn commit_text(text: &str) {
    QUEUE.lock().unwrap().push(ImeCommand::Commit(text.to_string()));
}

pub fn delete_backward(length: usize) {
    QUEUE.lock().unwrap().push(ImeCommand::Backspace(length.max(1)));
}

pub fn delete_forward(length: usize) {
    QUEUE.lock().unwrap().push(ImeCommand::DeleteForward(length.max(1)));
}

pub fn preview_text(text: &str) {
    QUEUE.lock().unwrap().push(ImeCommand::Preview(text.to_string()));
}

pub fn finish_preview() {
    QUEUE.lock().unwrap().push(ImeCommand::ClearPreview);
}
