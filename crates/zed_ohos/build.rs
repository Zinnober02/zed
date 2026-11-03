fn main() {
    // OHOS 的 musl libc 少几个依赖图里引用到的符号（ALSA、glibc 专有的 robust mutex、
    // 分配器与加密钩子）。启动路径不会调用它们，但加载器会急切解析重定位，所以 cdylib
    // 必须自己定义才能装载。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("ohos") {
        cc::Build::new()
            .file("src/musl_stubs.c")
            .compile("ohos_musl_stubs");
    }
    // 指纹只有一个来源：workflow 算好之后经 OHOS_RUST_FP 传进来。没有它就是手工编译的
    // 产物，不是 workflow 编的；留空让宿主那个判据当场认出来。这里不再自己算一遍 ——
    // 两边的算法只要有一处分叉（宿主还带 profile 后缀），库里的值与它算出的值就永远
    // 比不相等，于是每一轮都判定「装的库来自别的输入」。
    println!("cargo:rerun-if-env-changed=OHOS_RUST_FP");
    let fingerprint = std::env::var("OHOS_RUST_FP").unwrap_or_default();
    println!("cargo:rustc-env=OHOS_RUST_FP={fingerprint}");
}
