//! Real file type detection from magic bytes.
//!
//! `sniff` names the type of a file from its first and last bytes. `verdict` compares that type
//! with the file name, so a file called `*.png` that is really an HTML page can be flagged.
//! The `content_type` argument is accepted but does not affect the decision in v1.

use std::borrow::Cow;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes read from the start of a file for sniffing.
pub const HEAD: usize = 8192;
/// Bytes read from the end of a file for sniffing (the DMG trailer).
pub const TAIL: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Image,
    Executable,
    Archive,
    Document,
    Audio,
    Video,
    Font,
    WebPage,
    Text,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
    Tiff,
    Svg,
    Heic,
    Avif,
    Exe,
    Ole,
    Elf,
    AppImage,
    MachO,
    Dmg,
    Deb,
    Rpm,
    Apk,
    Jar,
    Zip,
    Ooxml,
    Odf,
    Epub,
    SevenZ,
    Rar,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Tar,
    Pdf,
    Mp4,
    Mov,
    M4a,
    Mkv,
    Webm,
    Mp3,
    Aac,
    Ogg,
    Wav,
    Avi,
    Flac,
    MpegTs,
    Ttf,
    Otf,
    Woff,
    Woff2,
    Sqlite,
    Wasm,
    Html,
    XmlError,
    Text,
    Unknown,
}

const ALL: &[Kind] = &[
    Kind::Png,
    Kind::Jpeg,
    Kind::Gif,
    Kind::Webp,
    Kind::Bmp,
    Kind::Ico,
    Kind::Tiff,
    Kind::Svg,
    Kind::Heic,
    Kind::Avif,
    Kind::Exe,
    Kind::Ole,
    Kind::Elf,
    Kind::AppImage,
    Kind::MachO,
    Kind::Dmg,
    Kind::Deb,
    Kind::Rpm,
    Kind::Apk,
    Kind::Jar,
    Kind::Zip,
    Kind::Ooxml,
    Kind::Odf,
    Kind::Epub,
    Kind::SevenZ,
    Kind::Rar,
    Kind::Gzip,
    Kind::Bzip2,
    Kind::Xz,
    Kind::Zstd,
    Kind::Tar,
    Kind::Pdf,
    Kind::Mp4,
    Kind::Mov,
    Kind::M4a,
    Kind::Mkv,
    Kind::Webm,
    Kind::Mp3,
    Kind::Aac,
    Kind::Ogg,
    Kind::Wav,
    Kind::Avi,
    Kind::Flac,
    Kind::MpegTs,
    Kind::Ttf,
    Kind::Otf,
    Kind::Woff,
    Kind::Woff2,
    Kind::Sqlite,
    Kind::Wasm,
    Kind::Html,
    Kind::XmlError,
    Kind::Text,
    Kind::Unknown,
];

/// Extensions the verdict logic knows about. Anything else is never flagged.
const KNOWN_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "jpe", "gif", "webp", "bmp", "ico", "cur", "tif", "tiff", "svg",
    "heic", "heif", "avif", "exe", "dll", "sys", "scr", "com", "cpl", "msi", "efi", "ocx", "drv",
    "doc", "xls", "ppt", "msg", "dot", "xlt", "pot", "so", "bin", "elf", "run", "o", "appimage",
    "dylib", "app", "dmg", "deb", "rpm", "apk", "zip", "docx", "xlsx", "pptx", "jar", "epub",
    "odt", "ods", "odp", "xpi", "ipa", "nupkg", "whl", "appx", "msix", "vsix", "docm", "xlsm",
    "pptm", "7z", "rar", "gz", "tgz", "gzip", "tar.gz", "bz2", "tbz2", "tbz", "xz", "txz", "zst",
    "tar", "pdf", "mp4", "m4v", "3gp", "m4a", "mov", "mkv", "webm", "mp3", "ogg", "oga", "ogv",
    "opus", "aac", "wav", "avi", "flac", "ts", "mts", "m2ts", "mpg", "mpeg", "tsv", "ttf", "ttc",
    "otf", "woff", "woff2", "sqlite", "db", "sqlite3", "wasm", "html", "htm", "xhtml", "php", "asp",
    "aspx", "jsp", "shtml", "mht", "mhtml", "txt", "csv", "json", "md", "js", "css", "xml", "dat",
    "tmp",
];

/// Extensions where HTML or XML content is expected, so it is not a mismatch.
const TEXT_MARKUP_EXTS: &[&str] = &[
    "html", "htm", "xhtml", "php", "asp", "aspx", "jsp", "shtml", "mht", "mhtml", "txt", "xml",
    "svg", "md", "json", "js", "css", "csv",
];

const HTML_EXTS: &[&str] = &[
    "html", "htm", "xhtml", "php", "asp", "aspx", "jsp", "shtml", "mht", "mhtml",
];

