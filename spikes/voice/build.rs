//! Points the linker at the Vosk import library in `models/`.

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models/vosk-win64-0.3.45");
    println!("cargo:rustc-link-search=native={}", dir.display());
}
