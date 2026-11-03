//! Safe archive extraction: zip, tar, tar.gz / tgz, gz, tar.xz and xz.
//!
//! Entries are written into a staging folder (`.tdm-extract-<id>`) inside the destination and moved
//! out only when the whole archive passed. Any failure deletes the staging folder and every file
//! moved so far. Links and device entries are skipped and reported, never created. Names are
//! checked before anything is written. Existing files are never overwritten: `name.ext` becomes
//! `name (2).ext`, `name (3).ext`, and so on.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::MultiGzDecoder;
use lzma_rust2::XzReader;
use tar::EntryType;
use thiserror::Error;

const CHUNK: usize = 64 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

/// Limits that stop an archive bomb. `max_ratio` is uncompressed size over compressed size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_total_bytes: u64,
    pub max_entries: u64,
    pub max_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total_bytes: 10 * GIB,
            max_entries: 100_000,
            max_ratio: 200,
        }
    }
}

/// What an extraction wrote. `files` holds the final paths; `skipped` holds links and devices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
    pub skipped: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("corrupt or unreadable archive: {0}")]
    Corrupt(String),
    #[error("unsafe entry path: {0}")]
    UnsafePath(String),
    #[error("archive is encrypted; nothing was extracted")]
    Encrypted,
    #[error("archive exceeds the {0} limit; nothing was kept")]
    Bomb(&'static str),
    #[error("{0} archives are not supported; open it with 7-Zip")]
    Unsupported(&'static str),
    #[error("unknown archive format")]
    UnknownFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Zip,
    Tar,
    TarGz,
    TarXz,
    Gz,
    Xz,
}

/// True when the file name ends in an archive format `extract` can open.
pub fn is_archive(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_ascii_lowercase(),
        None => return false,
    };
    [".zip", ".tar", ".tar.gz", ".tgz", ".gz", ".tar.xz", ".xz"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// Extracts `archive` into `dest_dir` under the safety rules in the module docs.
pub fn extract(archive: &Path, dest_dir: &Path, limits: &Limits) -> Result<Extracted, ExtractError> {
    let format = detect(archive)?;
    fs::create_dir_all(dest_dir)?;
    let staging = make_staging(dest_dir)?;
    let mut sink = Sink::new(&staging, *limits);
    let mut moved = Vec::new();

    let result = run(format, archive, &mut sink)
        .and_then(|()| publish(&sink, dest_dir, &mut moved));

    match result {
        Ok(()) => {
            let _ = fs::remove_dir_all(&staging);
            Ok(Extracted {
                files: moved,
                total_bytes: sink.total,
                skipped: sink.skipped,
            })
        }
        Err(err) => {
            for path in &moved {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_dir_all(&staging);
            Err(err)
        }
    }
}

fn detect(archive: &Path) -> Result<Format, ExtractError> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        Ok(Format::Zip)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Ok(Format::TarGz)
    } else if name.ends_with(".tar.xz") {
        Ok(Format::TarXz)
    } else if name.ends_with(".tar") {
        Ok(Format::Tar)
    } else if name.ends_with(".gz") {
        Ok(Format::Gz)
    } else if name.ends_with(".xz") {
        Ok(Format::Xz)
    } else if name.ends_with(".7z") {
        Err(ExtractError::Unsupported("7z"))
    } else if name.ends_with(".rar") {
        Err(ExtractError::Unsupported("rar"))
    } else {
        Err(ExtractError::UnknownFormat)
    }
}

fn make_staging(dest: &Path) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = dest.join(format!(".tdm-extract-{}-{nanos:x}", std::process::id()));
    fs::create_dir(&dir)?;
    Ok(dir)
}

fn run(format: Format, archive: &Path, sink: &mut Sink) -> Result<(), ExtractError> {
    match format {
        Format::Zip => extract_zip(archive, sink),
        Format::Tar | Format::TarGz | Format::TarXz => extract_tar(format, archive, sink),
        Format::Gz | Format::Xz => extract_single(format, archive, sink),
    }
}

/// Everything one run writes, plus the limit counters.
struct Sink<'a> {
    staging: &'a Path,
    limits: Limits,
    /// Compressed size of the whole stream (tar.gz, tar.xz, gz, xz). `None` for zip and plain tar.
    stream_base: Option<u64>,
    total: u64,
    entries: u64,
    /// Staged paths, relative to the staging folder.
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    skipped: Vec<String>,
}

