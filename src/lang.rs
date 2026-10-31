//! Reading `.lng` translation files — our own simple format.
//!
//! Plain text, UTF-8 (a BOM is tolerated), `//` comments, a
//! `lang=0x<LANGID> <Name>` header plus optional companion-file keys
//! (`help=...`), then a flat `<prefix><id>=text` table in four namespaces:
//! bare ids for dialog controls, `t` tooltips, `m` menu captions, `q` prompts.
//! The ids are ours, assigned by the UI.
//!
//! Files are read from disk at runtime, so a translator can drop a file into
//! the languages directory and iterate on it without rebuilding.

use std::collections::HashMap;
use std::path::Path;

/// Which table a string belongs to. The prefix letter in the file selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// Bare numeric id: a dialog control id, or a code-only string.
    Control,
    /// `t` — toolbar and status tooltips, keyed by menu command id.
    Tooltip,
    /// `m` — menu captions, keyed by menu command id.
    Menu,
    /// `q` — prompts and questions.
    Prompt,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("{path} has no lang= header")]
    NoHeader { path: String },
}

#[derive(Clone, Debug, Default)]
pub struct Language {
    /// Windows LANGID from the `lang=0x..` header.
    pub id: u16,
    /// Human-readable name that follows the id on the same line.
    pub name: String,
    /// Companion files the header names (`tips`, `help`, `tutor`, ...).
    pub companions: HashMap<String, String>,
    strings: HashMap<(Namespace, u32), String>,
}

impl Language {
    pub fn load(path: &Path) -> Result<Language, Error> {
        let bytes = std::fs::read(path).map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })?;
        let lang = Language::parse(&bytes);
        if lang.name.is_empty() && lang.id == 0 {
            return Err(Error::NoHeader { path: path.display().to_string() });
        }
        Ok(lang)
    }

    /// Parse raw file bytes.
    ///
    /// Files are UTF-8. Bytes that are not (a file saved in a legacy
    /// codepage) are decoded lossily rather than rejected, so a wrong encoding
    /// costs a few mangled accents instead of the whole translation.
    pub fn parse(bytes: &[u8]) -> Language {
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let text = String::from_utf8_lossy(body);

        let mut lang = Language::default();
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() || line.trim_start().starts_with("//") {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();

            if key == "lang" {
                let mut parts = value.trim().splitn(2, char::is_whitespace);
                if let Some(id) = parts.next() {
                    let hex = id.trim_start_matches("0x").trim_start_matches("0X");
                    lang.id = u16::from_str_radix(hex, 16).unwrap_or(0);
                }
                lang.name = parts.next().unwrap_or("").trim().to_owned();
                continue;
            }

            match parse_key(key) {
                Some((ns, id)) => {
                    lang.strings.insert((ns, id), value.to_owned());
                }
                None => {
                    // A non-numeric key is a companion filename.
                    lang.companions.insert(key.to_owned(), value.trim().to_owned());
                }
            }
        }
        lang
    }

    pub fn get(&self, ns: Namespace, id: u32) -> Option<&str> {
        self.strings.get(&(ns, id)).map(String::as_str)
    }

    /// Caption for a dialog control.
    pub fn control(&self, id: u32) -> Option<&str> {
        self.get(Namespace::Control, id)
    }

    /// Caption for a menu command.
    pub fn menu(&self, command: u32) -> Option<&str> {
        self.get(Namespace::Menu, command)
    }

    /// Tooltip for a toolbar command.
    pub fn tooltip(&self, command: u32) -> Option<&str> {
        self.get(Namespace::Tooltip, command)
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    pub fn count(&self, ns: Namespace) -> usize {
        self.strings.keys().filter(|(n, _)| *n == ns).count()
    }
}

