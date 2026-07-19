use std::io::Read;
use std::path::Path;

use cu::pre::*;
use sha2::{Digest, Sha256};

pub fn verify(left: &str, right: &str) -> cu::Result<()> {
    if left.trim().eq_ignore_ascii_case(right.trim()) {
        return Ok(());
    }
    cu::bail!("{left} != {right}")
}

/// SHA-256 a file on disk.
pub fn sha256_file(path: &Path) -> cu::Result<String> {
    let mut file = cu::fs::reader(path)?;
    let mut hasher = Sha256::new();
    // the wrapper is around 50KB so this will read it in one go
    // while also work for files that are much larger
    let mut buf = vec![0u8; 64 * 1024].into_boxed_slice();
    loop {
        let n = cu::check!(
            file.read(&mut buf),
            "sha256_file: error reading '{}'",
            path.display()
        )?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
    }

    #[test]
    fn sha256_of_known_input() -> cu::Result<()> {
        let dir = std::env::temp_dir().join("gradlew-test-sha256");
        cu::fs::make_dir_empty(&dir)?;
        let path = dir.join("empty");
        cu::fs::write(&path, b"")?;
        // Well-known SHA-256 of the empty string.
        assert_eq!(
            sha256_file(&path)?,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = cu::fs::rec_remove(&dir);
        Ok(())
    }
}