impl Kind {
    /// Short display name, for example "PNG" or "HTML page".
    pub fn label(self) -> &'static str {
        use Kind::*;
        match self {
            Png => "PNG",
            Jpeg => "JPEG",
            Gif => "GIF",
            Webp => "WebP",
            Bmp => "BMP",
            Ico => "ICO",
            Tiff => "TIFF",
            Svg => "SVG",
            Heic => "HEIC",
            Avif => "AVIF",
            Exe => "Windows executable",
            Ole => "OLE document",
            Elf => "ELF binary",
            AppImage => "AppImage",
            MachO => "Mach-O binary",
            Dmg => "Disk image (DMG)",
            Deb => "Debian package",
            Rpm => "RPM package",
            Apk => "Android APK",
            Jar => "Java archive",
            Zip => "ZIP",
            Ooxml => "Office document",
            Odf => "OpenDocument",
            Epub => "EPUB",
            SevenZ => "7z",
            Rar => "RAR",
            Gzip => "gzip",
            Bzip2 => "bzip2",
            Xz => "xz",
            Zstd => "Zstandard",
            Tar => "TAR",
            Pdf => "PDF",
            Mp4 => "MP4",
            Mov => "QuickTime",
            M4a => "M4A",
            Mkv => "Matroska",
            Webm => "WebM",
            Mp3 => "MP3",
            Aac => "AAC",
            Ogg => "Ogg",
            Wav => "WAV",
            Avi => "AVI",
            Flac => "FLAC",
            MpegTs => "MPEG-TS",
            Ttf => "TrueType font",
            Otf => "OpenType font",
            Woff => "WOFF",
            Woff2 => "WOFF2",
            Sqlite => "SQLite database",
            Wasm => "WebAssembly",
            Html => "HTML page",
            XmlError => "XML error",
            Text => "Text",
            Unknown => "Unknown",
        }
    }

    /// Canonical extension without the dot.
    pub fn ext(self) -> &'static str {
        use Kind::*;
        match self {
            Png => "png",
            Jpeg => "jpg",
            Gif => "gif",
            Webp => "webp",
            Bmp => "bmp",
            Ico => "ico",
            Tiff => "tif",
            Svg => "svg",
            Heic => "heic",
            Avif => "avif",
            Exe => "exe",
            Ole => "doc",
            Elf => "elf",
            AppImage => "appimage",
            MachO => "dylib",
            Dmg => "dmg",
            Deb => "deb",
            Rpm => "rpm",
            Apk => "apk",
            Jar => "jar",
            Zip => "zip",
            Ooxml => "docx",
            Odf => "odt",
            Epub => "epub",
            SevenZ => "7z",
            Rar => "rar",
            Gzip => "gz",
            Bzip2 => "bz2",
            Xz => "xz",
            Zstd => "zst",
            Tar => "tar",
            Pdf => "pdf",
            Mp4 => "mp4",
            Mov => "mov",
            M4a => "m4a",
            Mkv => "mkv",
            Webm => "webm",
            Mp3 => "mp3",
            Aac => "aac",
            Ogg => "ogg",
            Wav => "wav",
            Avi => "avi",
            Flac => "flac",
            MpegTs => "ts",
            Ttf => "ttf",
            Otf => "otf",
            Woff => "woff",
            Woff2 => "woff2",
            Sqlite => "sqlite",
            Wasm => "wasm",
            Html => "html",
            XmlError => "xml",
            Text => "txt",
            Unknown => "bin",
        }
    }

    pub fn category(self) -> Category {
        use Kind::*;
        match self {
            Png | Jpeg | Gif | Webp | Bmp | Ico | Tiff | Svg | Heic | Avif => Category::Image,
            Exe | Elf | AppImage | MachO | Dmg | Deb | Rpm | Apk | Jar | Wasm => {
                Category::Executable
            }
            Zip | SevenZ | Rar | Gzip | Bzip2 | Xz | Zstd | Tar => Category::Archive,
            Ole | Ooxml | Odf | Epub | Pdf => Category::Document,
            Mp3 | Aac | Ogg | Wav | Flac | M4a => Category::Audio,
            Mp4 | Mov | Mkv | Webm | Avi | MpegTs => Category::Video,
            Ttf | Otf | Woff | Woff2 => Category::Font,
            Html | XmlError => Category::WebPage,
            Text => Category::Text,
            Sqlite | Unknown => Category::Other,
        }
    }

    /// Stable storage id: the lowercase variant name.
    pub fn id(self) -> &'static str {
        use Kind::*;
        match self {
            Png => "png",
            Jpeg => "jpeg",
            Gif => "gif",
            Webp => "webp",
            Bmp => "bmp",
            Ico => "ico",
            Tiff => "tiff",
            Svg => "svg",
            Heic => "heic",
            Avif => "avif",
            Exe => "exe",
            Ole => "ole",
            Elf => "elf",
            AppImage => "appimage",
            MachO => "macho",
            Dmg => "dmg",
            Deb => "deb",
            Rpm => "rpm",
            Apk => "apk",
            Jar => "jar",
            Zip => "zip",
            Ooxml => "ooxml",
            Odf => "odf",
            Epub => "epub",
            SevenZ => "sevenz",
            Rar => "rar",
            Gzip => "gzip",
            Bzip2 => "bzip2",
            Xz => "xz",
            Zstd => "zstd",
            Tar => "tar",
            Pdf => "pdf",
            Mp4 => "mp4",
            Mov => "mov",
            M4a => "m4a",
            Mkv => "mkv",
            Webm => "webm",
            Mp3 => "mp3",
            Aac => "aac",
            Ogg => "ogg",
            Wav => "wav",
            Avi => "avi",
            Flac => "flac",
            MpegTs => "mpegts",
            Ttf => "ttf",
            Otf => "otf",
            Woff => "woff",
            Woff2 => "woff2",
            Sqlite => "sqlite",
            Wasm => "wasm",
            Html => "html",
            XmlError => "xmlerror",
            Text => "text",
            Unknown => "unknown",
        }
    }

    /// Inverse of `id`. None for unknown ids.
    pub fn from_id(id: &str) -> Option<Kind> {
        ALL.iter().copied().find(|k| k.id() == id)
    }

    /// True for unambiguous binary formats. Text-like and weak guesses are not strong.
    /// Whether TDM may offer to rename a file to `ext()`. Legacy Office files,
    /// OOXML and ODF share one signature, so their `ext()` is a guess: renaming
    /// `budget.xls` (really xlsx) to `.docx` would be wrong. Those are never renamed.
    pub fn renamable(self) -> bool {
        !matches!(self, Kind::Ole | Kind::Ooxml | Kind::Odf)
            && self.category() != Category::Executable
    }

    pub fn strong(self) -> bool {
        !matches!(
            self,
            Kind::Unknown
                | Kind::Text
                | Kind::Mp3
                | Kind::Aac
                | Kind::Html
                | Kind::XmlError
                | Kind::Svg
        )
    }

    /// Whether a file with this lowercase extension (no dot) may legitimately have this type.
    pub fn compatible(self, file_ext: &str) -> bool {
        use Kind::*;
        let has = |list: &[&str]| list.contains(&file_ext);
        match self {
            Png => has(&["png"]),
            Jpeg => has(&["jpg", "jpeg", "jfif", "jpe"]),
            Gif => has(&["gif"]),
            Webp => has(&["webp"]),
            Bmp => has(&["bmp"]),
            Ico => has(&["ico", "cur"]),
            Tiff => has(&["tif", "tiff"]),
            Svg => has(&["svg"]),
            Heic => has(&["heic", "heif"]),
            Avif => has(&["avif"]),
            Exe => has(&["exe", "dll", "sys", "scr", "com", "cpl", "msi", "efi", "ocx", "drv"]),
            Ole => has(&["doc", "xls", "ppt", "msi", "msg", "dot", "xlt", "pot"]),
            Elf => has(&["so", "bin", "elf", "run", "o"]),
            AppImage => has(&["appimage"]),
            MachO => has(&["dylib", "bin", "app", "o"]),
            Dmg => has(&["dmg"]),
            Deb => has(&["deb"]),
            Rpm => has(&["rpm"]),
            Apk => Kind::Zip.compatible(file_ext) || has(&["apk"]),
            Zip => has(&[
                "zip", "docx", "xlsx", "pptx", "jar", "apk", "epub", "odt", "ods", "odp", "xpi",
                "ipa", "nupkg", "whl", "appx", "msix", "vsix",
            ]),
            Ooxml => {
                Kind::Zip.compatible(file_ext)
                    || has(&["docx", "xlsx", "pptx", "docm", "xlsm", "pptm"])
            }
            Odf => Kind::Zip.compatible(file_ext) || has(&["odt", "ods", "odp"]),
            Epub => Kind::Zip.compatible(file_ext) || has(&["epub"]),
            Jar => Kind::Zip.compatible(file_ext) || has(&["jar"]),
            SevenZ => has(&["7z"]),
            Rar => has(&["rar"]),
            Gzip => has(&["gz", "tgz", "gzip", "tar.gz"]),
            Bzip2 => has(&["bz2", "tbz2", "tbz"]),
            Xz => has(&["xz", "txz"]),
            Zstd => has(&["zst"]),
            Tar => has(&["tar"]),
            Pdf => has(&["pdf"]),
            Mp4 => has(&["mp4", "m4v", "3gp", "m4a", "mov"]),
            Mov => has(&["mov", "mp4", "m4v"]),
            M4a => has(&["m4a", "mp4"]),
            Mkv => has(&["mkv"]),
            Webm => has(&["webm", "mkv"]),
            Mp3 => has(&["mp3"]),
            Aac => has(&["aac", "m4a"]),
            Ogg => has(&["ogg", "oga", "ogv", "opus"]),
            Wav => has(&["wav"]),
            Avi => has(&["avi"]),
            Flac => has(&["flac"]),
            MpegTs => has(&["ts", "mts", "m2ts", "mpg", "mpeg"]),
            Ttf => has(&["ttf", "ttc"]),
            Otf => has(&["otf"]),
            Woff => has(&["woff"]),
            Woff2 => has(&["woff2"]),
            Sqlite => has(&["sqlite", "db", "sqlite3"]),
            Wasm => has(&["wasm"]),
            Html => has(HTML_EXTS),
            XmlError => has(&["xml"]) || has(HTML_EXTS),
            Text | Unknown => true,
        }
    }
}

