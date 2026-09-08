#![cfg(target_env = "ohos")]

mod ohos;

pub use gpui::*;
pub use ohos::{
    LogFn, current_platform, log_line, pointer_down, pointer_move, pointer_up, run_with_surface,
    scroll, surface_destroyed, surface_resized, tick,
};
