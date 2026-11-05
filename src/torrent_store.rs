//! Saved torrent list, so torrents come back after a restart.
//!
//! Only what is needed to re-add a torrent is stored: the magnet text or the
//! `.torrent` file path, the save folder and whether it was paused. Progress
//! and peers are not stored; the engine's own resume data handles that.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The only file format version this build reads or writes.
const VERSION: u32 = 1;

/// Where a torrent came from, so it can be re-added on start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum TorrentSource {
    Magnet(String),
    File(PathBuf),
}

/// One torrent to re-add on start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTorrent {
    pub source: TorrentSource,
    pub save_dir: PathBuf,
    #[serde(default)]
    pub paused: bool,
}

/// On-disk shape. `version` is checked before `torrents` is read.
#[derive(Deserialize)]
struct Stored {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    torrents: Vec<SavedTorrent>,
}

/// Borrowing twin of [`Stored`] used for writing, so saving does not clone.
#[derive(Serialize)]
struct StoredRef<'a> {
    version: u32,
    torrents: &'a [SavedTorrent],
}

/// Reads the saved list. A missing, unreadable, corrupt or unknown-version file
/// gives an empty list. Never panics.
pub fn load(path: &Path) -> Vec<SavedTorrent> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    if value.get("version").and_then(|v| v.as_u64()) != Some(u64::from(VERSION)) {
        return Vec::new();
    }
    serde_json::from_value::<Stored>(value).map(|s| s.torrents).unwrap_or_default()
}

/// Writes the list atomically: a temp file in the same folder is written and
/// synced, then renamed over `path`. A crash leaves either the old file or the
/// new one, never a half-written file.
pub fn save(path: &Path, items: &[SavedTorrent]) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(&StoredRef { version: VERSION, torrents: items })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "torrents.json".to_owned());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));

    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `<config dir>/tdm/torrents.json`, the same folder the app uses for its
/// other state. Falls back to `./tdm` when the OS reports no config folder.
pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tdm")
        .join("torrents.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system temp dir, unique per test and process.
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "tdm_torrent_store_{tag}_{}_{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn sample() -> Vec<SavedTorrent> {
        vec![
            SavedTorrent {
                source: TorrentSource::Magnet("magnet:?xt=urn:btih:abc".into()),
                save_dir: PathBuf::from("C:/Downloads"),
                paused: false,
            },
            SavedTorrent {
                source: TorrentSource::File(PathBuf::from("C:/t/linux.torrent")),
                save_dir: PathBuf::from("D:/Iso"),
                paused: true,
            },
        ]
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let dir = scratch("round");
        let path = dir.join("torrents.json");
        save(&path, &sample()).expect("save");
        assert_eq!(load(&path), sample());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_gives_empty_list() {
        let dir = scratch("missing");
        assert!(load(&dir.join("nope.json")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_gives_empty_list() {
        let dir = scratch("corrupt");
        let path = dir.join("torrents.json");
        fs::write(&path, "{ not json at all").expect("write");
        assert!(load(&path).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_version_gives_empty_list() {
        let dir = scratch("version");
        let path = dir.join("torrents.json");
        fs::write(
            &path,
            r#"{"version": 2, "torrents": [{"source": {"kind": "Magnet", "value": "magnet:?x"}, "save_dir": "C:/d"}]}"#,
        )
        .expect("write");
        assert!(load(&path).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_save_leaves_no_temp_file() {
        let dir = scratch("atomic");
        let path = dir.join("torrents.json");
        save(&path, &sample()).expect("first save");
        save(&path, &sample()[..1]).expect("overwrite");
        let names: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["torrents.json".to_owned()]);
        assert_eq!(load(&path), sample()[..1].to_vec());
        let _ = fs::remove_dir_all(&dir);
    }
}