/// Identify the type of a file from its first bytes `head` and last bytes `tail`.
///
/// `head` is the first HEAD bytes (or the whole file if it is short). `tail` is the last TAIL
/// bytes, and may equal `head` for small files. Never panics.
pub fn sniff(head: &[u8], tail: &[u8]) -> Kind {
    if head.is_empty() {
        return Kind::Unknown;
    }
    binary_kind(head, tail).unwrap_or_else(|| text_kind(head))
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

fn find_ci(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

fn starts_ci(t: &[u8], p: &[u8]) -> bool {
    t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p)
}

/// Strong binary signatures. Returns None when no binary signature matches.
/// `Some(Unknown)` means a signature matched in a way that must not be labelled.
fn binary_kind(h: &[u8], tail: &[u8]) -> Option<Kind> {
    use Kind::*;

    if h.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Png);
    }
    if h.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Jpeg);
    }
    if h.starts_with(b"GIF87a") || h.starts_with(b"GIF89a") {
        return Some(Gif);
    }
    if h.starts_with(b"RIFF") {
        let tag = h.get(8..12).unwrap_or(&[]);
        if tag == b"WEBP" {
            return Some(Webp);
        }
        if tag == b"WAVE" {
            return Some(Wav);
        }
        if tag == b"AVI " {
            return Some(Avi);
        }
    }
    if h.starts_with(b"BM") && h.len() >= 18 {
        let dib = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        if matches!(dib, 12 | 40 | 52 | 56 | 108 | 124) {
            return Some(Bmp);
        }
    }
    if h.starts_with(&[0x00, 0x00, 0x01, 0x00]) || h.starts_with(&[0x00, 0x00, 0x02, 0x00]) {
        return Some(Ico);
    }
    if h.starts_with(b"II*\0") || h.starts_with(b"MM\0*") {
        return Some(Tiff);
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        let brand = &h[8..12];
        return Some(if brand == b"heic" || brand == b"heix" || brand == b"mif1" {
            Heic
        } else if brand == b"avif" || brand == b"avis" {
            Avif
        } else if brand == b"qt  " {
            Mov
        } else if brand == b"M4A " || brand == b"M4B " {
            M4a
        } else {
            Mp4
        });
    }
    if h.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let n = h.len().min(64);
        return Some(if find(&h[..n], b"webm") { Webm } else { Mkv });
    }
    // Only at offset 0. Searching deeper made text and HTML files that quote
    // "%PDF-" look like PDFs.
    if h.starts_with(b"%PDF-") {
        return Some(Pdf);
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") {
        return Some(zip_kind(h));
    }
    if h.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(SevenZ);
    }
    if h.starts_with(b"Rar!\x1A\x07\x00") || h.starts_with(b"Rar!\x1A\x07\x01\x00") {
        return Some(Rar);
    }
    if h.starts_with(&[0x1F, 0x8B, 0x08]) {
        return Some(Gzip);
    }
    if h.len() >= 4 && h.starts_with(b"BZh") && (b'1'..=b'9').contains(&h[3]) {
        return Some(Bzip2);
    }
    if h.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Some(Xz);
    }
    if h.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Zstd);
    }
    if h.len() >= 262 && &h[257..262] == b"ustar" {
        return Some(Tar);
    }
    if h.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Some(Ole);
    }
    if h.starts_with(b"MZ") {
        return Some(pe_kind(h));
    }
    if h.starts_with(&[0x7F, b'E', b'L', b'F']) {
        let appimage = h.get(8..11).map_or(false, |s| s == b"AI\x02");
        return Some(if appimage { AppImage } else { Elf });
    }
    if h.starts_with(&[0xFE, 0xED, 0xFA, 0xCE])
        || h.starts_with(&[0xFE, 0xED, 0xFA, 0xCF])
        || h.starts_with(&[0xCE, 0xFA, 0xED, 0xFE])
        || h.starts_with(&[0xCF, 0xFA, 0xED, 0xFE])
    {
        return Some(MachO);
    }
    if h.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]) {
        // Fat Mach-O stores a small architecture count; a Java class stores a class version.
        let count = h
            .get(4..8)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
        return Some(match count {
            Some(n) if n < 30 => MachO,
            _ => Unknown,
        });
    }
    // The UDIF trailer is the last 512 bytes of the file and starts with "koly".
    if tail.len() >= TAIL && tail[tail.len() - TAIL..].starts_with(b"koly") {
        return Some(Dmg);
    }
    if h.starts_with(b"!<arch>\ndebian-binary") {
        return Some(Deb);
    }
    if h.starts_with(&[0xED, 0xAB, 0xEE, 0xDB]) {
        return Some(Rpm);
    }
    if h.starts_with(&[0x00, 0x01, 0x00, 0x00]) || h.starts_with(b"ttcf") {
        return Some(Ttf);
    }
    if h.starts_with(b"OTTO") {
        return Some(Otf);
    }
    if h.starts_with(b"wOFF") {
        return Some(Woff);
    }
    if h.starts_with(b"wOF2") {
        return Some(Woff2);
    }
    if h.starts_with(b"SQLite format 3\0") {
        return Some(Sqlite);
    }
    if h.starts_with(b"\0asm") {
        return Some(Wasm);
    }
    if h.starts_with(b"OggS") {
        return Some(Ogg);
    }
    if h.starts_with(b"fLaC") {
        return Some(Flac);
    }
    if h.starts_with(b"ID3") {
        return Some(Mp3);
    }
    if h.starts_with(&[0xFF, 0xF1]) || h.starts_with(&[0xFF, 0xF9]) {
        return Some(Aac);
    }
    if mpeg_ts(h) {
        return Some(MpegTs);
    }
    None
}

