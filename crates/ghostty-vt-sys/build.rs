// Links a prebuilt libghostty-vt; README.md says how to get one per platform.
// GHOSTTY_VT_LIB_DIR is the directory holding the library, else GHOSTTY_VT_DIR/lib.
fn main() {
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_LIB_DIR");
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");
    let dir = std::env::var("GHOSTTY_VT_LIB_DIR")
        .or_else(|_| std::env::var("GHOSTTY_VT_DIR").map(|d| format!("{d}/lib")));
    let Ok(dir) = dir else { return };
    println!("cargo:rustc-link-search=native={dir}");
    // Windows gets the DLL's import library, which upstream tests from MSVC.
    let kind = if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        "dylib"
    } else {
        "static"
    };
    println!("cargo:rustc-link-lib={kind}=ghostty-vt");
}
