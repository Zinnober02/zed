#![cfg(target_env = "ohos")]

mod ohos;

pub use gpui::*;
pub use ohos::{
    HostOps, LogFn, current_platform, host_event, key_event, log_line, pointer_down, pointer_move,
    key_pre_ime, pointer_up, root_dir, run_with_surface, scroll, set_host_ops, set_root_dir,
    surface_created, surface_destroyed, surface_resized, tick, ime_commit_text, ime_delete_backward, ime_delete_forward,
    ime_finish_preview, ime_hide, ime_preview_text, ime_show, list_picked_dir, open_picked_file,
    pick_items, read_clipboard, read_picked_file, take_restore_folder, write_clipboard,
    write_picked_file,
};