impl<'a> Sink<'a> {
    fn new(staging: &'a Path, limits: Limits) -> Self {
        Self {
            staging,
            limits,
            stream_base: None,
            total: 0,
            entries: 0,
            files: Vec::new(),
            dirs: Vec::new(),
            skipped: Vec::new(),
        }
    }

    fn count_entry(&mut self) -> Result<(), ExtractError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(ExtractError::Bomb("entry count"));
        }
        Ok(())
    }

    fn dir(&mut self, rel: PathBuf) -> Result<(), ExtractError> {
        fs::create_dir_all(self.staging.join(&rel))?;
        self.dirs.push(rel);
        Ok(())
    }

    fn file<R: Read>(
        &mut self,
        rel: &Path,
        src: &mut R,
        entry_base: Option<u64>,
    ) -> Result<(), ExtractError> {
        if let Some(parent) = rel.parent() {
            fs::create_dir_all(self.staging.join(parent))?;
        }
        let (mut out, staged) = create_unique(self.staging, rel)?;
        self.copy(src, &mut out, entry_base)?;
        self.files.push(staged);
        Ok(())
    }

    /// Copies one entry and stops as soon as a limit is passed, so a bomb never fills the disk.
    fn copy<R: Read>(
        &mut self,
        src: &mut R,
        dst: &mut File,
        entry_base: Option<u64>,
    ) -> Result<(), ExtractError> {
        let mut buf = vec![0u8; CHUNK];
        let mut written = 0u64;
        let ratio = self.limits.max_ratio;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(ExtractError::Corrupt(err.to_string())),
            };
            written += n as u64;
            self.total += n as u64;
            if self.total > self.limits.max_total_bytes {
                return Err(ExtractError::Bomb("total size"));
            }
            let entry_too_big = entry_base
                .is_some_and(|base| written > ratio.saturating_mul(base.max(1)));
            let stream_too_big = self
                .stream_base
                .is_some_and(|base| self.total > ratio.saturating_mul(base.max(1)));
            if entry_too_big || stream_too_big {
                return Err(ExtractError::Bomb("compression ratio"));
            }
            dst.write_all(&buf[..n])?;
        }
    }
}

/// Moves the staged files and folders into `dest`, recording each final path in `moved`.
fn publish(sink: &Sink, dest: &Path, moved: &mut Vec<PathBuf>) -> Result<(), ExtractError> {
    for dir in &sink.dirs {
        fs::create_dir_all(dest.join(dir))?;
    }
    for rel in &sink.files {
        let target = free_path(&dest.join(rel));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(sink.staging.join(rel), &target)?;
        moved.push(target);
    }
    Ok(())
}

/// `base` for n = 1, otherwise `stem (n).ext`.
fn numbered(base: &Path, n: u32) -> PathBuf {
    if n <= 1 {
        return base.to_path_buf();
    }
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match base.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    base.with_file_name(name)
}

/// First name under `base` that is not taken in the staging folder. Returns the relative name.
fn create_unique(staging: &Path, rel: &Path) -> io::Result<(File, PathBuf)> {
    let mut n: u32 = 1;
    loop {
        let candidate = numbered(rel, n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.join(&candidate))
        {
            Ok(file) => return Ok((file, candidate)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(err) => return Err(err),
        }
    }
}

/// First free name at or after `base`, so an existing file is never replaced.
fn free_path(base: &Path) -> PathBuf {
    let mut n: u32 = 1;
    loop {
        let candidate = numbered(base, n);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

/// Turns an entry name into a relative path of normal components. `Ok(None)` means the name only
/// names the current folder. Absolute paths, drive prefixes, and `..` are errors.
fn safe_relative(name: &str) -> Result<Option<PathBuf>, ExtractError> {
    let unsafe_path = || ExtractError::UnsafePath(name.to_string());
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains('\0') {
        return Err(unsafe_path());
    }
    let mut out = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(unsafe_path()),
            p if p.contains(':') => return Err(unsafe_path()),
            p => out.push(p),
        }
    }
    Ok(if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    })
}

