//! Durable JSON state files: atomic replace, a one-save-old backup, and
//! recovery from a corrupt main file. A corrupt file is never silently reset.
//!
//! Two write modes:
//! - [`SyncMode::Lifecycle`] (settings, list saves on user action): fsync the
//!   new file, keep the previous good file as `<path>.bak`, rename into place,
//!   then fsync the parent directory on unix. Survives power loss.
//! - [`SyncMode::Tick`] (frequent progress-only saves): rename only. Cheaper,
//!   but a Tick write can be lost on power failure, and the `.bak` is one
//!   Lifecycle save older than the main file.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{} is corrupt: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Saves on user action, open and close. Fully synced; keeps a backup.
    Lifecycle,
    /// Frequent progress saves. No fsync, no backup.
    Tick,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadOutcome<T> {
    /// The main file parsed.
    Main(T),
    /// The main file was missing or corrupt; the `.bak` file was used.
    Backup(T),
    /// Neither file exists.
    Missing,
}

/// Write `value` as pretty JSON to `path` via a temp file in the same
/// directory and a rename, so an interrupted write never leaves a half-written
/// file. The parent directory must exist.
pub fn write_json_durable<T: Serialize>(
    path: &Path,
    value: &T,
    mode: SyncMode,
) -> Result<(), StoreError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(&bytes)?;
    tmp.flush()?;
    if mode == SyncMode::Lifecycle {
        tmp.as_file().sync_all()?;
        keep_backup(path)?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    if mode == SyncMode::Lifecycle {
        sync_dir(parent);
    }
    Ok(())
}

/// Read `path`. If it is missing or corrupt, fall back to `<path>.bak`.
///
/// A corrupt main file is moved aside to `<path>.corrupt-<unix secs>.bak`
/// (with `-<n>` added when that name is taken) before the backup is tried, so
/// the bytes are never lost. If no valid file remains, returns
/// [`StoreError::Corrupt`].
pub fn read_json_or_backup<T: DeserializeOwned>(
    path: &Path,
) -> Result<LoadOutcome<T>, StoreError> {
    let bak = backup_path(path);
    match read_probe::<T>(path)? {
        Probe::Value(v) => Ok(LoadOutcome::Main(v)),
        Probe::Missing => match read_probe::<T>(&bak)? {
            Probe::Value(v) => Ok(LoadOutcome::Backup(v)),
            Probe::Missing => Ok(LoadOutcome::Missing),
            Probe::Bad(reason) => Err(StoreError::Corrupt { path: bak, reason }),
        },
        Probe::Bad(reason) => {
            let aside = move_aside(path)?;
            tracing::warn!(
                path = %path.display(),
                aside = %aside.display(),
                "corrupt state file moved aside; trying the backup"
            );
            match read_probe::<T>(&bak)? {
                Probe::Value(v) => Ok(LoadOutcome::Backup(v)),
                _ => Err(StoreError::Corrupt { path: path.to_owned(), reason }),
            }
        }
    }
}

enum Probe<T> {
    Value(T),
    Missing,
    Bad(String),
}

fn read_probe<T: DeserializeOwned>(path: &Path) -> Result<Probe<T>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Probe::Missing),
        Err(e) => return Err(e.into()),
    };
    // Notepad and PowerShell write a UTF-8 BOM; serde_json would reject it.
    let body = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    Ok(match serde_json::from_slice(body) {
        Ok(v) => Probe::Value(v),
        Err(e) => Probe::Bad(e.to_string()),
    })
}

/// Before a lifecycle save replaces `path`, keep the current file as the
/// backup, but only if it is valid JSON. A corrupt main never overwrites a
/// good backup. The copy goes to a temp file that is synced and then renamed
/// over the backup, so a crash mid-copy leaves the previous `.bak` intact.
fn keep_backup(path: &Path) -> Result<(), StoreError> {
    if let Ok(Probe::Value(_)) = read_probe::<serde_json::Value>(path) {
        let bak = backup_path(path);
        let tmp = with_suffix(&bak, ".tmp");
        std::fs::copy(path, &tmp)?;
        std::fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, &bak)?;
    }
    Ok(())
}

