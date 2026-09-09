fn main() {
    for (source, output) in [
        ("src/ohos/shaders.wgsl", "glyph.spv"),
        ("src/ohos/quad.wgsl", "quad.spv"),
        ("src/ohos/path.wgsl", "path.spv"),
    ] {
        println!("cargo:rerun-if-changed={source}");
        let source = std::fs::read_to_string(source).expect("read shader");
        let module = naga::front::wgsl::parse_str(&source).expect("parse wgsl");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validate wgsl");
        let words =
            naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
                .expect("emit spirv");
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
        std::fs::write(format!("{out_dir}/{output}"), bytes).expect("write spirv");
    }
}
