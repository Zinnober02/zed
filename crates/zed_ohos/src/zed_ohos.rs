#![cfg(target_env = "ohos")]

//! OHOS entry point for Zed.
//!
//! The ArkTS/C++ host creates an XComponent, then calls
//! ohos_gpui_app_main with its surface. The shared platform receives the
//! surface, Zed builds its application on it, and the host drives the frames.

mod host_abi;

use std::ffi::{CStr, c_char, c_void};
use std::path::Path;

use gpui_ohos::LogFn;

/// The interactive shell to hand to the terminal, as an absolute path.
///
/// /system/usr/bin/zsh is the shell the system terminal itself runs; /system/bin/sh
/// is toybox's POSIX shell and only serves as the fallback for a device that ships
/// no zsh.
fn system_shell() -> &'static str {
    if Path::new("/system/usr/bin/zsh").is_file() {
        "/system/usr/bin/zsh"
    } else {
        "/system/bin/sh"
    }
}

/// Variables this entry point sets for a reason, and which the user's shell must not
/// take back.
///
/// The editor's data, cache, state and temp directories stay in the sandbox: the
/// application cannot execute anything under the home directory until the user grants
/// that, and language servers have to be executable. The rest - PATH above all - belongs
/// to whoever configured the shell.
const OWNED_VARIABLES: [&str; 8] = [
    "HOME",
    "SHELL",
    "ZDOTDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "TMPDIR",
];

/// Report one line through the host's logger.
///
/// These lines are written before the editor installs its own sink, so the logger the
/// host passed in is the only one that reaches hilog.
fn host_log(log: LogFn, text: &str) {
    if let Ok(line) = std::ffi::CString::new(format!("{text}\n")) {
        log(line.as_ptr());
    }
}

/// How long the user's startup files may take before the editor gives up on them.
const SHELL_ENVIRONMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

/// rust 阶段输入内容的指纹，构建脚本算好交过来。
///
/// 宿主用它判断"装的库与当前源码是否一致"，条件只看这些文件的内容：与 git HEAD 无关，
/// 所以只改文档或只改宿主时，库与指纹仍然一致。
const RUST_FINGERPRINT: &str = match option_env!("OHOS_RUST_FP") {
    Some(value) => value,
    None => "unknown",
};

