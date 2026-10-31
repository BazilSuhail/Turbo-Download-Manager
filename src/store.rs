//! On-disk state: settings and the download list.
//!
//! Plain JSON files rather than the registry or a database: portable across
//! platforms, easy to back up, and hand-editable. Per-download resume state is
//! not here; it lives in the `.tdmstate` sidecar written by `engine`, next to
//! the data it describes, so the two always fail or survive together.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Settings;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("writing {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{path} is not valid JSON: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("encoding state: {0}")]
    Encode(#[from] serde_json::Error),
}

/// One row of the saved download list. Live progress is not stored here; it is
/// recovered from the engine's sidecar when the download is resumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub url: String,
    pub dest: PathBuf,
    /// Category id this download was filed under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<u32>,
    /// Headers replayed on every request for this download (Referer, Cookie,
    /// User-Agent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    /// Total size once known, so the list can render before the first probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Whether the last run finished. Everything else is re-derived.
    #[serde(default)]
    pub complete: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DownloadList {
    #[serde(default)]
    pub downloads: Vec<DownloadRecord>,
}

/// The per-user directory for everything we keep: settings, the download
/// list, the IPC endpoint and the native-host manifests. That is
/// `%APPDATA%\tdm`, `~/.config/tdm` or
/// `~/Library/Application Support/tdm`; `./tdm` only when the OS
/// reports no config directory at all (no `HOME`).
pub fn app_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("tdm")
}

/// Where our state lives. One directory, two files.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// [`app_dir`].
    pub fn default_location() -> Self {
        Self { dir: app_dir() }
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    pub fn list_path(&self) -> PathBuf {
        self.dir.join("downloads.json")
    }

    /// Load settings, falling back to defaults when the file is absent.
    /// A corrupt file is an error rather than a silent reset, so a typo in a
    /// hand-edited config cannot quietly discard the user's categories.
    pub fn load_settings(&self) -> Result<Settings, Error> {
        match read_json(&self.settings_path())? {
            Some(s) => Ok(s),
            None => Ok(Settings::default()),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), Error> {
        write_json(&self.settings_path(), settings)
    }

    pub fn load_list(&self) -> Result<DownloadList, Error> {
        Ok(read_json(&self.list_path())?.unwrap_or_default())
    }

    pub fn save_list(&self, list: &DownloadList) -> Result<(), Error> {
        write_json(&self.list_path(), list)
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, Error> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Read { path: path.to_owned(), source }),
    };
    // Notepad, PowerShell's `Set-Content -Encoding utf8` and most Windows
    // editors put a UTF-8 BOM at the front. `serde_json` treats it as junk, so
    // a hand-edited settings file would silently fall back to defaults and
    // lose the user's categories on the next save.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|source| Error::Parse { path: path.to_owned(), source })
}

/// Write via a temporary file and a rename, so an interrupted write cannot
/// leave a half-written state file behind.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| Error::Write { path: parent.to_owned(), source })?;
    }
    let json = serde_json::to_vec_pretty(value)?;
    // The temp name carries the pid: two instances writing at once would
    // otherwise share one scratch file and rename a half-written blend of both
    // over the real one.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|source| Error::Write { path: tmp.clone(), source })?;
    std::fs::rename(&tmp, path).map_err(|source| Error::Write { path: path.to_owned(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir()
            .join(format!("tdm-store-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        Store::at(dir)
    }

    #[test]
    fn missing_files_yield_defaults_not_errors() {
        let s = temp_store("missing");
        assert_eq!(s.load_settings().unwrap().max_active, 3);
        assert!(s.load_list().unwrap().downloads.is_empty());
    }

    #[test]
    fn settings_round_trip_through_disk() {
        let store = temp_store("roundtrip");
        let settings = Settings {
            max_segments: 16,
            speed_limit: Some(1024 * 1024),
            ..Settings::default()
        };
        store.save_settings(&settings).unwrap();

        let loaded = store.load_settings().unwrap();
        assert_eq!(loaded.max_segments, 16);
        assert_eq!(loaded.speed_limit, Some(1024 * 1024));
        assert_eq!(loaded.categories.len(), settings.categories.len());
    }

    #[test]
    fn download_list_round_trips_and_keeps_headers() {
        let store = temp_store("list");
        let list = DownloadList {
            downloads: vec![DownloadRecord {
                url: "https://example.test/a.zip".into(),
                dest: PathBuf::from("D:\\dl\\Compressed\\a.zip"),
                category_id: Some(7),
                headers: vec![("Referer".into(), "https://example.test/".into())],
                total: Some(1234),
                complete: false,
            }],
        };
        store.save_list(&list).unwrap();

        let back = store.load_list().unwrap();
        assert_eq!(back.downloads.len(), 1);
        assert_eq!(back.downloads[0].category_id, Some(7));
        assert_eq!(back.downloads[0].headers[0].0, "Referer");
        assert_eq!(back.downloads[0].total, Some(1234));
    }

    #[test]
    fn a_corrupt_settings_file_reports_an_error_rather_than_resetting() {
        let store = temp_store("corrupt");
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.settings_path(), b"{ not json").unwrap();
        let err = store.load_settings().unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
    }

    #[test]
    fn no_temp_file_is_left_behind_after_a_successful_write() {
        let store = temp_store("tmp");
        store.save_settings(&Settings::default()).unwrap();
        assert!(store.settings_path().exists());
        let leftovers: Vec<_> = std::fs::read_dir(&store.dir)
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    #[test]
    fn a_file_saved_by_a_windows_editor_still_loads() {
        // Notepad and PowerShell both write this BOM; without the strip the
        // file parses as corrupt and the user's settings vanish.
        let store = temp_store("bom");
        std::fs::create_dir_all(&store.dir).unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        let settings = Settings { max_segments: 12, ..Settings::default() };
        bytes.extend(serde_json::to_vec(&settings).unwrap());
        std::fs::write(store.settings_path(), bytes).unwrap();
        assert_eq!(store.load_settings().expect("loads").max_segments, 12);
    }

    #[test]
    fn the_temp_name_is_unique_per_process() {
        // Two instances must not share one scratch file.
        let store = temp_store("pid");
        let tmp = store.settings_path().with_extension(format!("{}.tmp", std::process::id()));
        assert!(
            tmp.to_string_lossy().contains(&std::process::id().to_string()),
            "temp name does not identify the process"
        );
    }
}
