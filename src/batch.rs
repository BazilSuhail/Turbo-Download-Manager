//! Batch links: expand `https://x/img[001-120].jpg` and `[a-z]` patterns.
//!
//! A bracket expands only when its content is a numeric range (`[1-9]`,
//! zero-padded `[001-120]`) or a single-letter range (`[a-z]`). Anything else
//! — an IPv6 host such as `[::1]`, say — is kept literally, so a pasted plain
//! URL is never mangled. Several groups multiply, last group fastest.

/// Upper bound on one expansion. Ten thousand queued rows is already more
/// than anyone reviews; past that a typo like `[1-99999999]` should fail
/// loudly instead of freezing the window.
pub const MAX_EXPANSION: usize = 10_000;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0} links is more than the limit of {MAX_EXPANSION}")]
    TooMany(u128),
    #[error("range [{0}] runs backwards")]
    Reversed(String),
}

enum Part<'a> {
    Lit(&'a str),
    Num { start: u64, count: u64, width: usize },
    Alpha { start: u8, count: u64 },
}

impl Part<'_> {
    fn count(&self) -> u64 {
        match self {
            Part::Lit(_) => 1,
            Part::Num { count, .. } | Part::Alpha { count, .. } => *count,
        }
    }

    fn push(&self, i: u64, out: &mut String) {
        match self {
            Part::Lit(s) => out.push_str(s),
            Part::Num { start, width, .. } => out.push_str(&format!("{:0width$}", start + i)),
            Part::Alpha { start, .. } => out.push((start + i as u8) as char),
        }
    }
}

/// Every link in `text`, one pattern per line. Blank lines are skipped.
pub fn expand_all(text: &str) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        out.extend(expand(line)?);
        if out.len() > MAX_EXPANSION {
            return Err(Error::TooMany(out.len() as u128));
        }
    }
    Ok(out)
}

pub fn expand(pattern: &str) -> Result<Vec<String>, Error> {
    let parts = parse(pattern)?;
    // u128 so the product of a few large ranges cannot wrap below the limit.
    let total = parts.iter().map(|p| p.count() as u128).product::<u128>();
    if total > MAX_EXPANSION as u128 {
        return Err(Error::TooMany(total));
    }
    let mut out = Vec::with_capacity(total as usize);
    for n in 0..total as u64 {
        // Mixed-radix digits of `n`, least significant = last group.
        let mut digits = vec![0u64; parts.len()];
        let mut rest = n;
        for (d, p) in digits.iter_mut().zip(&parts).rev() {
            *d = rest % p.count();
            rest /= p.count();
        }
        let mut s = String::with_capacity(pattern.len() + 8);
        for (p, d) in parts.iter().zip(digits) {
            p.push(d, &mut s);
        }
        out.push(s);
    }
    Ok(out)
}

fn parse(pattern: &str) -> Result<Vec<Part<'_>>, Error> {
    let mut parts = Vec::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('[') {
        let Some(len) = rest[open..].find(']') else { break };
        let inner = &rest[open + 1..open + len];
        match range(inner)? {
            Some(part) => {
                parts.push(Part::Lit(&rest[..open]));
                parts.push(part);
            }
            None => parts.push(Part::Lit(&rest[..=open + len])),
        }
        rest = &rest[open + len + 1..];
    }
    parts.push(Part::Lit(rest));
    Ok(parts)
}

fn range(inner: &str) -> Result<Option<Part<'static>>, Error> {
    let Some((a, b)) = inner.split_once('-') else { return Ok(None) };
    let reversed = || Error::Reversed(inner.to_owned());
    if is_digits(a) && is_digits(b) {
        let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) else { return Ok(None) };
        if start > end {
            return Err(reversed());
        }
        // A leading zero asks for padding to the width the user typed.
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 0 };
        return Ok(Some(Part::Num { start, count: (end - start).saturating_add(1), width }));
    }
    let (&[x], &[y]) = (a.as_bytes(), b.as_bytes()) else { return Ok(None) };
    let same_case = (x.is_ascii_lowercase() && y.is_ascii_lowercase())
        || (x.is_ascii_uppercase() && y.is_ascii_uppercase());
    if !same_case {
        return Ok(None);
    }
    if x > y {
        return Err(reversed());
    }
    Ok(Some(Part::Alpha { start: x, count: (y - x + 1) as u64 }))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_padded_numbers_keep_their_width() {
        let v = expand("https://x/img[001-120].jpg").unwrap();
        assert_eq!(v.len(), 120);
        assert_eq!(v[0], "https://x/img001.jpg");
        assert_eq!(v[9], "https://x/img010.jpg");
        assert_eq!(v[119], "https://x/img120.jpg");
    }

    #[test]
    fn unpadded_numbers_and_letters() {
        assert_eq!(expand("a[8-10]").unwrap(), ["a8", "a9", "a10"]);
        assert_eq!(expand("p[a-c].htm").unwrap(), ["pa.htm", "pb.htm", "pc.htm"]);
        assert_eq!(expand("[X-Z]").unwrap(), ["X", "Y", "Z"]);
    }

    #[test]
    fn groups_multiply_with_the_last_one_fastest() {
        assert_eq!(expand("/[1-2]/[a-b]").unwrap(), ["/1/a", "/1/b", "/2/a", "/2/b"]);
    }

    #[test]
    fn non_ranges_stay_literal() {
        for url in ["http://[::1]:8080/f.zip", "https://x/a[b]c", "https://x/[a-9]", "https://x/[a-Z]", "https://x/open[1-2"] {
            assert_eq!(expand(url).unwrap(), [url], "{url}");
        }
    }

    #[test]
    fn a_huge_or_reversed_range_is_an_error() {
        assert_eq!(expand("x[1-99999999]"), Err(Error::TooMany(99_999_999)));
        assert!(matches!(expand("[1-9999999999][1-9999999999][1-9999999999]"), Err(Error::TooMany(_))));
        assert_eq!(expand("x[9-1]"), Err(Error::Reversed("9-1".into())));
        assert_eq!(expand("x[z-a]"), Err(Error::Reversed("z-a".into())));
        assert_eq!(expand("[1-10000]").map(|v| v.len()), Ok(MAX_EXPANSION));
    }

    #[test]
    fn expand_all_reads_lines_and_caps_the_total() {
        let v = expand_all("  https://a/[1-2]\n\n https://b/c \r\n").unwrap();
        assert_eq!(v, ["https://a/1", "https://a/2", "https://b/c"]);
        assert!(matches!(expand_all("[1-6000]\n[1-6000]"), Err(Error::TooMany(_))));
    }
}
