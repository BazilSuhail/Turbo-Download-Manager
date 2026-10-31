//! File checksums, for verifying a finished download against a published hash.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Lowercase hex SHA-256 of a file. Blocking: call it off the UI thread.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// True when `expected` (any case, surrounding whitespace ignored) names this digest.
pub fn matches(digest: &str, expected: &str) -> bool {
    digest.eq_ignore_ascii_case(expected.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        let p = std::env::temp_dir().join(format!("tdm-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let d = sha256_file(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(matches(&d, "  BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD\n"));
    }
}
