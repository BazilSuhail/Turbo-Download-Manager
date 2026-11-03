//! Server-announced hashes: read the expected digest from a probe response's
//! headers, and hash the downloaded bytes with the same algorithm.
//!
//! Header priority: `Digest` (RFC 3230), then `Content-MD5`, then `x-goog-hash`.
//! Within `Digest`, the first entry with a supported algorithm and a valid
//! digest length wins. A malformed entry or header is skipped, and the next
//! source is tried. `x-goog-hash` `crc32c=` is ignored.

use crc32fast::Hasher as Crc32Hasher;
use md5::Context as Md5Context;
use reqwest::header::HeaderMap;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

/// A digest algorithm the server may announce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Crc32,
}

impl HashAlgo {
    /// Length of the raw digest in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
            Self::Crc32 => 4,
        }
    }
}

/// A server-announced digest, as raw bytes (not hex or base64).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedHash {
    pub algo: HashAlgo,
    pub bytes: Vec<u8>,
}

/// The expected hash from the response headers, or `None` when no header
/// names a supported algorithm with a valid digest.
pub fn from_headers(headers: &HeaderMap) -> Option<ExpectedHash> {
    let digest = values(headers, "digest").find_map(parse_digest_header);
    digest
        .or_else(|| values(headers, "content-md5").find_map(|v| expected(HashAlgo::Md5, v)))
        .or_else(|| {
            values(headers, "x-goog-hash")
                .flat_map(|v| v.split(','))
                .find_map(|entry| {
                    let (name, b64) = entry.trim().split_once('=')?;
                    if name.trim().eq_ignore_ascii_case("md5") {
                        expected(HashAlgo::Md5, b64)
                    } else {
                        None
                    }
                })
        })
}

/// Header values that are valid ASCII/UTF-8 text. Others are skipped.
fn values<'a>(headers: &'a HeaderMap, name: &'static str) -> impl Iterator<Item = &'a str> + 'a {
    headers.get_all(name).iter().filter_map(|v| v.to_str().ok())
}

/// RFC 3230: `algo=base64`, comma-separated. `sha` means SHA-1.
fn parse_digest_header(value: &str) -> Option<ExpectedHash> {
    value.split(',').find_map(|entry| {
        let (name, b64) = entry.trim().split_once('=')?;
        let algo = match name.trim().to_ascii_lowercase().as_str() {
            "sha-256" => HashAlgo::Sha256,
            "sha-512" => HashAlgo::Sha512,
            "sha" => HashAlgo::Sha1,
            "md5" => HashAlgo::Md5,
            _ => return None,
        };
        expected(algo, b64)
    })
}

/// Decode base64 text as a digest of `algo`. The length must match exactly.
fn expected(algo: HashAlgo, b64: &str) -> Option<ExpectedHash> {
    let bytes = base64_decode(b64.trim())?;
    (bytes.len() == algo.digest_len()).then_some(ExpectedHash { algo, bytes })
}

/// Standard base64 alphabet. Padding is optional but must be well formed.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let body = match text.find('=') {
        Some(i) => {
            let pad = &text[i..];
            if text.len() % 4 != 0 || pad.len() > 2 || !pad.bytes().all(|b| b == b'=') {
                return None;
            }
            &text[..i]
        }
        None => text,
    };
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for c in body.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = ((acc << 6) | v) & 0xFF_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Incremental hasher for one `HashAlgo`.
pub enum Hasher {
    Md5(Md5Context),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
    Crc32(Crc32Hasher),
}