fn zip_error(err: zip::result::ZipError) -> ExtractError {
    match err {
        zip::result::ZipError::Io(e) => ExtractError::Io(e),
        zip::result::ZipError::InvalidPassword => ExtractError::Encrypted,
        other => ExtractError::Corrupt(other.to_string()),
    }
}

enum ZipKind {
    Dir,
    Link,
    File,
}

fn extract_zip(archive: &Path, sink: &mut Sink) -> Result<(), ExtractError> {
    let mut zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?)).map_err(zip_error)?;

    // Pass 1: check every entry before anything is written.
    let mut plan = Vec::with_capacity(zip.len());
    let mut declared = 0u64;
    for index in 0..zip.len() {
        sink.count_entry()?;
        let entry = zip.by_index_raw(index).map_err(zip_error)?;
        if entry.encrypted() {
            return Err(ExtractError::Encrypted);
        }
        let name = String::from_utf8_lossy(entry.name_raw()).into_owned();
        declared = declared.saturating_add(entry.size());
        if declared > sink.limits.max_total_bytes {
            return Err(ExtractError::Bomb("total size"));
        }
        let kind = if entry.is_symlink() {
            ZipKind::Link
        } else if entry.is_dir() {
            ZipKind::Dir
        } else {
            ZipKind::File
        };
        let rel = safe_relative(&name)?;
        plan.push((index, name, kind, rel, entry.compressed_size()));
    }

    // Pass 2: write.
    for (index, name, kind, rel, compressed) in plan {
        match kind {
            ZipKind::Link => sink.skipped.push(name),
            ZipKind::Dir => {
                if let Some(rel) = rel {
                    sink.dir(rel)?;
                }
            }
            ZipKind::File => {
                let rel = rel.ok_or(ExtractError::UnsafePath(name))?;
                let mut entry = zip.by_index(index).map_err(zip_error)?;
                sink.file(&rel, &mut entry, Some(compressed))?;
            }
        }
    }
    Ok(())
}

fn extract_tar(format: Format, archive: &Path, sink: &mut Sink) -> Result<(), ExtractError> {
    let file = File::open(archive)?;
    let stream_len = file.metadata()?.len();
    let reader: Box<dyn Read> = match format {
        Format::TarGz => Box::new(MultiGzDecoder::new(BufReader::new(file))),
        Format::TarXz => Box::new(XzReader::new(BufReader::new(file), true)),
        _ => Box::new(file),
    };
    if format != Format::Tar {
        sink.stream_base = Some(stream_len);
    }

    let mut tar = tar::Archive::new(reader);
    for entry in tar.entries()? {
        let mut entry = entry?;
        sink.count_entry()?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let rel = safe_relative(&name)?;
        let kind = entry.header().entry_type();
        if kind.is_symlink()
            || kind.is_hard_link()
            || matches!(kind, EntryType::Char | EntryType::Block | EntryType::Fifo)
        {
            sink.skipped.push(name);
        } else if kind.is_dir() {
            if let Some(rel) = rel {
                sink.dir(rel)?;
            }
        } else if kind.is_file() {
            let rel = rel.ok_or(ExtractError::UnsafePath(name))?;
            sink.file(&rel, &mut entry, None)?;
        } else {
            sink.skipped.push(name);
        }
    }
    Ok(())
}

