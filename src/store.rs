//! On-disk state: settings, the download list and learned host limits.
//!
//! Plain JSON files rather than the registry or a database: portable across
//! platforms, easy to back up, and hand-editable. Per-download resume state is
//! not here; it lives in the `.tdmstate` sidecar written by `engine`, next to
//! the data it describes, so the two always fail or survive together.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tdm_store::{LoadOutcome, SyncMode};

use crate::hosts::{HostLimits, HostRecord};
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
    #[error(transparent)]
    Store(#[from] tdm_store::StoreError),
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
    /// Alternative URLs for the same file, tried when a segment keeps failing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    /// Total size once known, so the list can render before the first probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Whether the last run finished. Everything else is re-derived.
    #[serde(default)]
    pub complete: bool,
    /// The real file type found by sniffing, as `filetype::Kind::id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The Content-Type the server sent, kept for the type warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DownloadList {
    #[serde(default)]
    pub downloads: Vec<DownloadRecord>,
}

/// The per-user directory for everything we keep: settings, the download
/// list, the IPC endpoint and the native-host manifests. That is
/// `%APPDATA%\tdm` or `~/.config/tdm`; `./tdm` only when the OS
/// reports no config directory at all (no `HOME`).
pub fn app_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("tdm")
}

/// Where our state lives: settings, the download list and learned host limits.
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
        load(&self.settings_path())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), Error> {
        save(&self.settings_path(), settings)
    }

    pub fn load_list(&self) -> Result<DownloadList, Error> {
        load(&self.list_path())
    }

    pub fn save_list(&self, list: &DownloadList) -> Result<(), Error> {
        save(&self.list_path(), list)
    }

    pub fn hosts_path(&self) -> PathBuf {
        self.dir.join("hosts.json")
    }

    /// Learned per-host connection limits. A missing file means none yet.
    pub fn load_hosts(&self) -> Result<HostLimits, Error> {
        let map: BTreeMap<String, HostRecord> = load(&self.hosts_path())?;
        Ok(HostLimits::from_map(map))
    }

    pub fn save_hosts(&self, limits: &HostLimits) -> Result<(), Error> {
        save(&self.hosts_path(), &limits.to_map())
    }
}

/// The entry in `list` that a new download of `url` would duplicate, if any.
///
/// Two rules, either one enough: the URLs agree on scheme, host, path and query
/// (the fragment is ignored), or the file names agree and so do the sizes
/// wherever both are known.
pub fn find_duplicate<'a>(
    list: &'a [DownloadRecord],
    url: &str,
    file_name: &str,
    size: Option<u64>,
) -> Option<&'a DownloadRecord> {
    let wanted = normalize_url(url);
    list.iter().find(|r| {
        let same_url = wanted.is_some() && normalize_url(&r.url) == wanted;
        let same_name = r.dest.file_name() == Some(std::ffi::OsStr::new(file_name));
        let same_size = match (size, r.total) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        same_url || (same_name && same_size)
    })
}

/// The URL without its fragment, so two links that differ only after `#` compare equal.
fn normalize_url(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url).ok()?;
    parsed.set_fragment(None);
    Some(parsed.as_str().to_owned())
}

/// The main file, else its backup, else defaults. A corrupt file is an error
/// (see `tdm_store::read_json_or_backup`), never a silent reset.
fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T, Error> {
    Ok(match tdm_store::read_json_or_backup(path)? {
        LoadOutcome::Main(v) | LoadOutcome::Backup(v) => v,
        LoadOutcome::Missing => T::default(),
    })
}

