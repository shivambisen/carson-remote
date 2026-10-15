//! Adds the Swift runtime's back-compat libraries to the link search path.
//!
//! ScreenCaptureKit's Swift dependencies reference `swiftCompatibility*` static libs. Their
//! build scripts only look in full Xcode's toolchain layout; with just the Command Line
//! Tools installed the libs live elsewhere, so we add whichever location exists.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(out) = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
    else {
        return;
    };
    let dev = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    for candidate in [
        format!("{dev}/usr/lib/swift/macosx"),
        format!("{dev}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
    ] {
        if std::path::Path::new(&candidate)
            .join("libswiftCompatibility56.a")
            .exists()
        {
            println!("cargo:rustc-link-search=native={candidate}");
        }
    }
}
