//! Installs Cisco's free OpenH264 library on this machine (software encoder fallback, and the
//! only H.264 encoder on platforms without a hardware one we support yet).
//!
//! Cisco's binary license requires that the end user's machine downloads it from Cisco, so
//! carson never ships it. The file is verified against the official SHA-256 list before use.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use carson_encode::openh264_cisco as cisco;

/// Largest file we accept (the real ones are ~1 MB).
const MAX_DOWNLOAD: usize = 16 << 20;

/// Ensures the library is installed; returns its path, or `None` if Cisco has no build for
/// this platform.
pub async fn ensure_openh264() -> Result<Option<PathBuf>> {
    if let Some(path) = cisco::installed_path() {
        return Ok(Some(path));
    }
    let (Some(url), Some(name), Some(dir)) = (
        cisco::download_url(),
        cisco::library_name(),
        cisco::install_dir(),
    ) else {
        return Ok(None);
    };
    println!("Downloading the OpenH264 video codec from Cisco ({url})…");
    println!("  {}", cisco::CREDIT);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let resp = http
        .get(&url)
        .send()
        .await
        .context("download OpenH264")?
        .error_for_status()?;
    let compressed = resp.bytes().await.context("download OpenH264")?;
    if compressed.len() > MAX_DOWNLOAD {
        bail!(
            "OpenH264 download is unexpectedly large ({} bytes)",
            compressed.len()
        );
    }
    let library = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut out = Vec::new();
        bzip2::read::BzDecoder::new(&compressed[..])
            .take(MAX_DOWNLOAD as u64)
            .read_to_end(&mut out)
            .context("decompress OpenH264")?;
        Ok(out)
    })
    .await??;

    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = dir.join(format!("{name}.download"));
    std::fs::write(&tmp, &library)?;
    if let Err(e) = cisco::verify(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("refusing to install the downloaded file"));
    }
    let path = dir.join(name);
    std::fs::rename(&tmp, &path)?;
    println!("  installed {} (verified)", path.display());
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    /// Downloads Cisco's library for real and encodes with it.
    /// `CARSON_OPENH264_DIR=/tmp/x cargo test -p carson-agent -- --ignored --nocapture cisco`
    #[tokio::test]
    #[ignore]
    async fn cisco_download_verifies_and_encodes() {
        assert!(std::env::var_os("CARSON_OPENH264_DIR").is_some(), "set CARSON_OPENH264_DIR to a scratch dir");
        let path = super::ensure_openh264().await.unwrap().expect("platform supported");
        let api = openh264::OpenH264API::from_blob_path(&path).expect("hash-verified load");
        let mut enc = openh264::encoder::Encoder::with_api_config(api, openh264::encoder::EncoderConfig::new()).unwrap();
        let yuv = openh264::formats::YUVBuffer::new(320, 240);
        let out = enc.encode(&yuv).unwrap().to_vec();
        assert!(!out.is_empty(), "Cisco's encoder produced a bitstream");
        eprintln!("cisco OpenH264 OK: {} ({} bytes out)", path.display(), out.len());
    }
}
