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

        let options = options_create(false);
        super::vk::log(&format!(
            "[gpui_ohos] IME proxy={:p} options={:p}",
            proxy, options
        ));
        let mut proxy_out: *mut c_void = std::ptr::null_mut();
        let code = attach(proxy, options, &mut proxy_out);
        super::vk::log(&format!("[gpui_ohos] IME attach code={code}"));
        *attached = true;
    }
}

pub(crate) fn drain() -> Vec<ImeCommand> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}