/// Renames `path` to a `.corrupt-<secs>.bak` name that no file holds yet. The
/// name is claimed with `create_new`, so an existing corrupt file is never
/// overwritten.
fn move_aside(path: &Path) -> io::Result<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let names = std::iter::once(format!(".corrupt-{secs}.bak"))
        .chain((1u64..).map(|n| format!(".corrupt-{secs}-{n}.bak")));
    for suffix in names {
        let aside = with_suffix(path, &suffix);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&aside) {
            Ok(claim) => {
                drop(claim);
                if let Err(e) = std::fs::rename(path, &aside) {
                    let _ = std::fs::remove_file(&aside);
                    return Err(e);
                }
                return Ok(aside);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("the name sequence is infinite")
}

fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".bak")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Make a rename durable. Best effort: the data is already synced, and a
/// failure here only risks the rename itself.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Doc {
        n: u32,
    }

    fn corrupt_asides(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                name.starts_with("state.json.corrupt-") && name.ends_with(".bak")
            })
            .collect()
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json_durable(&path, &Doc { n: 1 }, SyncMode::Lifecycle).unwrap();
        assert_eq!(
            read_json_or_backup::<Doc>(&path).unwrap(),
            LoadOutcome::Main(Doc { n: 1 })
        );
    }

    #[test]
    fn lifecycle_write_keeps_the_previous_file_as_bak() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json_durable(&path, &Doc { n: 1 }, SyncMode::Lifecycle).unwrap();
        write_json_durable(&path, &Doc { n: 2 }, SyncMode::Lifecycle).unwrap();
        let bak: Doc = serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(bak, Doc { n: 1 });
    }

    #[test]
    fn backup_is_complete_after_repeated_lifecycle_writes_and_no_temp_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        for n in 1..=3 {
            write_json_durable(&path, &Doc { n }, SyncMode::Lifecycle).unwrap();
        }
        let bak: Doc = serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(bak, Doc { n: 2 });
        assert!(!with_suffix(&backup_path(&path), ".tmp").exists());
    }

    #[test]
    fn corrupt_main_with_valid_backup_loads_the_backup_and_keeps_the_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json_durable(&path, &Doc { n: 1 }, SyncMode::Lifecycle).unwrap();
        write_json_durable(&path, &Doc { n: 2 }, SyncMode::Lifecycle).unwrap();
        std::fs::write(&path, b"{ broken").unwrap();

        assert_eq!(
            read_json_or_backup::<Doc>(&path).unwrap(),
            LoadOutcome::Backup(Doc { n: 1 })
        );
        let asides = corrupt_asides(dir.path());
        assert_eq!(asides.len(), 1, "asides: {asides:?}");
        assert_eq!(std::fs::read(&asides[0]).unwrap(), b"{ broken");
    }

    #[test]
    fn corrupt_main_without_backup_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{ broken").unwrap();
        let err = read_json_or_backup::<Doc>(&path).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "got {err:?}");
        assert!(err.to_string().contains("state.json"));
    }

    #[test]
    fn corrupt_main_and_corrupt_backup_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{ broken").unwrap();
        std::fs::write(backup_path(&path), b"also broken").unwrap();
        let err = read_json_or_backup::<Doc>(&path).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "got {err:?}");
    }

    #[test]
    fn tick_write_creates_no_bak() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json_durable(&path, &Doc { n: 1 }, SyncMode::Tick).unwrap();
        write_json_durable(&path, &Doc { n: 2 }, SyncMode::Tick).unwrap();
        assert!(!backup_path(&path).exists());
        assert_eq!(
            read_json_or_backup::<Doc>(&path).unwrap(),
            LoadOutcome::Main(Doc { n: 2 })
        );
    }
}
