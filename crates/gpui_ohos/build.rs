fn main() {
    println!("cargo:rerun-if-changed=src/ohos/shaders.wgsl");
    let source = std::fs::read_to_string("src/ohos/shaders.wgsl")
        .expect("read shaders.wgsl");
    let module = naga::front::wgsl::parse_str(&source).expect("parse wgsl");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("validate wgsl");
    let words = naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spirv");
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::write(format!("{out_dir}/glyph.spv"), bytes).expect("write glyph.spv");
}