/// PE needs a valid "PE\0\0" signature at the offset stored at 0x3C.
fn pe_kind(h: &[u8]) -> Kind {
    if h.len() >= 0x40 {
        let off = u32::from_le_bytes([h[0x3C], h[0x3D], h[0x3E], h[0x3F]]) as usize;
        if h.get(off..off.saturating_add(4)).map_or(false, |s| s == b"PE\0\0") {
            return Kind::Exe;
        }
    }
    Kind::Unknown
}

/// Refine a ZIP container by the first local file name, when it is inside head.
fn zip_kind(h: &[u8]) -> Kind {
    if h.len() < 30 {
        return Kind::Zip;
    }
    let name_len = u16::from_le_bytes([h[26], h[27]]) as usize;
    let name_end = 30 + name_len;
    let name = match h.get(30..name_end) {
        Some(n) => n,
        None => return Kind::Zip,
    };
    match name {
        b"[Content_Types].xml" => Kind::Ooxml,
        b"mimetype" => {
            if find(&h[name_end..], b"application/epub+zip") {
                Kind::Epub
            } else {
                Kind::Odf
            }
        }
        n if n.starts_with(b"META-INF/") => Kind::Jar,
        b"AndroidManifest.xml" | b"classes.dex" => Kind::Apk,
        _ => Kind::Zip,
    }
}

/// MPEG-TS sync byte 0x47 at 0 and 188, and at 376 when present. Needs at least two packets
/// in head, because one 0x47 byte is too weak to call a file MPEG-TS.
fn mpeg_ts(h: &[u8]) -> bool {
    if h.len() <= 188 || h[0] != 0x47 || h[188] != 0x47 {
        return false;
    }
    h.get(376).map_or(true, |&b| b == 0x47)
}

/// Text checks, run only when no binary signature matched.
fn text_kind(h: &[u8]) -> Kind {
    use Kind::*;

    // A UTF-8 BOM is dropped. A UTF-16 BOM is dropped and every 0x00 byte is removed, which
    // leaves ASCII-ish text.
    let body: Cow<[u8]> = if h.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Cow::Borrowed(&h[3..])
    } else if h.starts_with(&[0xFF, 0xFE]) || h.starts_with(&[0xFE, 0xFF]) {
        Cow::Owned(h[2..].iter().copied().filter(|&b| b != 0).collect())
    } else {
        Cow::Borrowed(h)
    };
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let t = &body[start..];

    if starts_ci(t, b"<!doctype html")
        || starts_ci(t, b"<html")
        || starts_ci(t, b"<head")
        || starts_ci(t, b"<body")
        || starts_ci(t, b"<script")
        || starts_ci(t, b"<meta")
        || starts_ci(t, b"<title")
    {
        return Html;
    }
    if starts_ci(t, b"<!--") && find_ci(&body[..body.len().min(1024)], b"<html") {
        return Html;
    }
    if starts_ci(t, b"<?xml") && find_ci(&body, b"<error") {
        return XmlError;
    }
    if starts_ci(t, b"<svg")
        || ((starts_ci(t, b"<?xml") || starts_ci(t, b"<!doctype svg")) && find_ci(&body, b"<svg"))
    {
        return Svg;
    }
    // A multi-byte UTF-8 character cut off at the end of head is accepted.
    let utf8_ok = match std::str::from_utf8(&body) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    };
    if !body.is_empty() && !body.contains(&0) && utf8_ok {
        return Text;
    }
    Unknown
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The name and the content agree, or the content is not checkable against the name.
    Match,
    /// The type cannot be judged from the name (no extension, plain text, weak signature, ...).
    Unchecked,
    /// The content is a binary type that the name does not allow.
    Mismatch,
    /// The content is an HTML or XML page where the name expects something else.
    WebPage,
}

