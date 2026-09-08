#![cfg(target_env = "ohos")]

mod ohos;

pub use gpui::*;
pub use ohos::{
    LogFn, current_platform, key_event, log_line, pointer_down, pointer_move, pointer_up, root_dir,
    run_with_surface, scroll, set_root_dir, surface_destroyed, surface_resized, tick,
};
