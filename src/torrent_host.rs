//! The app side of the torrent worker.
//!
//! The worker is a separate process (`tdm-torrent-worker`, next to the app
//! executable). This module spawns it, writes one encoded command per line to
//! its stdin, and reads one encoded event per line from its stdout. Its stderr
//! is forwarded to tracing. The worker is never started through a shell.
//!
//! Torrents stay off unless the settings enable them and the DHT notice is
//! accepted. `may_start` is the single check for that.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command as ProcCommand};
use tokio::sync::mpsc;

use crate::model::DownloadSettings;
use crate::torrent_policy;
use crate::torrent_worker::{Command, Event, Source, decode_line, encode_line};

/// Events the worker may have queued before the app reads them.
const EVENT_QUEUE: usize = 256;

/// How long the worker gets to exit after `Shutdown` before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("torrents are not enabled")]
    Disabled,
    #[error("torrent worker not found at {0}")]
    WorkerMissing(PathBuf),
    #[error("could not start the torrent worker: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("torrent worker pipe error: {0}")]
    Io(#[source] std::io::Error),
    #[error("magnet link refused")]
    MagnetRefused,
    #[error("torrent file refused ({0} bytes)")]
    FileRefused(u64),
    #[error("torrent path is not a regular file")]
    NotAFile,
}

/// True only when torrents are switched on and the DHT notice is accepted.
pub fn may_start(settings: &DownloadSettings) -> bool {
    settings.torrents_enabled && settings.torrents_dht_notice_accepted
}

/// The worker binary beside the app executable.
fn worker_path() -> Result<PathBuf, HostError> {
    let exe = std::env::current_exe().map_err(HostError::Spawn)?;
    let dir = exe.parent().ok_or_else(|| {
        HostError::Spawn(std::io::Error::other("app executable has no parent directory"))
    })?;
    let name = if cfg!(windows) {
        "tdm-torrent-worker.exe"
    } else {
        "tdm-torrent-worker"
    };
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(HostError::WorkerMissing(path))
    }
}

/// Budget for torrent traffic, from the HTTP cap and the smoothed HTTP rate.
pub fn budget_for(cap: Option<u64>, http_smoothed: u64) -> Option<u64> {
    torrent_policy::torrent_budget(cap, http_smoothed)
}

pub struct TorrentHost {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<Event>,
}

impl TorrentHost {
    /// Spawns the worker. Refuses unless `may_start` says yes.
    pub async fn start(settings: &DownloadSettings) -> Result<Self, HostError> {
        if !may_start(settings) {
            return Err(HostError::Disabled);
        }
        let path = worker_path()?;
        let mut cmd = ProcCommand::new(&path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(HostError::Spawn)?;

        let pipe_err = || HostError::Spawn(std::io::Error::other("worker pipe missing"));
        let stdin = child.stdin.take().ok_or_else(pipe_err)?;
        let stdout = child.stdout.take().ok_or_else(pipe_err)?;
        let stderr = child.stderr.take().ok_or_else(pipe_err)?;

        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match decode_line(&line) {
                    Ok(event) => {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => tracing::warn!("torrent worker sent an unreadable line"),
                }
            }
        });

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "tdm::torrent_worker", "{line}");
            }
        });

        Ok(Self {
            child,
            stdin,
            events: rx,
        })
    }

    /// Events from the worker. The channel closes when the worker's stdout ends.
    pub fn events(&mut self) -> &mut mpsc::Receiver<Event> {
        &mut self.events
    }

    async fn send(&mut self, cmd: &Command) -> Result<(), HostError> {
        let mut line = encode_line(cmd);
        if !line.ends_with('\n') {
            line.push('\n');
        }
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(HostError::Io)?;
        self.stdin.flush().await.map_err(HostError::Io)
    }

    /// Adds a magnet link. Refused before sending unless `magnet_ok` accepts it.
    pub async fn add_magnet(&mut self, id: u64, magnet: &str, save_dir: &Path) -> Result<(), HostError> {
        if !torrent_policy::magnet_ok(magnet) {
            return Err(HostError::MagnetRefused);
        }
        let cmd = Command::Add {
            id: id.to_string(),
            source: Source::Magnet(magnet.to_owned()),
            save_dir: save_dir.to_string_lossy().into_owned(),
        };
        self.send(&cmd).await
    }

    /// Adds a `.torrent` file. Refused when `torrent_file_ok` rejects its size.
    pub async fn add_file(&mut self, id: u64, path: &Path, save_dir: &Path) -> Result<(), HostError> {
        let meta = tokio::fs::metadata(path).await.map_err(HostError::Io)?;
        if !meta.is_file() {
            return Err(HostError::NotAFile);
        }
        let size = meta.len();
        if !torrent_policy::torrent_file_ok(size) {
            return Err(HostError::FileRefused(size));
        }
        let cmd = Command::Add {
            id: id.to_string(),
            source: Source::TorrentPath(path.to_string_lossy().into_owned()),
            save_dir: save_dir.to_string_lossy().into_owned(),
        };
        self.send(&cmd).await
    }

    pub async fn pause(&mut self, id: u64) -> Result<(), HostError> {
        self.send(&Command::Pause { id: id.to_string() }).await
    }

    pub async fn resume(&mut self, id: u64) -> Result<(), HostError> {
        self.send(&Command::Resume { id: id.to_string() }).await
    }

    pub async fn remove(&mut self, id: u64) -> Result<(), HostError> {
        self.send(&Command::Remove { id: id.to_string() }).await
    }

    /// Sets the rate limits in bytes per second. `None` means unlimited.
    /// The caller decides when to send this, using `torrent_policy::should_send_budget`.
    pub async fn set_limits(&mut self, down: Option<u64>, up: Option<u64>) -> Result<(), HostError> {
        self.send(&Command::SetLimits { down_bps: down, up_bps: up }).await
    }

    /// Asks the worker to exit, waits briefly, then kills it. Safe to call when the worker is already gone.
    pub async fn shutdown(&mut self) -> Result<(), HostError> {
        let _ = self.send(&Command::Shutdown).await;
        let _ = self.stdin.shutdown().await;
        match tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(HostError::Io(e)),
            Err(_) => self.child.kill().await.map_err(HostError::Io),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_start_unless_both_flags_are_set() {
        let mut s = DownloadSettings::default();
        assert!(!may_start(&s), "off by default");

        s.torrents_enabled = true;
        assert!(!may_start(&s), "enabled without the DHT notice");

        s.torrents_enabled = false;
        s.torrents_dht_notice_accepted = true;
        assert!(!may_start(&s), "notice without enabling");

        s.torrents_enabled = true;
        assert!(may_start(&s), "both flags set");
    }
}
