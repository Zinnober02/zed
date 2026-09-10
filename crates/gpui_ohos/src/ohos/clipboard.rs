//! Clipboard access through the OHOS pasteboard/UDMF NDK.
//!
//! Writing needs no permission; reading requires ohos.permission.READ_PASTEBOARD
//! (an ACL permission), so read_text returns None when it is not granted.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

type PbCreate = unsafe extern "C" fn() -> *mut c_void;
type PbDestroy = unsafe extern "C" fn(*mut c_void);
type PbGetData = unsafe extern "C" fn(*mut c_void, *mut c_int) -> *mut c_void;
type PbSetData = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type DataCreate = unsafe extern "C" fn() -> *mut c_void;
type DataDestroy = unsafe extern "C" fn(*mut c_void);
type DataAddRecord = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type DataGetPrimaryPlainText = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type RecordCreate = unsafe extern "C" fn() -> *mut c_void;
type RecordDestroy = unsafe extern "C" fn(*mut c_void);
type RecordAddPlainText = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type UdsCreate = unsafe extern "C" fn() -> *mut c_void;
type UdsDestroy = unsafe extern "C" fn(*mut c_void);
type UdsSetContent = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type UdsGetContent = unsafe extern "C" fn(*mut c_void) -> *const c_char;

struct ClipboardFns {
    pb_create: PbCreate,
    pb_destroy: PbDestroy,
    pb_get_data: PbGetData,
    pb_set_data: PbSetData,
    data_create: DataCreate,
    data_destroy: DataDestroy,
    data_add_record: DataAddRecord,
    data_get_primary_plain_text: DataGetPrimaryPlainText,
    record_create: RecordCreate,
    record_destroy: RecordDestroy,
    record_add_plain_text: RecordAddPlainText,
    uds_create: UdsCreate,
    uds_destroy: UdsDestroy,
    uds_set_content: UdsSetContent,
    uds_get_content: UdsGetContent,
}

static FNS: OnceLock<Option<ClipboardFns>> = OnceLock::new();

fn symbol(handle: *mut c_void, name: &str) -> *mut c_void {
    let cname = CString::new(name).unwrap();
    unsafe { dlsym(handle, cname.as_ptr()) }
}

fn load() -> Option<ClipboardFns> {
    unsafe {
        let pasteboard = dlopen(CString::new("libpasteboard.so").unwrap().as_ptr(), RTLD_NOW);
        let udmf = dlopen(CString::new("libudmf.so").unwrap().as_ptr(), RTLD_NOW);
        if pasteboard.is_null() || udmf.is_null() {
            return None;
        }
        macro_rules! sym {
            ($handle:expr, $name:literal, $ty:ty) => {{
                let p = symbol($handle, $name);
                if p.is_null() {
                    return None;
                }
                std::mem::transmute::<*mut c_void, $ty>(p)
            }};
        }
        Some(ClipboardFns {
            pb_create: sym!(pasteboard, "OH_Pasteboard_Create", PbCreate),
            pb_destroy: sym!(pasteboard, "OH_Pasteboard_Destroy", PbDestroy),
            pb_get_data: sym!(pasteboard, "OH_Pasteboard_GetData", PbGetData),
            pb_set_data: sym!(pasteboard, "OH_Pasteboard_SetData", PbSetData),
            data_create: sym!(udmf, "OH_UdmfData_Create", DataCreate),
            data_destroy: sym!(udmf, "OH_UdmfData_Destroy", DataDestroy),
            data_add_record: sym!(udmf, "OH_UdmfData_AddRecord", DataAddRecord),
            data_get_primary_plain_text: sym!(
                udmf,
                "OH_UdmfData_GetPrimaryPlainText",
                DataGetPrimaryPlainText
            ),
            record_create: sym!(udmf, "OH_UdmfRecord_Create", RecordCreate),
            record_destroy: sym!(udmf, "OH_UdmfRecord_Destroy", RecordDestroy),
            record_add_plain_text: sym!(udmf, "OH_UdmfRecord_AddPlainText", RecordAddPlainText),
            uds_create: sym!(udmf, "OH_UdsPlainText_Create", UdsCreate),
            uds_destroy: sym!(udmf, "OH_UdsPlainText_Destroy", UdsDestroy),
            uds_set_content: sym!(udmf, "OH_UdsPlainText_SetContent", UdsSetContent),
            uds_get_content: sym!(udmf, "OH_UdsPlainText_GetContent", UdsGetContent),
        })
    }
}

fn fns() -> Option<&'static ClipboardFns> {
    FNS.get_or_init(|| load()).as_ref()
}

/// Read plain text from the system pasteboard. Requires the ACL permission
/// ohos.permission.READ_PASTEBOARD; returns None otherwise.
pub fn read_text() -> Option<String> {
    let f = fns()?;
    unsafe {
        let pasteboard = (f.pb_create)();
        if pasteboard.is_null() {
            return None;
        }
        let mut status: c_int = 0;
        let data = (f.pb_get_data)(pasteboard, &mut status);
        if data.is_null() {
            super::vk::log(&format!(
                "[gpui_ohos] pasteboard read failed status={status} (ACL?)"
            ));
            (f.pb_destroy)(pasteboard);
            return None;
        }
        let uds = (f.uds_create)();
        let mut result = None;
        if (f.data_get_primary_plain_text)(data, uds) == 0 {
            let content = (f.uds_get_content)(uds);
            if !content.is_null() {
                result = CStr::from_ptr(content).to_str().ok().map(str::to_string);
            }
        }
        (f.uds_destroy)(uds);
        (f.data_destroy)(data);
        (f.pb_destroy)(pasteboard);
        result
    }
}

/// Write plain text to the system pasteboard. No permission required.
pub fn write_text(text: &str) -> bool {
    let Some(f) = fns() else {
        return false;
    };
    let Ok(content) = CString::new(text) else {
        return false;
    };
    unsafe {
        let pasteboard = (f.pb_create)();
        let data = (f.data_create)();
        let record = (f.record_create)();
        let uds = (f.uds_create)();
        if pasteboard.is_null() || data.is_null() || record.is_null() || uds.is_null() {
            return false;
        }
        let ok = (f.uds_set_content)(uds, content.as_ptr()) == 0
            && (f.record_add_plain_text)(record, uds) == 0
            && (f.data_add_record)(data, record) == 0
            && (f.pb_set_data)(pasteboard, data) == 0;
        (f.uds_destroy)(uds);
        (f.record_destroy)(record);
        (f.data_destroy)(data);
        (f.pb_destroy)(pasteboard);
        ok
    }
}
