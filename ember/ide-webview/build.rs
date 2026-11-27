//! Links the Windows system libraries the WebView2 loader needs.
//!
//! The `webview2-com-sys` loader objects call registry and ETW functions from
//! advapi32 (`RegOpenKeyExW`, `EventRegister` and others) but do not ask for the
//! library themselves, so MSVC links fail with LNK2019 unless the binary names it
//! (#1165).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