/// Take the environment the user's own shell produces.
///
/// An interactive shell sources the files they configured; running one and reading back
/// what it reports is how the editor learns the PATH and the variables those files build,
/// including whatever they installed under their home directory. A shell that hangs or
/// writes nothing usable leaves the environment as it was - the editor still runs, just
/// with the environment the platform gave it.
fn adopt_user_shell_environment(shell: &str, log: LogFn) {
    let started = std::time::Instant::now();
    // The marker separates the environment from anything the startup files print; the
    // answer is everything after the last one.
    const MARKER: &str = "__zed_env_below__";
    let mut command = std::process::Command::new(shell);
    command
        // 与上游的 login shell 语义对齐：-l 覆盖 zprofile/zlogin，-i 覆盖 zshrc。
        .arg("-l")
        .arg("-i")
        .arg("-c")
        .arg(format!("printf '{MARKER}\\n'; env"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            host_log(
                log,
                &format!("[zed_ohos] the shell {shell} would not start: {error}"),
            );
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        host_log(log, "[zed_ohos] the shell has no output pipe");
        let _ = child.kill();
        return;
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut reader = stdout;
        let mut buffer = String::new();
        let result = reader.read_to_string(&mut buffer).map(|_| buffer);
        let _ = sender.send(result);
    });
    let deadline = started + SHELL_ENVIRONMENT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                host_log(
                    log,
                    &format!(
                        "[zed_ohos] the shell did not finish in {}ms; keeping the environment the platform gave",
                        SHELL_ENVIRONMENT_TIMEOUT.as_millis()
                    ),
                );
                return;
            }
            Err(error) => {
                host_log(
                    log,
                    &format!("[zed_ohos] waiting for the shell failed: {error}"),
                );
                return;
            }
        }
    }
    let Ok(Ok(output)) = receiver.recv_timeout(std::time::Duration::from_millis(200)) else {
        host_log(log, "[zed_ohos] the shell produced no readable environment");
        return;
    };
    let Some(answer) = output.rsplit_once(MARKER).map(|(_, after)| after) else {
        host_log(log, "[zed_ohos] the shell printed no environment marker");
        return;
    };
    let mut adopted = 0;
    for line in answer.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.is_empty() || OWNED_VARIABLES.contains(&name) {
            continue;
        }
        // SAFETY: this runs before the editor starts any thread, so no other thread
        // reads the environment while it changes.
        unsafe { std::env::set_var(name, value) };
        adopted += 1;
    }
    let path = std::env::var("PATH").unwrap_or_default();
    host_log(
        log,
        &format!(
            "[zed_ohos] adopted {adopted} variable(s) from {shell} in {}ms, PATH={path}",
            started.elapsed().as_millis()
        ),
    );
}
/// Called by the host from OnSurfaceCreated.
#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_app_main(
    id: *const c_char,
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
) -> i32 {
    let id = if id.is_null() {
        String::new()
    } else {
        // SAFETY: the host passes a valid, NUL-terminated identifier.
        unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned()
    };
    host_log(
        log,
        &format!("[zed_ohos] rust fingerprint {RUST_FINGERPRINT}"),
    );
    // Zed resolves its data, config and log directories from HOME, which the
    // sandbox does not define; without this every directory creation is
    // denied and the app stops at its launch-failure dialog.
    // SAFETY: called before any other thread exists.
    unsafe {
        // HOME is the directory the user's own files live in: every shell and tool the
        // editor starts reads ~/.local/bin and friends out of it, so pointing it at the
        // sandbox left those installations invisible inside the terminal. The editor's
        // own data, cache and state move to explicit XDG paths below instead - they
        // must stay in the sandbox, where they can be executed and written.
        let sandbox = Path::new("/data/storage/el2/base/haps/entry/files");
        std::env::set_var("HOME", "/storage/Users/currentUser");
        // The same paths those defaults resolved to while HOME was the sandbox, so the
        // language servers already installed there keep working.
        std::env::set_var("XDG_DATA_HOME", sandbox.join(".local/share"));
        std::env::set_var("XDG_CACHE_HOME", sandbox.join(".cache"));
        std::env::set_var("XDG_STATE_HOME", sandbox.join(".local/state"));
        std::env::set_var("TMPDIR", sandbox.join(".cache/tmp"));
        for dir in [".local/share", ".cache", ".local/state", ".cache/tmp"] {
            let path = sandbox.join(dir);
            if let Err(error) = std::fs::create_dir_all(&path) {
                let message = format!("[zed_ohos] could not create {}: {error}\n", path.display());
                if let Ok(text) = std::ffi::CString::new(message) {
                    log(text.as_ptr());
                }
            }
        }
        // The terminal backend takes the shell and the user name from the
        // environment and falls back to the passwd database, which holds no
        // entry for an application uid (getpwuid_r answers "not found"), and
        // which names /bin/false as the shell of every account it does hold;
        // without a shell in the environment no terminal can be started.
        //
        // The device ships a full zsh at /system/usr/bin/zsh - it is what the system
        // terminal runs and carries the same sh_exec label as /system/bin/sh, while
        // that sh is toybox's stripped down POSIX shell that reads no rc file at all.
        std::env::set_var("SHELL", system_shell());
        // HOME stays inside the sandbox so zed keeps its own configuration there, and
        // the shell is told where to read the user's startup files instead: zsh looks
        // at ZDOTDIR before HOME, so the .zshrc they configured in the system terminal
        // applies to the terminal in this editor as well.
        let user_home = Path::new("/storage/Users/currentUser");
        if user_home.is_dir() {
            std::env::set_var("ZDOTDIR", user_home);
        }
        // Settings belong where the user can reach them: the configuration directory
        // moves to their home, so settings.json, keymap.json, tasks and themes are the
        // same files the terminal shows. Data, cache and state stay in the sandbox -
        // language servers install there, and nothing under the home directory can be
        // executed by this application.
        let user_config = user_home.join(".config");
        if user_config.is_dir() {
            std::env::set_var("XDG_CONFIG_HOME", user_config);
        }
        // Everything the editor spawns looks for its tools on PATH, and the one the
        // ability starts with carries no package manager: the git panel then asks for
        // "git" and gets ENOENT. The user's installs live in their home, which the
        // application may execute once that directory is granted.
        let user_bin = user_home.join(".harmonybrew/bin");
        if user_bin.is_dir() {
            let path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{path}", user_bin.display()));
        }
        // Everything else comes from the user's own shell, the way it does on the
        // platforms this editor was written for: no login shell exists here, so the
        // shell is named outright and told where the startup files are, and it reports
        // the environment those files produce. Child processes start needing it within a
        // second of launch - the git panel and the language servers look for their tools
        // on PATH - so the capture is bounded and runs before any of them.
        adopt_user_shell_environment(system_shell(), log);
        // This git reads no home configuration file, so the only way to tell it that
        // the repositories under the user's home are fine to touch is the environment,
        // which every child the editor spawns inherits.
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var("GIT_CONFIG_KEY_0", "safe.directory");
        std::env::set_var("GIT_CONFIG_VALUE_0", "*");
        // The backend reads the user name with env::var, which fails on a value
        // that is not valid UTF-8 as well as on an absent one.
        if std::env::var("USER").is_err() {
            std::env::set_var("USER", libc::getuid().to_string());
        }
    }

    install_filesystem_bridge();

    gpui_ohos::run_app_on_surface(&id, window, width, height, log, zed_app::run)
}

/// Let the filesystem wrapper reach picked documents through the host.
///
/// The picker hands back `file://docs` URIs and descriptors instead of paths the
/// sandbox can open, so `fs::OhosFs` forwards tagged paths to these entry
/// points and leaves every other path to the real filesystem.
fn install_filesystem_bridge() {
    // These hop to the UI thread internally, because the host bridge enters the
    // ArkUI JavaScript VM.
    fs::set_ohos_fs_bridge(fs::OhosFsBridge {
        list_dir: gpui_ohos::list_picked_dir_async,
        open_file: gpui_ohos::open_picked_file_async,
        read_fd: gpui_ohos::read_picked_file_bytes_async,
        close_fd: gpui_ohos::close_picked_file_async,
        write_fd: gpui_ohos::write_picked_file_bytes_async,
        create_dir: gpui_ohos::create_picked_dir_async,
        remove: gpui_ohos::remove_picked_path_async,
        rename: gpui_ohos::rename_picked_path_async,
        log: gpui_ohos::log_line,
    });
}
