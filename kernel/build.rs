use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set");
    let linker_script = PathBuf::from(manifest_dir).join("linker-x86_64.ld");
    let linker_path = linker_script
        .to_str()
        .expect("linker script path must be valid UTF-8");

    println!("cargo:rustc-link-arg=-T{}", linker_path);
    println!("cargo:rerun-if-changed={}", linker_path);
}
