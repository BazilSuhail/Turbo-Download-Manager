//! Fitting text into a fixed number of characters.

/// Shortens `s` to at most `max` characters, eliding the middle with `…`.
///
/// Counts and slices by `char`, never by byte, so multi-byte text is safe.
/// When the input is longer than `max`, the output is exactly `max` characters.
/// For a path, the last segment (after `/` or `\`) is kept whole when it fits
/// in 60% of `max`, so the file name stays readable.
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    if max < 5 {
        let mut out: String = chars[..max - 1].iter().collect();
        out.push('…');
        return out;
    }

    let seg_start = chars
        .iter()
        .rposition(|&c| c == '/' || c == '\\')
        .map_or(0, |i| i + 1);
    let seg_len = n - seg_start;

    let (head_len, tail): (usize, Vec<char>) = if seg_len * 10 <= max * 6 {
        (max - 1 - seg_len, chars[seg_start..].to_vec())
    } else {
        let head = (max - 1) / 2;
        let tail_len = (max - 1) - head;
        (head, chars[n - tail_len..].to_vec())
    };

    let mut out: String = chars[..head_len].iter().collect();
    out.push('…');
    out.extend(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::middle_ellipsis;

    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(middle_ellipsis("file.zip", 24), "file.zip");
        assert_eq!(middle_ellipsis("", 5), "");
    }

    #[test]
    fn exact_length_is_unchanged() {
        let s = "abcdefghij";
        assert_eq!(middle_ellipsis(s, 10), s);
    }

    #[test]
    fn windows_path_keeps_drive_and_file_name() {
        let out = middle_ellipsis(r"C:\Users\x\Downloads\file.zip", 24);
        assert_eq!(out.chars().count(), 24);
        assert!(out.starts_with(r"C:\"));
        assert!(out.ends_with("file.zip"));
        assert!(out.contains('…'));
    }

    #[test]
    fn url_keeps_scheme_and_file_name() {
        let out = middle_ellipsis("https://example.com/a/b/c/report.pdf", 24);
        assert_eq!(out.chars().count(), 24);
        assert!(out.starts_with("https://"));
        assert!(out.ends_with("report.pdf"));
    }

    #[test]
    fn multibyte_text_does_not_panic_and_has_exact_length() {
        let s = "ünïcödé-ñame-äöü.txt";
        for max in 0..=s.chars().count() + 2 {
            let out = middle_ellipsis(s, max);
            if s.chars().count() > max {
                assert_eq!(out.chars().count(), max, "max = {max}");
            } else {
                assert_eq!(out, s);
            }
        }
    }

    #[test]
    fn tiny_limits_do_not_panic() {
        assert_eq!(middle_ellipsis("abcdef", 0), "");
        assert_eq!(middle_ellipsis("abcdef", 1), "…");
        let out = middle_ellipsis("abcdef", 3);
        assert_eq!(out, "ab…");
        assert_eq!(out.chars().count(), 3);
    }

    #[test]
    fn output_length_always_equals_max_for_long_input() {
        let inputs = [
            r"C:\Users\Bazil Suhail\Documents\Turbo-Download-Manager\archive.tar.gz",
            "https://example.com/a/b/c/some-very-long-file-name-that-keeps-going.pdf",
            "no_separators_at_all_in_this_very_long_string",
            "trailing/slash/",
        ];
        for s in inputs {
            for max in 5..=40 {
                if s.chars().count() > max {
                    assert_eq!(middle_ellipsis(s, max).chars().count(), max, "{s} @ {max}");
                }
            }
        }
    }
}
