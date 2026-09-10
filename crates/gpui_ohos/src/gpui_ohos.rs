#![cfg(target_env = "ohos")]

mod ohos;

pub use gpui::*;
pub use ohos::{
    HostOps, LogFn, current_platform, host_event, ime_commit_text, ime_delete_backward,
    ime_delete_forward, ime_finish_preview, ime_hide, ime_preview_text, ime_show, key_event,
    key_pre_ime, list_picked_dir, list_picked_dir_async, log_line, open_picked_file,
    open_picked_file_async, pick_items, pointer_down, pointer_move, pointer_up, read_clipboard,
    read_picked_file, read_picked_file_bytes, read_picked_file_bytes_async, root_dir,
    run_app_on_surface, run_with_surface, scale, scroll, set_host_ops, set_root_dir,
    surface_created, surface_destroyed, surface_resized, take_restore_folder, tick,
    write_clipboard, write_picked_file, write_picked_file_bytes, write_picked_file_bytes_async,
};