/// Single-file gz and xz: the output is the archive name without its last extension.
fn extract_single(format: Format, archive: &Path, sink: &mut Sink) -> Result<(), ExtractError> {
    let file = File::open(archive)?;
    sink.stream_base = Some(file.metadata()?.len());
    let name = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .ok_or(ExtractError::UnknownFormat)?;
    let rel = safe_relative(&name)?.ok_or_else(|| ExtractError::UnsafePath(name.clone()))?;

    let mut reader: Box<dyn Read> = match format {
        Format::Gz => Box::new(MultiGzDecoder::new(BufReader::new(file))),
        _ => Box::new(XzReader::new(BufReader::new(file), true)),
    };
    sink.file(&rel, &mut reader, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "tdm-extract-test-{tag}-{}-{nanos:x}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_tar_gz_with_symlink(path: &Path) {
        let gz = flate2::write::GzEncoder::new(
            File::create(path).unwrap(),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(gz);
        let data = b"hello";

        let mut file_header = tar::Header::new_gnu();
        file_header.set_path("real.txt").unwrap();
        file_header.set_size(data.len() as u64);
        file_header.set_mode(0o644);
        file_header.set_entry_type(EntryType::Regular);
        file_header.set_cksum();
        builder.append(&file_header, &data[..]).unwrap();

        let mut link_header = tar::Header::new_gnu();
        link_header.set_path("link").unwrap();
        link_header.set_size(0);
        link_header.set_entry_type(EntryType::Symlink);
        link_header.set_link_name("real.txt").unwrap();
        link_header.set_cksum();
        builder.append(&link_header, io::empty()).unwrap();

        builder.into_inner().unwrap().finish().unwrap();
    }

    fn entry_count(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn zip_with_parent_dir_entry_is_rejected() {
        let tmp = TempDir::new("dotdot");
        let archive = tmp.0.join("evil.zip");
        write_zip(&archive, &[("../evil.txt", &b"x"[..])]);
        let dest = tmp.0.join("out");

        let err = extract(&archive, &dest, &Limits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)));
        assert!(!tmp.0.join("evil.txt").exists());
        assert_eq!(entry_count(&dest), 0);
    }

    #[test]
    fn zip_with_absolute_path_is_rejected() {
        let tmp = TempDir::new("absolute");
        let archive = tmp.0.join("abs.zip");
        write_zip(&archive, &[("ok.txt", &b"fine"[..]), ("/abs/evil.txt", &b"x"[..])]);
        let dest = tmp.0.join("out");

        let err = extract(&archive, &dest, &Limits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)));
        assert_eq!(entry_count(&dest), 0);
    }

    #[test]
    fn normal_zip_extracts_with_content() {
        let tmp = TempDir::new("normal");
        let archive = tmp.0.join("ok.zip");
        write_zip(&archive, &[("a/hello.txt", &b"hi there"[..])]);
        let dest = tmp.0.join("out");

        let out = extract(&archive, &dest, &Limits::default()).unwrap();
        assert_eq!(out.files, vec![dest.join("a").join("hello.txt")]);
        assert_eq!(out.total_bytes, 8);
        assert!(out.skipped.is_empty());
        assert_eq!(fs::read(dest.join("a").join("hello.txt")).unwrap(), b"hi there");
        assert_eq!(entry_count(&dest), 1);
    }

    #[test]
    fn tar_gz_symlink_is_skipped_not_created() {
        let tmp = TempDir::new("symlink");
        let archive = tmp.0.join("links.tar.gz");
        write_tar_gz_with_symlink(&archive);
        let dest = tmp.0.join("out");

        let out = extract(&archive, &dest, &Limits::default()).unwrap();
        assert_eq!(out.skipped, vec!["link".to_string()]);
        assert_eq!(out.files, vec![dest.join("real.txt")]);
        assert_eq!(fs::read(dest.join("real.txt")).unwrap(), b"hello");
        assert!(fs::symlink_metadata(dest.join("link")).is_err());
    }

    #[test]
    fn existing_name_gets_numbered_copy() {
        let tmp = TempDir::new("duplicate");
        let archive = tmp.0.join("dup.zip");
        write_zip(&archive, &[("a.txt", &b"new"[..])]);
        let dest = tmp.0.join("out");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("a.txt"), b"old").unwrap();

        let out = extract(&archive, &dest, &Limits::default()).unwrap();
        assert_eq!(out.files, vec![dest.join("a (2).txt")]);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dest.join("a (2).txt")).unwrap(), b"new");
    }

    #[test]
    fn total_size_limit_stops_with_bomb_and_leaves_nothing() {
        let tmp = TempDir::new("bomb");
        let archive = tmp.0.join("big.zip");
        let zeros = vec![0u8; 1024 * 1024];
        write_zip(&archive, &[("zeros.bin", &zeros[..])]);
        let dest = tmp.0.join("out");
        let limits = Limits {
            max_total_bytes: 100_000,
            ..Limits::default()
        };

        let err = extract(&archive, &dest, &limits).unwrap_err();
        assert!(matches!(err, ExtractError::Bomb(_)));
        assert_eq!(entry_count(&dest), 0);
    }
}
