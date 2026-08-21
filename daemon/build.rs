use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-link-lib=va");
    println!("cargo:rustc-link-lib=va-drm");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("va[A-Z].*")
        .allowlist_type("VA.*")
        .allowlist_var("VA_.*")
        .allowlist_var("VAProfile.*")
        .allowlist_var("VAEntrypoint.*")
        .allowlist_var("VAConfigAttrib.*")
        .allowlist_var("VASurfaceAttrib.*")
        .allowlist_var("VAEncPackedHeader.*")
        .allowlist_var("VARateControl.*")
        .derive_default(true)
        .generate()
        .expect("failed to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("failed to write bindings");
}
