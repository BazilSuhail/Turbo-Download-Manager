//! File checksums, for verifying a finished download against a published hash.
//!
//! A file is read once and fed to both SHA-256 and MD5. Publishers quote either
//! one, and the expected hash's length says which, so the dialog does not need
//! to know the algorithm before the user pastes it.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Lowercase hex digests of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digests {
    pub sha256: String,
    pub md5: String,
}

/// Hash a file with SHA-256 and MD5 in a single read. Blocking: call it off the
/// UI thread.
pub fn digests_file(path: &Path) -> std::io::Result<Digests> {
    let mut file = std::fs::File::open(path)?;
    let mut sha = Sha256::new();
    let mut md = md5::Context::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md.consume(&buf[..n]);
    }
    Ok(Digests {
        sha256: hex(&sha.finalize()),
        md5: format!("{:x}", md.compute()),
    })
}

/// Lowercase hex SHA-256 of a file. Blocking: call it off the UI thread.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    Ok(digests_file(path)?.sha256)
}

/// True when `expected` (any case, surrounding whitespace ignored) names this
/// SHA-256 digest.
pub fn matches(digest: &str, expected: &str) -> bool {
    digest.eq_ignore_ascii_case(expected.trim())
}

/// True when `expected` names one of the file's digests. A 64-character hex
/// string is compared as SHA-256 and a 32-character one as MD5. Anything else
/// never matches, so a typo cannot pass for a good file.
pub fn matches_any(digests: &Digests, expected: &str) -> bool {
    let expected = expected.trim();
    match expected.len() {
        64 => digests.sha256.eq_ignore_ascii_case(expected),
        32 => digests.md5.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str, contents: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("tdm-sum-{tag}-{}", std::process::id()));
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn known_vector() {
        let p = temp_file("sha", b"abc");
        let d = sha256_file(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(matches(&d, "  BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD\n"));
    }

    #[test]
    fn md5_vector_and_length_routing() {
        let p = temp_file("md5", b"abc");
        let d = digests_file(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(d.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert!(matches_any(&d, "900150983cd24fb0d6963f7d28e17f72"));
        assert!(matches_any(&d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert!(!matches_any(&d, "900150983cd24fb0d6963f7d28e17f73"));
        assert!(!matches_any(&d, "not a hash"));
    }
}
