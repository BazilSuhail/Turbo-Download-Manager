//! Runs the torrent host on its own thread and tokio runtime, so the UI thread
//! never waits on async work. The UI sends requests with `send` and reads
//! notes with `try_next` on each tick.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::mpsc as tok_mpsc;

use crate::model::DownloadSettings;
use crate::torrent_host::{HostError, TorrentHost, may_start};
use crate::torrent_worker::Event;

/// How long `shutdown` waits for the thread to finish.
const STOP_WAIT: Duration = Duration::from_secs(8);

/// Requests from the UI. Ids are the app's own download ids.
#[derive(Debug)]
pub enum Req {
    Add { id: u64, magnet: String, save_dir: PathBuf },
    Pause(u64),
    Resume(u64),
    Remove(u64),
    Limits { down: Option<u64>, up: Option<u64> },
    Shutdown,
}

/// Notes from the worker to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Progress { id: u64, done: u64, total: u64, peers: u32, down_bps: u64, up_bps: u64 },
    Finished(u64),
    Error { id: Option<u64>, message: String },
    /// The service thread has ended, with the reason.
    Stopped(String),
}

pub struct TorrentService {
    req: tok_mpsc::UnboundedSender<Req>,
    notes: std_mpsc::Receiver<Note>,
    thread: Option<JoinHandle<()>>,
}

impl TorrentService {
    /// Starts the service thread. Refuses unless torrents are enabled and the DHT notice is accepted.
    /// Worker start-up errors arrive later as `Note::Stopped`.
    pub fn start(settings: &DownloadSettings) -> Result<Self, HostError> {
        if !may_start(settings) {
            return Err(HostError::Disabled);
        }
        let settings = settings.clone();
        let (req_tx, req_rx) = tok_mpsc::unbounded_channel();
        let (note_tx, note_rx) = std_mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tdm-torrents".into())
            .spawn(move || {
                let reason = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(run(settings, req_rx, note_tx.clone())),
                    Err(e) => format!("could not start the torrent runtime: {e}"),
                };
                let _ = note_tx.send(Note::Stopped(reason));
            })
            .map_err(HostError::Spawn)?;
        Ok(Self { req: req_tx, notes: note_rx, thread: Some(thread) })
    }

    /// Queues a request. Returns false when the service thread is gone.
    pub fn send(&self, req: Req) -> bool {
        self.req.send(req).is_ok()
    }

    /// The next note, if any. Never blocks.
    pub fn try_next(&self) -> Option<Note> {
        self.notes.try_recv().ok()
    }

    /// Asks the thread to stop and waits a bounded time for it to finish.
    pub fn shutdown(mut self) {
        let _ = self.req.send(Req::Shutdown);
        let Some(thread) = self.thread.take() else { return };
        let (done_tx, done_rx) = std_mpsc::channel();
        std::thread::spawn(move || {
            let _ = thread.join();
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(STOP_WAIT);
    }
}

async fn run(
    settings: DownloadSettings,
    mut req_rx: tok_mpsc::UnboundedReceiver<Req>,
    note_tx: std_mpsc::Sender<Note>,
) -> String {
    let mut host = match TorrentHost::start(&settings).await {
        Ok(host) => host,
        Err(e) => return e.to_string(),
    };
    let Some(mut events) = host.take_events() else {
        return "torrent events unavailable".to_owned();
    };

    loop {
        tokio::select! {
            req = req_rx.recv() => {
                let result = match req {
                    None | Some(Req::Shutdown) => break,
                    Some(Req::Add { id, magnet, save_dir }) => {
                        host.add_magnet(id, &magnet, &save_dir).await.err().map(|e| (Some(id), e))
                    }
                    Some(Req::Pause(id)) => host.pause(id).await.err().map(|e| (Some(id), e)),
                    Some(Req::Resume(id)) => host.resume(id).await.err().map(|e| (Some(id), e)),
                    Some(Req::Remove(id)) => host.remove(id).await.err().map(|e| (Some(id), e)),
                    Some(Req::Limits { down, up }) => host.set_limits(down, up).await.err().map(|e| (None, e)),
                };
                if let Some((id, e)) = result {
                    let _ = note_tx.send(Note::Error { id, message: e.to_string() });
                }
            }
            event = events.recv() => {
                let Some(event) = event else {
                    let _ = host.shutdown().await;
                    return "the torrent worker stopped".to_owned();
                };
                if let Some(note) = note_from(event) {
                    let _ = note_tx.send(note);
                }
            }
        }
    }
    let _ = host.shutdown().await;
    "stopped".to_owned()
}

/// Maps a worker event to a UI note. Worker ids are the app's ids as text.
fn note_from(event: Event) -> Option<Note> {
    match event {
        Event::Ready => None,
        Event::Progress { id, bytes_done, bytes_total, peers, down_bps, up_bps } => Some(Note::Progress {
            id: id.parse().ok()?,
            done: bytes_done,
            total: bytes_total,
            peers,
            down_bps,
            up_bps,
        }),
        Event::Finished { id } => Some(Note::Finished(id.parse().ok()?)),
        Event::Error { id, message } => Some(Note::Error { id: id.and_then(|s| s.parse().ok()), message }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_start_when_disabled() {
        let settings = DownloadSettings::default();
        assert!(matches!(TorrentService::start(&settings), Err(HostError::Disabled)));
    }

    #[test]
    fn maps_worker_events_to_notes() {
        assert_eq!(note_from(Event::Ready), None);
        assert_eq!(
            note_from(Event::Finished { id: "7".into() }),
            Some(Note::Finished(7))
        );
        assert_eq!(
            note_from(Event::Error { id: Some("x".into()), message: "m".into() }),
            Some(Note::Error { id: None, message: "m".into() })
        );
        // An id that is not one of ours is dropped, not guessed.
        assert_eq!(note_from(Event::Finished { id: "x".into() }), None);
    }
}
