//! Downloading, checksum verification, and unzipping.
//!
//! Everything here is deliberately paranoid about HTTP status. During
//! development a `curl -sSL` without `-f` happily wrote a 9-byte file containing
//! the text `Not Found` and exited successfully; that file would then have been
//! cached as a "known-good" Gradle distribution. Every response is status-checked
//! before a single byte is trusted.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Download `url` to `dest`, streaming so a large distribution never has to fit
/// in memory. Returns the SHA-256 of what was written.
///
/// Redirects are followed (the distribution URL 307-redirects to a CDN).
pub fn download(url: &str, dest: &Path) -> Result<String> {
    log::debug!("GET {url}");

    let mut resp = ureq::get(url)
        .call()
        .with_context(|| format!("request failed: {url}"))?;

    let status = resp.status();
    if !status.is_success() {
        bail!("{url} returned HTTP {status}");
    }

    let file = File::create(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    let mut writer = BufWriter::new(file);
    let mut reader = resp.body_mut().as_reader();

    // Hash while writing rather than re-reading the file afterwards.
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("error reading response body from {url}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer
            .write_all(&buf[..n])
            .with_context(|| format!("cannot write {}", dest.display()))?;
        total += n as u64;
    }
    writer
        .flush()
        .with_context(|| format!("cannot flush {}", dest.display()))?;

    let digest = hex(&hasher.finalize());
    log::debug!("downloaded {total} bytes, sha256={digest}");
    Ok(digest)
}

/// Fetch a small text body (a `.sha256` file) and return it trimmed.
pub fn fetch_text(url: &str) -> Result<String> {
    log::debug!("GET {url}");

    let mut resp = ureq::get(url)
        .call()
        .with_context(|| format!("request failed: {url}"))?;

    let status = resp.status();
    if !status.is_success() {
        bail!("{url} returned HTTP {status}");
    }

    let body = resp
        .body_mut()
        .read_to_string()
        .with_context(|| format!("cannot read body of {url}"))?;
    Ok(body.trim().to_string())
}

/// SHA-256 a file on disk.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Compare two checksums case-insensitively, failing loudly on mismatch.
pub fn verify(what: &str, actual: &str, expected: &str) -> Result<()> {
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "checksum mismatch for {what}\n  expected: {expected}\n  actual:   {actual}\n\n\
             Refusing to continue."
        );
    }
    log::debug!("{what} checksum ok: {actual}");
    Ok(())
}

/// Unzip `archive` into `dest`.
pub fn unzip(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid zip", archive.display()))?;

    // `extract` rejects entries that would escape the destination.
    zip.extract(dest)
        .with_context(|| format!("cannot unzip {} into {}", archive.display(), dest.display()))?;

    log::debug!("unzipped {} into {}", archive.display(), dest.display());
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
    }

    #[test]
    fn sha256_of_known_input() {
        let dir = std::env::temp_dir().join("gradlew-test-sha256");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty");
        std::fs::write(&path, b"").unwrap();
        // Well-known SHA-256 of the empty string.
        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_is_case_insensitive_and_catches_mismatch() {
        assert!(verify("x", "ABCD", "abcd").is_ok());
        assert!(verify("x", "abcd", "dcba").is_err());
    }
}
