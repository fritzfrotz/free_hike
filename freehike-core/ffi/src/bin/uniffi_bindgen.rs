//! Bindgen CLI for generating Swift/Kotlin bindings from the compiled library
//! (library-mode generation). Phase 1 usage:
//!
//! ```sh
//! cargo build -p ffi
//! cargo run -p ffi --features cli --bin uniffi-bindgen -- \
//!     generate --library target/debug/libfreehike_ffi.dylib \
//!     --language swift --language kotlin --out-dir bindings/
//! ```

fn main() {
    uniffi::uniffi_bindgen_main()
}
