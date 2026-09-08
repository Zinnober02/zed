//! OHOS input-method (IME) bridge.
//!
//! The IME C API drives a TextEditorProxy from a non-UI thread, so callbacks
//! only enqueue commands; OhosPlatform::tick drains them on the UI thread and
//! forwards them to the focused window's PlatformInputHandler.

use std::ffi::{c_char, c_int, c_void, CString};
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
}

static QUEUE: Mutex<Vec<ImeCommand>> = Mutex::new(Vec::new());
static ATTACHED: Mutex<bool> = Mutex::new(false);
static SET_INPUT_TYPE: OnceLock<unsafe extern "C" fn(*mut c_void, i32) -> i32> = OnceLock::new();

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

unsafe extern "C" fn on_delete_forward(_proxy: *mut c_void, length: i32) {
    QUEUE
        .lock()
        .unwrap()
        .push(ImeCommand::DeleteForward(length.max(1) as usize));
}

unsafe extern "C" fn on_keyboard_status(_proxy: *mut c_void, _status: i32) {}

unsafe extern "C" fn on_enter_key(_proxy: *mut c_void, _kind: i32) {
    QUEUE.lock().unwrap().push(ImeCommand::Commit("\n".to_string()));
}

unsafe extern "C" fn on_move_cursor(_proxy: *mut c_void, _direction: i32) {}

unsafe extern "C" fn on_set_selection(_proxy: *mut c_void, _start: i32, _end: i32) {}

unsafe extern "C" fn on_extend_action(_proxy: *mut c_void, _action: i32) {}

unsafe extern "C" fn on_get_left_text(
    _proxy: *mut c_void,
    _number: i32,
    _text: *mut u16,
    length: *mut usize,
) {
    if !length.is_null() {
        *length = 0;
    }
}

unsafe extern "C" fn on_get_right_text(
    _proxy: *mut c_void,
    _number: i32,
    _text: *mut u16,
    length: *mut usize,
) {
    if !length.is_null() {
        *length = 0;
    }
}

unsafe extern "C" fn on_text_index_at_cursor(_proxy: *mut c_void) -> i32 {
    0
}

unsafe extern "C" fn on_private_command(
    _proxy: *mut c_void,
    _command: *mut c_void,
    _size: usize,
) -> i32 {
    0
}

unsafe extern "C" fn on_get_text_config(_proxy: *mut c_void, config: *mut c_void) {
    if let Some(set) = SET_INPUT_TYPE.get() {
        // 1 = IME_TEXT_INPUT_TYPE_MULTILINE
        set(config, 1);
    }
}

unsafe extern "C" fn on_finish_preview(_proxy: *mut c_void) {
    QUEUE.lock().unwrap().push(ImeCommand::ClearPreview);
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
        type ProxySetInsert = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type ProxySetDelete = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type ProxySetPreview = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type ProxySetFinish = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type ProxySetTextConfig = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
        type SetInputType = unsafe extern "C" fn(*mut c_void, i32) -> i32;
        type OptionsCreate = unsafe extern "C" fn(bool) -> *mut c_void;
        type ControllerAttach =
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;

        let proxy_create: ProxyCreate =
            sym!("OH_TextEditorProxy_Create", ProxyCreate);
        let set_insert: ProxySetInsert =
            sym!("OH_TextEditorProxy_SetInsertTextFunc", ProxySetInsert);
        let set_delete: ProxySetDelete =
            sym!("OH_TextEditorProxy_SetDeleteBackwardFunc", ProxySetDelete);
        let set_preview: ProxySetPreview =
            sym!("OH_TextEditorProxy_SetSetPreviewTextFunc", ProxySetPreview);
        let set_finish: ProxySetFinish =
            sym!("OH_TextEditorProxy_SetFinishTextPreviewFunc", ProxySetFinish);
        let set_delete_forward: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetDeleteForwardFunc", ProxySetTextConfig);
        let set_keyboard_status: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetSendKeyboardStatusFunc", ProxySetTextConfig);
        let set_enter_key: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetSendEnterKeyFunc", ProxySetTextConfig);
        let set_move_cursor: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetMoveCursorFunc", ProxySetTextConfig);
        let set_set_selection: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetHandleSetSelectionFunc", ProxySetTextConfig);
        let set_extend_action: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetHandleExtendActionFunc", ProxySetTextConfig);
        let set_get_left: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetGetLeftTextOfCursorFunc", ProxySetTextConfig);
        let set_get_right: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetGetRightTextOfCursorFunc", ProxySetTextConfig);
        let set_text_index: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetGetTextIndexAtCursorFunc", ProxySetTextConfig);
        let set_private_command: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetReceivePrivateCommandFunc", ProxySetTextConfig);
        let set_text_config: ProxySetTextConfig =
            sym!("OH_TextEditorProxy_SetGetTextConfigFunc", ProxySetTextConfig);
        let set_input_type: SetInputType =
            sym!("OH_TextConfig_SetInputType", SetInputType);
        let _ = SET_INPUT_TYPE.set(set_input_type);
        let options_create: OptionsCreate = sym!("OH_AttachOptions_Create", OptionsCreate);
        let attach: ControllerAttach =
            sym!("OH_InputMethodController_Attach", ControllerAttach);

        let proxy = proxy_create();
        if proxy.is_null() {
            super::vk::log("[gpui_ohos] OH_TextEditorProxy_Create failed");
            return;
        }
        set_insert(proxy, on_insert as *const c_void);
        set_delete(proxy, on_delete_backward as *const c_void);
        set_preview(proxy, on_preview as *const c_void);
        set_finish(proxy, on_finish_preview as *const c_void);
        set_text_config(proxy, on_get_text_config as *const c_void);
        set_delete_forward(proxy, on_delete_forward as *const c_void);
        set_keyboard_status(proxy, on_keyboard_status as *const c_void);
        set_enter_key(proxy, on_enter_key as *const c_void);
        set_move_cursor(proxy, on_move_cursor as *const c_void);
        set_set_selection(proxy, on_set_selection as *const c_void);
        set_extend_action(proxy, on_extend_action as *const c_void);
        set_get_left(proxy, on_get_left_text as *const c_void);
        set_get_right(proxy, on_get_right_text as *const c_void);
        set_text_index(proxy, on_text_index_at_cursor as *const c_void);
        set_private_command(proxy, on_private_command as *const c_void);

        let options = options_create(false);
        super::vk::log(&format!(
            "[gpui_ohos] IME proxy={:p} options={:p}",
            proxy, options
        ));
        let mut proxy_out: *mut c_void = std::ptr::null_mut();
        let code = attach(proxy, options, &mut proxy_out);
        super::vk::log(&format!("[gpui_ohos] IME attach code={code}"));
        if code == 0 && !proxy_out.is_null() {
            type ShowTextInput = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
            let show: ShowTextInput =
                sym!("OH_InputMethodProxy_ShowTextInput", ShowTextInput);
            let show_code = show(proxy_out, options);
            super::vk::log(&format!("[gpui_ohos] IME showTextInput code={show_code}"));
        }
        *attached = true;
    }
}

pub(crate) fn drain() -> Vec<ImeCommand> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}
