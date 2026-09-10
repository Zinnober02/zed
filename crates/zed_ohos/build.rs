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
}
