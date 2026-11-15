//! Build settings of the desktop program.
//!
//! Windows gives a program's main thread 1 MiB of stack unless the executable says otherwise;
//! macOS and Linux give 8 MiB. The renderer runs on the main thread and recurses deeply while it
//! composes and lays out a screen, so Ember v0.1.0 ended on Windows right after its window opened:
//! "thread 'main' has overflowed its stack" (exception 0xc00000fd, #880). The executable therefore
//! reserves [`STACK_BYTES`]. Reserved address space costs no memory until it is touched.
//!
//! Only binaries get the argument: the library is also built as a `cdylib` for Android, where the
//! host process owns its threads.

/// 16 MiB: twice the main-thread stack of macOS and Linux, where the same screens run.
const STACK_BYTES: u64 = 16 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "windows" {
        return;
    }
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
    } else {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_BYTES}");
    }
}