/// Split a key into its namespace and numeric id.
///
/// Returns `None` for a key that is not `<optional prefix><digits>`, which is
/// how header keys like `tips` are told apart from string ids.
fn parse_key(key: &str) -> Option<(Namespace, u32)> {
    let (ns, digits) = match key.as_bytes().first()? {
        b'0'..=b'9' => (Namespace::Control, key),
        b't' => (Namespace::Tooltip, &key[1..]),
        b'm' => (Namespace::Menu, &key[1..]),
        b'q' => (Namespace::Prompt, &key[1..]),
        _ => return None,
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((ns, digits.parse().ok()?))
}

/// List the `.lng` files in a directory, newest-looking first by name.
pub fn available(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lng")))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "lang=0x07 German\r\n\
        tips=tips_de.txt\r\n\
        help=tdm_de.html\r\n\
        \r\n\
        //This template should be used to translate\r\n\
        //to foreign languages.\r\n\
        \r\n\
        1281=Categories\r\n\
        1022=O&K\r\n\
        1080=&Cancel\r\n\
        1617=Address\r\n\
        t32790=Start/Resume downloading selected file(s)\r\n\
        t32794=Remove all completed files from the list\r\n\
        m32771=Add new download\r\n\
        q1=Are you sure?\r\n\
        20369=Q\r\n";

    #[test]
    fn reads_the_language_header() {
        let l = Language::parse(SAMPLE.as_bytes());
        assert_eq!(l.id, 0x07);
        assert_eq!(l.name, "German");
    }

    #[test]
    fn companion_files_are_kept_apart_from_strings() {
        let l = Language::parse(SAMPLE.as_bytes());
        assert_eq!(l.companions.get("tips").map(String::as_str), Some("tips_de.txt"));
        assert_eq!(l.companions.get("help").map(String::as_str), Some("tdm_de.html"));
        // A companion key must not land in the string table.
        assert!(!l.companions.contains_key("1281"));
    }

    #[test]
    fn each_namespace_is_keyed_separately() {
        let l = Language::parse(SAMPLE.as_bytes());
        assert_eq!(l.control(1281), Some("Categories"));
        assert_eq!(l.tooltip(32790), Some("Start/Resume downloading selected file(s)"));
        assert_eq!(l.menu(32771), Some("Add new download"));
        assert_eq!(l.get(Namespace::Prompt, 1), Some("Are you sure?"));
        // The same number in a different namespace must not collide.
        assert_eq!(l.control(32790), None);
        assert_eq!(l.menu(1281), None);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let l = Language::parse(SAMPLE.as_bytes());
        // 1281, 1022, 1080, 1617, 20369, t32790, t32794, m32771, q1
        assert_eq!(l.len(), 9, "comment or blank line leaked into the table");
        assert_eq!(l.count(Namespace::Control), 5);
        assert_eq!(l.count(Namespace::Tooltip), 2);
        assert_eq!(l.count(Namespace::Menu), 1);
        assert_eq!(l.count(Namespace::Prompt), 1);
    }

    #[test]
    fn mnemonics_are_preserved_verbatim() {
        // The caption must keep its '&' so the menu renderer can strip it.
        let l = Language::parse(SAMPLE.as_bytes());
        assert_eq!(l.control(1022), Some("O&K"));
        assert_eq!(l.control(1080), Some("&Cancel"));
    }

    #[test]
    fn a_utf8_bom_is_stripped_before_parsing() {
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice("lang=0x0C French\r\n1281=Cat\u{e9}gories\r\n".as_bytes());
        let l = Language::parse(&bytes);
        assert_eq!(l.id, 0x0C);
        assert_eq!(l.name, "French");
        assert_eq!(l.control(1281), Some("Catégories"));
    }

    #[test]
    fn a_value_containing_an_equals_sign_survives() {
        let l = Language::parse(b"lang=0x09 English\r\n100=a=b=c\r\n");
        assert_eq!(l.control(100), Some("a=b=c"));
    }

    #[test]
    fn key_parsing_rejects_non_numeric_keys() {
        assert_eq!(parse_key("1281"), Some((Namespace::Control, 1281)));
        assert_eq!(parse_key("t32790"), Some((Namespace::Tooltip, 32790)));
        assert_eq!(parse_key("m1"), Some((Namespace::Menu, 1)));
        assert_eq!(parse_key("q2"), Some((Namespace::Prompt, 2)));
        // Header keys must not look like ids.
        assert_eq!(parse_key("tips"), None);
        assert_eq!(parse_key("tutor"), None);
        assert_eq!(parse_key("help"), None);
        assert_eq!(parse_key("t"), None);
        assert_eq!(parse_key(""), None);
    }
}
