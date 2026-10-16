//! Cisco's prebuilt OpenH264 library: free for everyone because Cisco pays the H.264 royalties
//! for its binaries. Terms (https://www.openh264.org/BINARY_LICENSE.txt): the end user's machine
//! downloads it directly from Cisco (we never redistribute it), and we credit
//! "OpenH264 Video Codec provided by Cisco Systems, Inc.".
//!
//! Integrity: Cisco serves over plain HTTP, so the `openh264` crate refuses to load any file
//! whose SHA-256 is not one of Cisco's official 2.6.0 builds.

use std::path::PathBuf;

pub const VERSION: &str = "2.6.0";
pub const CREDIT: &str = "OpenH264 Video Codec provided by Cisco Systems, Inc.";

/// Cisco's file name for this platform, if Cisco builds one.
pub fn library_name() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "libopenh264-2.6.0-mac-arm64.dylib",
        ("macos", "x86_64") => "libopenh264-2.6.0-mac-x64.dylib",
        ("windows", "x86_64") => "openh264-2.6.0-win64.dll",
        ("windows", "aarch64") => "openh264-2.6.0-win-arm64.dll",
        ("linux", "x86_64") => "libopenh264-2.6.0-linux64.8.so",
        ("linux", "aarch64") => "libopenh264-2.6.0-linux-arm64.8.so",
        _ => return None,
    })
}

/// Download URL (bzip2-compressed) on Cisco's server.
pub fn download_url() -> Option<String> {
    library_name().map(|n| format!("http://ciscobinary.openh264.org/{n}.bz2"))
}

/// Directory holding the downloaded library (`$CARSON_OPENH264_DIR` overrides).
pub fn install_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CARSON_OPENH264_DIR") {
        return Some(PathBuf::from(d));
    }
    dirs::data_local_dir().map(|d| d.join("carson").join("openh264"))
}

/// Path to the installed library, if present.
pub fn installed_path() -> Option<PathBuf> {
    let p = install_dir()?.join(library_name()?);
    p.is_file().then_some(p)
}

/// Validates a downloaded library (hash + loadability) before it is used.
pub fn verify(path: &std::path::Path) -> anyhow::Result<()> {
    openh264::OpenH264API::from_blob_path(path)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("not an official Cisco OpenH264 {VERSION} build: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_platform_has_a_cisco_build() {
        let url = download_url().expect("supported platform");
        assert!(url.starts_with("http://ciscobinary.openh264.org/") && url.ends_with(".bz2"));
    }

    #[test]
    fn rejects_files_that_are_not_cisco_builds() {
        let dir = std::env::temp_dir().join(format!("carson-oh264-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake.dylib");
        std::fs::write(&fake, b"definitely not openh264").unwrap();
        assert!(verify(&fake).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
