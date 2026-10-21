use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 一个目录下的全部文件，递归。
fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

/// rust 阶段的输入指纹：只取决于这些文件的内容，与 git HEAD 无关。
///
/// 宿主用同一个定义算一遍，比对"库里的指纹"与"当前输入的指纹"，于是同一个工作树
/// 先后两次构建得到相同的值，而换一个提交只改文档时值也不变。
fn stage_fingerprint(root: &Path) -> String {
    let mut files = Vec::new();
    for directory in ["crates", "patches"] {
        walk(&root.join(directory), &mut files);
    }
    for single in ["Cargo.toml", "Cargo.lock", "script/cargo-ohos"] {
        let path = root.join(single);
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    for path in &files {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let digest = match std::fs::read(path) {
            Ok(bytes) => Sha256::digest(&bytes).to_vec(),
            Err(_) => Vec::new(),
        };
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        hasher.update(&digest);
        hasher.update([b'\n']);
        // 这些输入不在本包里，cargo 默认不会因为它们的变动重跑这个脚本。
        println!("cargo:rerun-if-changed={}", path.display());
    }
    format!("{:x}", Sha256::digest(hasher.finalize()))
}

fn main() {
    // OHOS's musl libc is missing a handful of symbols the dependency graph
    // references (ALSA, glibc-only robust mutexes, allocator/crypto hooks).
    // The startup path never calls them, but the loader resolves relocations
    // eagerly, so the cdylib has to define them to load at all.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("ohos") {
        cc::Build::new()
            .file("src/musl_stubs.c")
            .compile("ohos_musl_stubs");
    }
    // 指纹只留一个定义：workflow 算好之后经 OHOS_RUST_FP 传进来，构建脚本把它编成常量；
    // 直接跑 cargo（旧脚本、手工排查）时才用这里的兜底实现。
    println!("cargo:rerun-if-env-changed=OHOS_RUST_FP");
    let fingerprint = match std::env::var("OHOS_RUST_FP") {
        Ok(value) if !value.is_empty() => value,
        _ => match Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
        {
            Some(root) => stage_fingerprint(root),
            None => String::from("unknown"),
        },
    };
    println!("cargo:rustc-env=OHOS_RUST_FP={fingerprint}");
}