/// Compare the sniffed kind with the file name.
///
/// `media` is true for media streams whose name is set by the app, so only a web page is
/// reported. `content_type` is accepted but not used in v1.
pub fn verdict(kind: Kind, file_name: &str, content_type: Option<&str>, media: bool) -> Verdict {
    let _ = content_type;
    let web = matches!(kind, Kind::Html | Kind::XmlError);
    if media {
        return if web { Verdict::WebPage } else { Verdict::Match };
    }
    let ext = match ext_of(file_name) {
        Some(e) => e,
        None => {
            return if web { Verdict::WebPage } else { Verdict::Unchecked };
        }
    };
    let ext = ext.as_str();
    if kind == Kind::Unknown {
        return Verdict::Unchecked;
    }
    if web {
        if TEXT_MARKUP_EXTS.contains(&ext) {
            return Verdict::Match;
        }
        // Generic names are never flagged, even when the content is a page.
        if KNOWN_EXTS.contains(&ext) && !matches!(ext, "bin" | "dat" | "tmp") {
            return Verdict::WebPage;
        }
    } else if matches!(
        ext,
        "txt" | "csv" | "tsv" | "json" | "md" | "js" | "css" | "xml" | "bin" | "dat" | "tmp"
    ) {
        // Text and generic names: binary signatures inside them are coincidences.
        return Verdict::Unchecked;
    }
    match kind {
        Kind::Text => return Verdict::Unchecked,
        Kind::Svg => {
            return if matches!(ext, "svg" | "xml" | "xhtml" | "html") {
                Verdict::Match
            } else {
                Verdict::Unchecked
            };
        }
        _ => {}
    }
    if !kind.strong() || !KNOWN_EXTS.contains(&ext) {
        return Verdict::Unchecked;
    }
    if kind.compatible(ext) {
        Verdict::Match
    } else {
        Verdict::Mismatch
    }
}

/// Lowercase extension after the last dot. None if there is no dot, the dot is first, or the
/// extension is empty.
fn ext_of(name: &str) -> Option<String> {
    let idx = name.rfind('.')?;
    if idx == 0 {
        return None;
    }
    let ext = &name[idx + 1..];
    if ext.is_empty() {
        None
    } else {
        Some(ext.to_ascii_lowercase())
    }
}

/// File name with its extension replaced by the kind's canonical extension.
/// A name whose extension already fits the kind is returned unchanged.
pub fn fixed_name(file_name: &str, kind: Kind) -> String {
    if let Some(ext) = ext_of(file_name) {
        if kind.compatible(&ext) {
            return file_name.to_string();
        }
    }
    match file_name.rfind('.') {
        Some(idx) if idx > 0 => format!("{}.{}", &file_name[..idx], kind.ext()),
        _ => format!("{}.{}", file_name, kind.ext()),
    }
}

