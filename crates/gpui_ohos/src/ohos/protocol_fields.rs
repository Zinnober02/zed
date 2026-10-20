// 由 gen-protocol.py 从 inkstone-docs/protocol/messages.json 生成，勿手改。
//
// 字段名只有这份文件里出现字面量；读写点一律引用 fields:: 的常量。

pub(crate) mod fields {
    /// pick_paths (op 12, 请求)
    pub const DIRECTORIES: &str = "directories";
    /// pick_new_path (op 13, 请求)
    pub const DIRECTORY: &str = "directory";
    /// pick_result (event 5, 事件)
    pub const FD: &str = "fd";
    /// pick_paths (op 12, 请求)
    pub const FILES: &str = "files";
    /// focus (event 2, 事件)
    pub const FOCUSED: &str = "focused";
    /// rename (query 111, 请求)
    pub const FROM: &str = "from";
    /// create_window (op 16, 请求)、window_rect (event 6, 事件)
    pub const HEIGHT: &str = "height";
    /// hover (event 9, 事件)
    pub const HOVERED: &str = "hovered";
    /// create_window (op 16, 请求)、focus (event 2, 事件)、window_status (event 3, 事件)、window_rect (event 6, 事件)、hover (event 9, 事件)
    pub const ID: &str = "id";
    /// list_dir (query 106, 应答)、remove (query 110, 请求)
    pub const IS_DIR: &str = "is_dir";
    /// pick_result (event 5, 事件)
    pub const ITEMS: &str = "items";
    /// create_window (op 16, 请求)、pick_result (event 5, 事件)
    pub const KIND: &str = "kind";
    /// create_window (op 16, 请求)、window_rect (event 6, 事件)
    pub const LEFT: &str = "left";
    /// pick_paths (op 12, 请求)
    pub const MULTIPLE: &str = "multiple";
    /// list_dir (query 106, 应答)、pick_result (event 5, 事件)
    pub const NAME: &str = "name";
    /// pick_result (event 5, 事件)
    pub const PATH: &str = "path";
    /// window_status (event 3, 事件)
    pub const STATUS: &str = "status";
    /// pick_new_path (op 13, 请求)
    pub const SUGGESTED_NAME: &str = "suggested_name";
    /// rename (query 111, 请求)
    pub const TO: &str = "to";
    /// pick_paths (op 12, 请求)、pick_new_path (op 13, 请求)、pick_result (event 5, 事件)
    pub const TOKEN: &str = "token";
    /// create_window (op 16, 请求)、window_rect (event 6, 事件)
    pub const TOP: &str = "top";
    /// list_dir (query 106, 应答)、remove (query 110, 请求)、pick_result (event 5, 事件)、restore_folder (event 7, 事件)
    pub const URI: &str = "uri";
    /// create_window (op 16, 请求)、window_rect (event 6, 事件)
    pub const WIDTH: &str = "width";
}
