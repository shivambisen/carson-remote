//! macOS: embed an Info.plist in the binary (`__TEXT,__info_plist`). macOS shows its name
//! and purpose strings in the Screen Recording / Accessibility prompts, and requires
//! NSScreenCaptureUsageDescription for screen capture in distributed builds.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.carson.agent</string>
  <key>CFBundleName</key><string>carson-agent</string>
  <key>CFBundleDisplayName</key><string>carson</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>NSScreenCaptureUsageDescription</key>
  <string>carson shares this screen with your own devices when you allow a connection.</string>
</dict>
</plist>
"#
    );
    let out =
        std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("Info.plist");
    std::fs::write(&out, plist).expect("write Info.plist");
    println!(
        "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        out.display()
    );
}