/// Read the first HEAD bytes and, for longer files, the last TAIL bytes, then sniff. Blocking.
pub fn sniff_file(path: &Path) -> std::io::Result<Kind> {
    let mut file = File::open(path)?;
    let mut head = Vec::with_capacity(HEAD);
    (&mut file).take(HEAD as u64).read_to_end(&mut head)?;
    if head.len() < HEAD {
        return Ok(sniff(&head, &head));
    }
    let mut tail = vec![0u8; TAIL];
    file.seek(SeekFrom::End(-(TAIL as i64)))?;
    file.read_exact(&mut tail)?;
    Ok(sniff(&head, &tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sn(b: &[u8]) -> Kind {
        sniff(b, b)
    }

    fn with_len(mut v: Vec<u8>, n: usize) -> Vec<u8> {
        if v.len() < n {
            v.resize(n, 0);
        }
        v
    }

    /// A ZIP local file header whose first entry is `name`, followed by `rest`.
    fn zip_local(name: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut v = b"PK\x03\x04".to_vec();
        v.resize(26, 0);
        v.extend_from_slice(&(name.len() as u16).to_le_bytes());
        v.extend_from_slice(&[0, 0]);
        v.extend_from_slice(name);
        v.extend_from_slice(rest);
        v
    }

    fn pe_file(lfanew: u32, with_sig: bool) -> Vec<u8> {
        let mut v = vec![0u8; 0x100];
        v[0] = b'M';
        v[1] = b'Z';
        v[0x3C..0x40].copy_from_slice(&lfanew.to_le_bytes());
        if with_sig {
            v[0x80..0x84].copy_from_slice(b"PE\0\0");
        }
        v
    }

    fn ftyp(brand: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0x18];
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.resize(0x18, 0);
        v
    }

    fn tar_block() -> Vec<u8> {
        let mut v = vec![0u8; 262];
        v[257..262].copy_from_slice(b"ustar");
        v
    }

    #[test]
    fn png() {
        assert_eq!(sn(&with_len(b"\x89PNG\r\n\x1a\nxx".to_vec(), 32)), Kind::Png);
    }

    #[test]
    fn jpeg() {
        assert_eq!(sn(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]), Kind::Jpeg);
    }

    #[test]
    fn gif() {
        assert_eq!(sn(b"GIF89a\x01\x00"), Kind::Gif);
        assert_eq!(sn(b"GIF87a\x01\x00"), Kind::Gif);
    }

    #[test]
    fn webp() {
        assert_eq!(sn(b"RIFF\x24\x00\x00\x00WEBPVP8 "), Kind::Webp);
    }

    #[test]
    fn wav() {
        assert_eq!(sn(b"RIFF\x24\x00\x00\x00WAVEfmt "), Kind::Wav);
    }

    #[test]
    fn avi() {
        assert_eq!(sn(b"RIFF\x24\x00\x00\x00AVI LIST"), Kind::Avi);
    }

    #[test]
    fn bmp_valid_and_invalid_header() {
        let mut ok = b"BM".to_vec();
        ok.resize(14, 0);
        ok.extend_from_slice(&40u32.to_le_bytes());
        ok.resize(64, 0);
        assert_eq!(sn(&ok), Kind::Bmp);

        let mut bad = b"BM".to_vec();
        bad.resize(14, 0);
        bad.extend_from_slice(&41u32.to_le_bytes());
        bad.resize(64, 0);
        assert_eq!(sn(&bad), Kind::Unknown);
    }

    #[test]
    fn ico_and_cur() {
        assert_eq!(sn(&[0, 0, 1, 0, 1, 0, 16, 16]), Kind::Ico);
        assert_eq!(sn(&[0, 0, 2, 0, 1, 0, 16, 16]), Kind::Ico);
    }

    #[test]
    fn tiff() {
        assert_eq!(sn(b"II*\0\x08\0\0\0"), Kind::Tiff);
        assert_eq!(sn(b"MM\0*\0\0\0\x08"), Kind::Tiff);
    }

    #[test]
    fn zip_plain() {
        assert_eq!(sn(&zip_local(b"a.txt", b"data")), Kind::Zip);
    }

    #[test]
    fn docx_via_content_types() {
        assert_eq!(sn(&zip_local(b"[Content_Types].xml", b"")), Kind::Ooxml);
    }

    #[test]
    fn jar_via_meta_inf() {
        assert_eq!(sn(&zip_local(b"META-INF/MANIFEST.MF", b"")), Kind::Jar);
    }

    #[test]
    fn apk_via_manifest_and_dex() {
        assert_eq!(sn(&zip_local(b"AndroidManifest.xml", b"")), Kind::Apk);
        assert_eq!(sn(&zip_local(b"classes.dex", b"")), Kind::Apk);
    }

    #[test]
    fn epub_via_mimetype() {
        let v = zip_local(b"mimetype", b"application/epub+zip");
        assert_eq!(sn(&v), Kind::Epub);
    }

    #[test]
    fn odt_via_mimetype() {
        let v = zip_local(b"mimetype", b"application/vnd.oasis.opendocument.text");
        assert_eq!(sn(&v), Kind::Odf);
    }

    #[test]
    fn pe_valid() {
        assert_eq!(sn(&pe_file(0x80, true)), Kind::Exe);
    }

    #[test]
    fn mz_without_pe_is_unknown() {
        assert_eq!(sn(&pe_file(0x80, false)), Kind::Unknown);
        assert_eq!(sn(&pe_file(0xFFFF_FFF0, false)), Kind::Unknown);
    }

    #[test]
    fn elf_and_appimage() {
        let mut elf = b"\x7FELF\x02\x01\x01\x00".to_vec();
        elf.resize(64, 0);
        assert_eq!(sn(&elf), Kind::Elf);

        let mut app = b"\x7FELF\x02\x01\x01\x00".to_vec();
        app.extend_from_slice(b"AI\x02");
        app.resize(64, 0);
        assert_eq!(sn(&app), Kind::AppImage);
    }

    #[test]
    fn macho_thin() {
        assert_eq!(sn(&[0xCF, 0xFA, 0xED, 0xFE, 0, 0, 0, 0]), Kind::MachO);
        assert_eq!(sn(&[0xFE, 0xED, 0xFA, 0xCE, 0, 0, 0, 0]), Kind::MachO);
    }

    #[test]
    fn cafebabe_fat_vs_java() {
        assert_eq!(sn(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2]), Kind::MachO);
        assert_eq!(sn(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 0x34]), Kind::Unknown);
    }

    #[test]
    fn pdf_with_junk_before() {
        assert_ne!(sn(b"junk junk\n%PDF-1.7\nrest"), Kind::Pdf, "a quoted %PDF- is not a PDF");
        assert_eq!(sn(b"%PDF-1.7\nrest"), Kind::Pdf);
    }

    #[test]
    fn sevenz() {
        assert_eq!(sn(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 4]), Kind::SevenZ);
    }

    #[test]
    fn rar() {
        assert_eq!(sn(b"Rar!\x1A\x07\x00rest"), Kind::Rar);
        assert_eq!(sn(b"Rar!\x1A\x07\x01\x00rest"), Kind::Rar);
    }

    #[test]
    fn gzip() {
        assert_eq!(sn(&[0x1F, 0x8B, 0x08, 0, 0, 0]), Kind::Gzip);
    }

    #[test]
    fn bzip2() {
        assert_eq!(sn(b"BZh9\x31\x41"), Kind::Bzip2);
    }

    #[test]
    fn xz() {
        assert_eq!(sn(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0, 0]), Kind::Xz);
    }

    #[test]
    fn zstd() {
        assert_eq!(sn(&[0x28, 0xB5, 0x2F, 0xFD, 0, 0]), Kind::Zstd);
    }

    #[test]
    fn tar_at_257() {
        assert_eq!(sn(&tar_block()), Kind::Tar);
    }

    #[test]
    fn tar_shorter_than_262_is_not_tar() {
        let mut v = tar_block();
        v.truncate(261);
        assert_ne!(sn(&v), Kind::Tar);
    }

    #[test]
    fn ole() {
        assert_eq!(
            sn(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]),
            Kind::Ole
        );
    }

    #[test]
    fn mp4_isom() {
        assert_eq!(sn(&ftyp(b"isom")), Kind::Mp4);
    }

    #[test]
    fn heic_brand() {
        assert_eq!(sn(&ftyp(b"heic")), Kind::Heic);
    }

    #[test]
    fn avif_brand() {
        assert_eq!(sn(&ftyp(b"avif")), Kind::Avif);
    }

    #[test]
    fn mov_brand() {
        assert_eq!(sn(&ftyp(b"qt  ")), Kind::Mov);
    }

    #[test]
    fn mkv() {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x01, 0, 0, 0];
        v.extend_from_slice(b"matroska");
        assert_eq!(sn(&v), Kind::Mkv);
    }

    #[test]
    fn webm() {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01];
        v.extend_from_slice(b"webm");
        assert_eq!(sn(&v), Kind::Webm);
    }

    #[test]
    fn mp3_with_id3() {
        assert_eq!(sn(b"ID3\x03\x00\x00\x00\x00\x00\x00"), Kind::Mp3);
    }

    #[test]
    fn mp3_without_id3_is_unknown() {
        assert_eq!(sn(&[0xFF, 0xFB, 0x90, 0x00, 0, 0, 0, 0]), Kind::Unknown);
    }

    #[test]
    fn aac_adts() {
        assert_eq!(sn(&[0xFF, 0xF1, 0x50, 0x80, 0, 0]), Kind::Aac);
    }

    #[test]
    fn mpeg_ts() {
        let mut v = vec![0u8; 377];
        v[0] = 0x47;
        v[188] = 0x47;
        v[376] = 0x47;
        assert_eq!(sn(&v), Kind::MpegTs);
    }

    #[test]
    fn fonts() {
        assert_eq!(sn(&[0, 1, 0, 0, 0, 0, 0, 0]), Kind::Ttf);
        assert_eq!(sn(b"ttcf\0\0\0\0"), Kind::Ttf);
        assert_eq!(sn(b"OTTO\0\0\0\0"), Kind::Otf);
        assert_eq!(sn(b"wOFF\0\0\0\0"), Kind::Woff);
        assert_eq!(sn(b"wOF2\0\0\0\0"), Kind::Woff2);
    }

    #[test]
    fn sqlite() {
        assert_eq!(sn(b"SQLite format 3\0\x10\0"), Kind::Sqlite);
    }

    #[test]
    fn wasm() {
        assert_eq!(sn(b"\0asm\x01\0\0\0"), Kind::Wasm);
    }

    #[test]
    fn ogg() {
        assert_eq!(sn(b"OggS\0\x02\0\0"), Kind::Ogg);
    }

    #[test]
    fn flac() {
        assert_eq!(sn(b"fLaC\0\0\0\x22"), Kind::Flac);
    }

    #[test]
    fn html_with_bom() {
        assert_eq!(sn(b"\xEF\xBB\xBF<html><body>x</body></html>"), Kind::Html);
    }

    #[test]
    fn html_with_leading_whitespace() {
        assert_eq!(sn(b"  \n\t<HTML>x</HTML>"), Kind::Html);
    }

    #[test]
    fn doctype_html() {
        assert_eq!(sn(b"<!DOCTYPE html>\n<p>x</p>"), Kind::Html);
    }

    #[test]
    fn html_utf16_bom() {
        let mut v = vec![0xFF, 0xFE];
        for &c in b"<html>" {
            v.push(c);
            v.push(0);
        }
        assert_eq!(sn(&v), Kind::Html);
    }

    #[test]
    fn s3_xml_error() {
        let x = b"<?xml version=\"1.0\"?>\n<Error><Code>AccessDenied</Code></Error>";
        assert_eq!(sn(x), Kind::XmlError);
    }

    #[test]
    fn svg_with_xml_prolog() {
        let x = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(sn(x), Kind::Svg);
    }

    #[test]
    fn plain_text() {
        assert_eq!(sn(b"hello"), Kind::Text);
    }

    #[test]
    fn nul_byte_is_unknown() {
        assert_eq!(sn(b"hello\0world"), Kind::Unknown);
    }

    #[test]
    fn empty_is_unknown() {
        assert_eq!(sn(b""), Kind::Unknown);
        assert_eq!(sniff(b"", b""), Kind::Unknown);
    }

    #[test]
    fn tiny_inputs_do_not_panic() {
        let _ = sn(&[0x89]);
        let _ = sn(&[0x1A, 0x45, 0xDF]);
        let _ = sn(b"abc");
        assert_eq!(sn(b"abc"), Kind::Text);
        assert_eq!(sniff(&[0x01], &[0x01]), Kind::Text);
        let _ = sniff(&[0xFF, 0xFE, 0x00], &[]);
    }

    #[test]
    fn dmg_trailer_in_tail() {
        let mut trailer = b"koly".to_vec();
        trailer.resize(TAIL, 0);
        let head = with_len(b"x".to_vec(), HEAD);
        assert_eq!(sniff(&head, &trailer), Kind::Dmg);
        assert_ne!(sniff(&head, &[0u8; TAIL]), Kind::Dmg);
    }

    #[test]
    fn verdict_table() {
        let png = with_len(b"\x89PNG\r\n\x1a\n".to_vec(), 32);
        let html = b"<!DOCTYPE html><html></html>".to_vec();
        let docx = zip_local(b"[Content_Types].xml", b"");

        assert_eq!(verdict(sn(&png), "a.png", None, false), Verdict::Match);
        assert_eq!(verdict(sn(&html), "a.png", None, false), Verdict::WebPage);
        assert_eq!(
            verdict(sn(&zip_local(b"a.txt", b"x")), "a.exe", None, false),
            Verdict::Mismatch
        );
        assert_eq!(verdict(sn(&docx), "a.docx", None, false), Verdict::Match);
        assert_eq!(verdict(sn(b"hello"), "a.txt", None, false), Verdict::Unchecked);
        assert_eq!(verdict(sn(&html), "a.html", None, false), Verdict::Match);
        assert_eq!(
            verdict(sn(&[0xFF, 0xFB, 0x90, 0x00, 0, 0]), "a.mp3", None, false),
            Verdict::Unchecked
        );
        assert_eq!(verdict(sn(&html), "x.png", None, true), Verdict::WebPage);
        assert_eq!(
            verdict(sn(&ftyp(b"isom")), "x.ts", None, true),
            Verdict::Match
        );
    }

    #[test]
    fn verdict_no_extension_and_html_exe() {
        assert_eq!(
            verdict(Kind::Html, "download", None, false),
            Verdict::WebPage
        );
        assert_eq!(verdict(Kind::Png, "download", None, false), Verdict::Unchecked);
        assert_eq!(verdict(Kind::Html, "a.exe", None, false), Verdict::WebPage);
    }

    #[test]
    fn fixed_name_cases() {
        assert_eq!(fixed_name("photo.jpg", Kind::Png), "photo.png");
        assert_eq!(fixed_name("a.tar.gz", Kind::Gzip), "a.tar.gz");
        assert_eq!(fixed_name("noext", Kind::Html), "noext.html");
        assert_eq!(fixed_name(".bashrc", Kind::Html), ".bashrc.html");
    }

    #[test]
    fn id_round_trip() {
        for &k in ALL {
            assert_eq!(Kind::from_id(k.id()), Some(k));
        }
        assert_eq!(Kind::from_id("nope"), None);
    }

    #[test]
    fn sniff_file_small_and_large() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();

        let small = dir.join(format!("tdm_filetype_small_{pid}.bin"));
        std::fs::write(&small, b"\x89PNG\r\n\x1a\nrest").unwrap();
        assert_eq!(sniff_file(&small).unwrap(), Kind::Png);
        std::fs::remove_file(&small).unwrap();

        let big = dir.join(format!("tdm_filetype_big_{pid}.bin"));
        let mut data = vec![0u8; HEAD + 4096];
        let n = data.len();
        data[n - TAIL..n - TAIL + 4].copy_from_slice(b"koly");
        std::fs::write(&big, &data).unwrap();
        assert_eq!(sniff_file(&big).unwrap(), Kind::Dmg);
        std::fs::remove_file(&big).unwrap();
    }

    #[test]
    fn fixed_name_and_compat_helpers() {
        assert!(Kind::Zip.compatible("docx"));
        assert!(!Kind::Png.compatible("jpg"));
        assert!(Kind::Text.compatible("anything"));
        assert_eq!(Kind::Ooxml.category(), Category::Document);
        assert_eq!(Kind::Html.category(), Category::WebPage);
    }

    #[test]
    fn executables_are_not_renamable() {
        for k in [
            Kind::Exe,
            Kind::Elf,
            Kind::MachO,
            Kind::Dmg,
            Kind::Deb,
            Kind::Rpm,
            Kind::Apk,
            Kind::AppImage,
        ] {
            assert!(!k.renamable(), "{k:?} must not be renamable");
        }
        assert!(Kind::Png.renamable());
    }

    #[test]
    fn apk_with_meta_inf_first_is_jar_and_accepts_apk() {
        let apk = zip_local(b"META-INF/", b"");
        assert_eq!(sn(&apk), Kind::Jar);
        assert!(Kind::Jar.compatible("apk"));
        assert_eq!(verdict(sn(&apk), "app.apk", None, false), Verdict::Match);
    }

    #[test]
    fn zip_family_names_match_ooxml_content() {
        let docx = zip_local(b"[Content_Types].xml", b"");
        assert_eq!(verdict(sn(&docx), "a.nupkg", None, false), Verdict::Match);
        assert_eq!(verdict(sn(&docx), "a.vsix", None, false), Verdict::Match);
    }

    #[test]
    fn text_and_generic_names_are_unchecked() {
        let mut ico_like = vec![0u8, 0, 1, 0];
        ico_like.extend_from_slice(&[1, 0, 16, 16]);
        assert_eq!(
            verdict(sn(b"OTTO,a,b\n"), "data.csv", None, false),
            Verdict::Unchecked
        );
        assert_eq!(
            verdict(sn(b"%PDF-1.4 is a common phrase"), "notes.txt", None, false),
            Verdict::Unchecked
        );
        assert_eq!(
            verdict(sn(&ico_like), "blob.dat", None, false),
            Verdict::Unchecked
        );
        assert_eq!(
            verdict(sn(&[0u8, 0, 1, 0]), "blob.bin", None, false),
            Verdict::Unchecked
        );
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Leading bytes of real formats, so the parsers get past their first checks.
    const MAGICS: &[&[u8]] = &[
        b"\x89PNG\r\n\x1a\n",
        b"\xFF\xD8\xFF",
        b"GIF89a",
        b"RIFF\0\0\0\0WEBP",
        b"RIFF\0\0\0\0AVI ",
        b"BM",
        b"%PDF-",
        b"PK\x03\x04",
        b"PK\x05\x06",
        b"7z\xBC\xAF\x27\x1C",
        b"\x1F\x8B\x08",
        b"BZh9",
        b"\x28\xB5\x2F\xFD",
        b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        b"MZ",
        b"\x7FELF",
        b"\xCF\xFA\xED\xFE",
        b"\xCA\xFE\xBA\xBE",
        b"\x1A\x45\xDF\xA3",
        b"\x00\x00\x00\x18ftyp",
        b"\x00\x00\x00\x18ftypisom",
        b"\x00\x00\x01\x00",
        b"ID3",
        b"OggS",
        b"fLaC",
        b"\xFF\xF1",
        b"\x47\x00",
        b"wOFF",
        b"wOF2",
        b"SQLite format 3\0",
        b"\0asm",
        b"<!DOCTYPE html>",
        b"<?xml version=\"1.0\"?><error/>",
        b"\xEF\xBB\xBF<html>",
        b"\xFF\xFE<\0h\0t\0m\0l\0",
    ];

    fn bytes_up_to_8k() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 0..=8192)
    }

    fn head_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            bytes_up_to_8k(),
            (prop::sample::select(MAGICS), bytes_up_to_8k()).prop_map(|(magic, rest)| {
                let mut head = magic.to_vec();
                head.extend(rest);
                head.truncate(HEAD);
                head
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn sniff_and_verdict_never_panic(head in head_bytes(), tail in bytes_up_to_8k()) {
            let kind = sniff(&head, &tail);
            for name in ["clip.mp4", ".mp4", "x.MP4", "a.png", "b.html", "c", "d.tar.gz", "e."] {
                let _ = verdict(kind, name, None, false);
                let _ = verdict(kind, name, Some("video/mp4"), true);
            }
            let _ = fixed_name("weird.", kind);
        }

        /// Random bytes named `.mp4` must not be reported as a match. A `Match` for an
        /// mp4 name needs an `ftyp` box at offset 4, which is the only signature that
        /// sniffs as an MP4-family kind, so that case is excluded.
        #[test]
        fn random_bytes_named_mp4_are_never_a_match(
            head in bytes_up_to_8k(),
            tail in bytes_up_to_8k(),
        ) {
            prop_assume!(head.get(4..8) != Some(b"ftyp".as_slice()));
            let kind = sniff(&head, &tail);
            for name in ["clip.mp4", ".mp4"] {
                prop_assert!(verdict(kind, name, None, false) != Verdict::Match, "{kind:?} {name}");
            }
        }
    }
}
