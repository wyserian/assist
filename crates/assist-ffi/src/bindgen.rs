//! `cargo run -p assist-ffi --features bindgen --bin uniffi-bindgen -- generate ...`: writes
//! the Kotlin bindings for the built library.

fn main() {
    uniffi::uniffi_bindgen_main()
}