impl Hasher {
    pub fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Md5 => Self::Md5(Md5Context::new()),
            HashAlgo::Sha1 => Self::Sha1(Sha1::new()),
            HashAlgo::Sha256 => Self::Sha256(Sha256::new()),
            HashAlgo::Sha512 => Self::Sha512(Sha512::new()),
            HashAlgo::Crc32 => Self::Crc32(Crc32Hasher::new()),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.consume(data),
            Self::Sha1(h) => Digest::update(h, data),
            Self::Sha256(h) => Digest::update(h, data),
            Self::Sha512(h) => Digest::update(h, data),
            Self::Crc32(h) => h.update(data),
        }
    }

    /// Raw digest bytes. CRC-32 is 4 bytes, big-endian.
    pub fn finish(self) -> Vec<u8> {
        match self {
            Self::Md5(h) => h.compute().0.to_vec(),
            Self::Sha1(h) => Digest::finalize(h).to_vec(),
            Self::Sha256(h) => Digest::finalize(h).to_vec(),
            Self::Sha512(h) => Digest::finalize(h).to_vec(),
            Self::Crc32(h) => h.finalize().to_be_bytes().to_vec(),
        }
    }
}

/// True when `actual` (raw digest bytes) equals the expected digest.
pub fn matches(expected: &ExpectedHash, actual: &[u8]) -> bool {
    expected.bytes.as_slice() == actual
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";
    const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn hex_bytes(h: &str) -> Vec<u8> {
        (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Test-only encoder, so the expected base64 is derived from the hex vector.
    fn b64_encode(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn b64_of_hex(h: &str) -> String {
        b64_encode(&hex_bytes(h))
    }

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(reqwest::header::HeaderName::from_static(k), HeaderValue::from_str(v).unwrap());
        }
        m
    }

    fn hash_abc(algo: HashAlgo) -> Vec<u8> {
        let mut h = Hasher::new(algo);
        h.update(b"abc");
        h.finish()
    }

    #[test]
    fn digest_sha256_parses_and_matches_abc() {
        let m = headers(&[("digest", format!("sha-256={}", b64_of_hex(SHA256_ABC)))]);
        let e = from_headers(&m).expect("sha-256 digest");
        assert_eq!(e.algo, HashAlgo::Sha256);
        assert_eq!(e.bytes, hex_bytes(SHA256_ABC));
        assert!(matches(&e, &hash_abc(HashAlgo::Sha256)));
    }

    #[test]
    fn content_md5_parses_and_matches_abc() {
        let m = headers(&[("content-md5", b64_of_hex(MD5_ABC))]);
        let e = from_headers(&m).expect("content-md5");
        assert_eq!(e.algo, HashAlgo::Md5);
        assert_eq!(e.bytes, hex_bytes(MD5_ABC));
        assert!(matches(&e, &hash_abc(HashAlgo::Md5)));
    }

    #[test]
    fn digest_sha1_uses_sha_name() {
        let m = headers(&[("digest", format!("sha={}", b64_of_hex(SHA1_ABC)))]);
        let e = from_headers(&m).expect("sha digest");
        assert_eq!(e.algo, HashAlgo::Sha1);
        assert!(matches(&e, &hash_abc(HashAlgo::Sha1)));
    }

    #[test]
    fn base64_decoder_standard_vectors() {
        assert_eq!(base64_decode("YWJj"), Some(b"abc".to_vec()));
        assert_eq!(base64_decode("YWI="), Some(b"ab".to_vec()));
        assert_eq!(base64_decode("Y*Jj"), None);
        assert_eq!(base64_decode("YW=Jj"), None);
        assert_eq!(base64_decode("YWI"), Some(b"ab".to_vec()));
    }

    #[test]
    fn priority_digest_then_content_md5_then_x_goog_hash() {
        let other_md5 = "900150983cd24fb0d6963f7d28e17f73";
        // Digest beats Content-MD5 even when Digest is listed second.
        let m = headers(&[
            ("content-md5", b64_of_hex(MD5_ABC)),
            ("digest", format!("md5={}", b64_of_hex(other_md5))),
        ]);
        assert_eq!(from_headers(&m).unwrap().bytes, hex_bytes(other_md5));

        // Unsupported Digest entries are skipped, so Content-MD5 wins.
        let m = headers(&[
            ("digest", "unixsum=1234".to_string()),
            ("content-md5", b64_of_hex(MD5_ABC)),
        ]);
        assert_eq!(from_headers(&m).unwrap().bytes, hex_bytes(MD5_ABC));

        // x-goog-hash: crc32c is ignored, md5 is used.
        let m = headers(&[(
            "x-goog-hash",
            format!("crc32c=AAAAAA==, md5={}", b64_of_hex(MD5_ABC)),
        )]);
        let e = from_headers(&m).expect("x-goog-hash md5");
        assert_eq!(e.algo, HashAlgo::Md5);
        assert_eq!(e.bytes, hex_bytes(MD5_ABC));

        // x-goog-hash with only crc32c yields nothing.
        let m = headers(&[("x-goog-hash", "crc32c=AAAAAA==".to_string())]);
        assert_eq!(from_headers(&m), None);
    }

    #[test]
    fn malformed_input_returns_none() {
        assert_eq!(from_headers(&HeaderMap::new()), None);
        let cases: Vec<HeaderMap> = vec![
            headers(&[("digest", "sha-256=!!!".to_string())]),
            headers(&[("digest", "sha-256=YWJj".to_string())]), // 3 bytes, not 32
            headers(&[("digest", "sha-256".to_string())]),       // no '='
            headers(&[("digest", "garbage".to_string())]),
            headers(&[("content-md5", "not base64!".to_string())]),
            headers(&[("content-md5", "".to_string())]),
            headers(&[("x-goog-hash", "md5".to_string())]),
            headers(&[("x-goog-hash", "md5=YWJj".to_string())]), // wrong length
        ];
        for m in &cases {
            assert_eq!(from_headers(m), None);
        }
        let mut bad = HeaderMap::new();
        bad.insert(
            reqwest::header::HeaderName::from_static("content-md5"),
            HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
        );
        assert_eq!(from_headers(&bad), None);
    }

    #[test]
    fn malformed_entry_does_not_hide_valid_one() {
        let m = headers(&[(
            "digest",
            format!("sha-256=!!!, md5={}", b64_of_hex(MD5_ABC)),
        )]);
        assert_eq!(from_headers(&m).unwrap().algo, HashAlgo::Md5);
    }

    #[test]
    fn crc32_of_check_string() {
        let mut h = Hasher::new(HashAlgo::Crc32);
        h.update(b"12345");
        h.update(b"6789");
        let out = h.finish();
        assert_eq!(out, vec![0xCB, 0xF4, 0x39, 0x26]);
        assert_eq!(u32::from_be_bytes(out.try_into().unwrap()), 0xCBF43926);
    }

    #[test]
    fn incremental_matches_one_shot() {
        for algo in [HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Sha512, HashAlgo::Crc32] {
            let mut h = Hasher::new(algo);
            h.update(b"a");
            h.update(b"bc");
            assert_eq!(h.finish(), hash_abc(algo), "{algo:?}");
            assert_eq!(hash_abc(algo).len(), algo.digest_len());
        }
    }

    #[test]
    fn known_vectors_for_abc() {
        assert_eq!(hash_abc(HashAlgo::Sha256), hex_bytes(SHA256_ABC));
        assert_eq!(hash_abc(HashAlgo::Md5), hex_bytes(MD5_ABC));
        assert_eq!(hash_abc(HashAlgo::Sha1), hex_bytes(SHA1_ABC));
    }

    #[test]
    fn matches_rejects_wrong_digest() {
        let e = ExpectedHash { algo: HashAlgo::Md5, bytes: hex_bytes(MD5_ABC) };
        assert!(matches(&e, &hex_bytes(MD5_ABC)));
        assert!(!matches(&e, &[0u8; 16]));
        assert!(!matches(&e, &hash_abc(HashAlgo::Sha1)[..16]));
    }
}