/// Lifecycle save: fsynced, with the previous good file kept as a backup.
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| Error::Write { path: parent.to_owned(), source })?;
    }
    tdm_store::write_json_durable(path, value, SyncMode::Lifecycle)?;
    Ok(())
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
    fn host_limits_round_trip_and_missing_file_is_empty() {
        let store = temp_store("hosts");
        assert_eq!(store.load_hosts().unwrap(), HostLimits::new());

        let mut limits = HostLimits::new();
        limits.on_refused("files.example.test", 5);
        store.save_hosts(&limits).unwrap();
        assert_eq!(store.load_hosts().unwrap(), limits);
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
                mirrors: vec!["https://mirror.example.test/a.zip".into()],
                total: Some(1234),
                complete: false,
                kind: Some("png".into()),
                content_type: Some("text/html".into()),
            }],
        };
        store.save_list(&list).unwrap();

        let back = store.load_list().unwrap();
        assert_eq!(back.downloads.len(), 1);
        assert_eq!(back.downloads[0].category_id, Some(7));
        assert_eq!(back.downloads[0].headers[0].0, "Referer");
        assert_eq!(back.downloads[0].total, Some(1234));
        assert_eq!(back.downloads[0].kind.as_deref(), Some("png"));
        assert_eq!(back.downloads[0].content_type.as_deref(), Some("text/html"));
    }

    #[test]
    fn a_record_saved_before_kind_existed_still_loads() {
        // Lists written by older builds have no kind or content_type fields.
        let store = temp_store("old-record");
        std::fs::create_dir_all(&store.dir).unwrap();
        let old = r#"{"downloads":[{"url":"https://example.test/a.zip","dest":"D:\\dl\\a.zip"}]}"#;
        std::fs::write(store.list_path(), old).unwrap();

        let list = store.load_list().expect("old list loads");
        assert_eq!(list.downloads[0].kind, None);
        assert_eq!(list.downloads[0].content_type, None);
        assert!(!list.downloads[0].complete);
    }

    #[test]
    fn an_absent_kind_is_not_written_to_disk() {
        // Keeps old readers happy: no new keys appear unless there is a value.
        let record = DownloadRecord {
            url: "https://example.test/a.zip".into(),
            dest: PathBuf::from("a.zip"),
            category_id: None,
            headers: vec![],
            mirrors: vec![],
            total: None,
            complete: true,
            kind: None,
            content_type: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("\"kind\""), "unexpected key in {json}");
        assert!(!json.contains("content_type"), "unexpected key in {json}");
    }

    #[test]
    fn a_corrupt_settings_file_reports_an_error_rather_than_resetting() {
        let store = temp_store("corrupt");
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.settings_path(), b"{ not json").unwrap();
        let err = store.load_settings().unwrap_err();
        assert!(
            matches!(err, Error::Store(tdm_store::StoreError::Corrupt { .. })),
            "got {err:?}"
        );
    }

    #[test]
    fn no_temp_file_is_left_behind_after_a_successful_write() {
        let store = temp_store("tmp");
        store.save_settings(&Settings::default()).unwrap();
        assert!(store.settings_path().exists());
        let names: Vec<_> = std::fs::read_dir(&store.dir)
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["settings.json"], "unexpected files: {names:?}");
    }

    #[test]
    fn a_second_save_keeps_the_previous_settings_as_a_backup() {
        let store = temp_store("backup");
        store.save_settings(&Settings { max_segments: 4, ..Settings::default() }).unwrap();
        store.save_settings(&Settings { max_segments: 9, ..Settings::default() }).unwrap();
        std::fs::write(store.settings_path(), b"{ torn").unwrap();
        assert_eq!(store.load_settings().expect("backup loads").max_segments, 4);
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

    fn record(url: &str, name: &str, total: Option<u64>) -> DownloadRecord {
        DownloadRecord {
            url: url.into(),
            dest: PathBuf::from("D:\\dl").join(name),
            category_id: None,
            headers: vec![],
            mirrors: vec![],
            total,
            complete: false,
            kind: None,
            content_type: None,
        }
    }

    #[test]
    fn the_same_url_is_a_duplicate_whatever_the_fragment() {
        let list = [record("https://example.test/a.zip?x=1", "a.zip", Some(10))];
        let hit = find_duplicate(&list, "https://example.test/a.zip?x=1#top", "other.bin", None);
        assert_eq!(hit.map(|r| r.url.as_str()), Some("https://example.test/a.zip?x=1"));
    }

    #[test]
    fn a_different_query_is_not_the_same_url() {
        let list = [record("https://example.test/get?id=1", "a.zip", Some(10))];
        // Name and size differ as well, so only the URL rule could match.
        assert!(find_duplicate(&list, "https://example.test/get?id=2", "b.zip", Some(20)).is_none());
    }

    #[test]
    fn a_different_query_with_the_same_name_and_size_matches_by_name_and_size() {
        let list = [record("https://example.test/get?id=1", "a.zip", Some(10))];
        assert!(find_duplicate(&list, "https://example.test/get?id=2", "a.zip", Some(10)).is_some());
    }

    #[test]
    fn the_same_name_and_size_is_a_duplicate_from_another_url() {
        let list = [record("https://a.test/a.zip", "a.zip", Some(10))];
        assert!(find_duplicate(&list, "https://b.test/mirror/a.zip", "a.zip", Some(10)).is_some());
        assert!(find_duplicate(&list, "https://b.test/mirror/a.zip", "a.zip", Some(11)).is_none());
    }

    #[test]
    fn an_unknown_size_does_not_block_a_name_match() {
        let list = [record("https://a.test/a.zip", "a.zip", None)];
        assert!(find_duplicate(&list, "https://b.test/a.zip", "a.zip", Some(10)).is_some());
        assert!(find_duplicate(&list, "https://b.test/a.zip", "a.zip", None).is_some());
    }

    #[test]
    fn an_empty_list_has_no_duplicates() {
        assert!(find_duplicate(&[], "https://a.test/a.zip", "a.zip", Some(1)).is_none());
    }
}
