//! Torrent worker: a separate process that runs librqbit for the app, so a
//! crash or a hostile peer cannot take the app down.
//!
//! Commands arrive as newline-delimited JSON on stdin and events leave on
//! stdout (see `tdm::torrent_worker`). Logs go to stderr only. On stdin EOF or
//! a `shutdown` command every torrent is stopped and the process exits.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ManagedTorrent, Session,
    SessionOptions,
};
use tdm::torrent_worker::{Command, Event, MAX_LINE, Source, decode_line, encode_line};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Upload cap until the app sends `set_limits`.
const DEFAULT_UP_BPS: u32 = 256 * 1024;
/// Largest `.torrent` file the worker will read.
const MAX_TORRENT_BYTES: u64 = 10 * 1024 * 1024;
/// A line, its CR LF, and one spare byte so an over-long line is detectable.
const READ_LIMIT: u64 = MAX_LINE as u64 + 3;
const CHANNEL_DEPTH: usize = 64;

/// Everything the main loop reacts to.
enum Msg {
    Line(String),
    TooLong,
    Eof,
    Added(String, Result<Arc<ManagedTorrent>, String>),
    Completed(String),
    Failed(String),
}

struct Job {
    torrent: Arc<ManagedTorrent>,
    /// Set once the torrent completed. A finished torrent is paused and is
    /// never resumed, so it never seeds.
    finished: bool,
    watcher: JoinHandle<()>,
}

struct Worker {
    session: Arc<Session>,
    jobs: HashMap<String, Job>,
    /// Ids whose add is still resolving (a magnet can take a while).
    pending: HashSet<String>,
    tx: mpsc::Sender<Msg>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(message) => {
            emit_error(None, message);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode, &'static str> {
    let (tx, mut rx) = mpsc::channel::<Msg>(CHANNEL_DEPTH);
    spawn_stdin_reader(tx.clone());

    let session = Session::new_with_opts(std::env::temp_dir(), session_options())
        .await
        .map_err(|e| {
            eprintln!("torrent session failed to start: {e:#}");
            "session failed to start"
        })?;
    emit(&Event::Ready);

    let mut worker = Worker {
        session,
        jobs: HashMap::new(),
        pending: HashSet::new(),
        tx,
    };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let code = loop {
        let stop = tokio::select! {
            _ = tick.tick() => {
                worker.progress();
                None
            }
            msg = rx.recv() => worker.on_msg(msg).await,
        };
        if let Some(code) = stop {
            break code;
        }
    };
    worker.session.stop().await;
    Ok(code)
}

/// DHT on, LSD off, UPnP off, no listening port, no trackers or blocklists
/// beyond those in each torrent, and an upload cap by default.
fn session_options() -> SessionOptions {
    SessionOptions {
        dht: Some(DhtSessionConfig {
            bootstrap_addrs: None,
            port: None,
            persistence: None,
        }),
        disable_local_service_discovery: true,
        ratelimits: LimitsConfig {
            upload_bps: NonZeroU32::new(DEFAULT_UP_BPS),
            download_bps: None,
        },
        ..Default::default()
    }
}

/// Reads stdin on a plain thread, so blocking reads never stall the runtime.
fn spawn_stdin_reader(tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        let mut stdin = BufReader::new(std::io::stdin().lock());
        loop {
            let mut buf = Vec::new();
            let msg = match (&mut stdin).take(READ_LIMIT).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => Msg::Eof,
                Ok(_) if buf.len() as u64 >= READ_LIMIT && buf.last() != Some(&b'\n') => {
                    Msg::TooLong
                }
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf);
                    let line = text.trim_end_matches(['\r', '\n']);
                    if line.len() > MAX_LINE {
                        Msg::TooLong
                    } else {
                        Msg::Line(line.to_owned())
                    }
                }
            };
            let last = !matches!(msg, Msg::Line(_));
            if tx.blocking_send(msg).is_err() || last {
                return;
            }
        }
    });
}

fn emit(event: &Event) {
    let line = encode_line(event);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// Error messages are fixed text. Details go to stderr only.
fn emit_error(id: Option<&str>, message: &str) {
    emit(&Event::Error {
        id: id.map(str::to_owned),
        message: message.to_owned(),
    });
}

/// Upload and download caps in bytes per second. `None` means no cap. A cap of
/// zero becomes one byte per second, since a limiter cannot be zero.
fn to_bps(v: Option<u64>) -> Option<NonZeroU32> {
    v.map(|b| {
        NonZeroU32::new(u32::try_from(b).unwrap_or(u32::MAX).max(1)).unwrap_or(NonZeroU32::MIN)
    })
}

/// Validates the source, then adds it. Magnet resolution can take minutes, so
/// this runs on its own task.
async fn add_source(
    session: &Arc<Session>,
    source: Source,
    opts: AddTorrentOptions,
) -> Result<Arc<ManagedTorrent>, String> {
    let add = match source {
        Source::Magnet(magnet) => {
            if !magnet.starts_with("magnet:?") {
                return Err("not a magnet link".into());
            }
            AddTorrent::from_url(magnet)
        }
        Source::TorrentPath(path) => {
            let path = Path::new(&path);
            if !path.is_absolute() {
                return Err("torrent_path must be absolute".into());
            }
            let meta = tokio::fs::metadata(path)
                .await
                .map_err(|_| "torrent file not found")?;
            if !meta.is_file() || meta.len() > MAX_TORRENT_BYTES {
                return Err("torrent file refused".into());
            }
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|_| "torrent file unreadable")?;
            AddTorrent::from_bytes(bytes)
        }
    };
    let response = session
        .add_torrent(add, Some(opts))
        .await
        .map_err(|e| {
            eprintln!("add failed: {e:#}");
            "could not add torrent".to_owned()
        })?;
    match response {
        AddTorrentResponse::Added(_, handle) => Ok(handle),
        _ => Err("torrent is already in the session".into()),
    }
}

impl Worker {
    /// Returns `Some(code)` when the loop should stop.
    async fn on_msg(&mut self, msg: Option<Msg>) -> Option<ExitCode> {
        match msg {
            Some(Msg::Line(line)) => self.on_line(&line).await,
            Some(Msg::Added(id, result)) => {
                self.on_added(id, result);
                None
            }
            Some(Msg::Completed(id)) => {
                self.on_completed(&id).await;
                None
            }
            Some(Msg::Failed(id)) => {
                emit_error(Some(id.as_str()), "torrent failed");
                None
            }
            Some(Msg::TooLong) => {
                emit_error(None, "command line too long");
                Some(ExitCode::FAILURE)
            }
            Some(Msg::Eof) | None => Some(ExitCode::SUCCESS),
        }
    }

    async fn on_line(&mut self, line: &str) -> Option<ExitCode> {
        match decode_line::<Command>(line) {
            Ok(cmd) => self.apply(cmd).await,
            Err(e) => {
                eprintln!("refused command line: {e:?}");
                emit_error(None, "bad command");
                None
            }
        }
    }

    async fn apply(&mut self, cmd: Command) -> Option<ExitCode> {
        match cmd {
            Command::Add {
                id,
                source,
                save_dir,
            } => self.add(id, source, save_dir),
            Command::Pause { id } => self.pause(&id).await,
            Command::Resume { id } => self.resume(&id).await,
            Command::Remove { id } => self.remove(&id).await,
            Command::SetLimits { down_bps, up_bps } => {
                self.session.ratelimits.set_download_bps(to_bps(down_bps));
                self.session.ratelimits.set_upload_bps(to_bps(up_bps));
            }
            Command::Shutdown => return Some(ExitCode::SUCCESS),
        }
        None
    }

    fn add(&mut self, id: String, source: Source, save_dir: String) {
        if self.jobs.contains_key(&id) || self.pending.contains(&id) {
            emit_error(Some(id.as_str()), "id already in use");
            return;
        }
        if !Path::new(&save_dir).is_absolute() {
            emit_error(Some(id.as_str()), "save_dir must be an absolute path");
            return;
        }
        self.pending.insert(id.clone());
        let session = self.session.clone();
        let tx = self.tx.clone();
        let opts = AddTorrentOptions {
            output_folder: Some(save_dir),
            ..Default::default()
        };
        tokio::spawn(async move {
            let result = add_source(&session, source, opts).await;
            let _ = tx.send(Msg::Added(id, result)).await;
        });
    }

    fn on_added(&mut self, id: String, result: Result<Arc<ManagedTorrent>, String>) {
        self.pending.remove(&id);
        match result {
            Ok(torrent) => {
                let watcher = self.watch(id.clone(), torrent.clone());
                self.jobs.insert(
                    id,
                    Job {
                        torrent,
                        finished: false,
                        watcher,
                    },
                );
            }
            Err(message) => emit_error(Some(id.as_str()), &message),
        }
    }

    /// Reports completion back to the main loop, which then pauses the torrent
    /// so it never seeds.
    fn watch(&self, id: String, torrent: Arc<ManagedTorrent>) -> JoinHandle<()> {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let msg = match torrent.wait_until_completed().await {
                Ok(()) => Msg::Completed(id),
                Err(_) => Msg::Failed(id),
            };
            let _ = tx.send(msg).await;
        })
    }

    async fn on_completed(&mut self, id: &str) {
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        job.finished = true;
        let torrent = job.torrent.clone();
        if let Err(e) = self.session.pause(&torrent).await {
            eprintln!("pause after finish failed: {e:#}");
            emit_error(Some(id), "could not pause finished torrent");
        }
        emit(&Event::Finished { id: id.to_owned() });
    }

    async fn pause(&mut self, id: &str) {
        let Some(job) = self.jobs.get(id) else {
            emit_error(Some(id), "unknown torrent");
            return;
        };
        if job.finished {
            return;
        }
        let torrent = job.torrent.clone();
        if let Err(e) = self.session.pause(&torrent).await {
            eprintln!("pause failed: {e:#}");
            emit_error(Some(id), "could not pause");
        }
    }

    async fn resume(&mut self, id: &str) {
        let Some(job) = self.jobs.get(id) else {
            emit_error(Some(id), "unknown torrent");
            return;
        };
        if job.finished {
            // Never resume a finished torrent: it would seed.
            emit(&Event::Finished { id: id.to_owned() });
            return;
        }
        let torrent = job.torrent.clone();
        if let Err(e) = self.session.unpause(&torrent).await {
            eprintln!("resume failed: {e:#}");
            emit_error(Some(id), "could not resume");
        }
    }

    async fn remove(&mut self, id: &str) {
        let Some(job) = self.jobs.remove(id) else {
            emit_error(Some(id), "unknown torrent");
            return;
        };
        job.watcher.abort();
        // delete_files = false: downloaded data stays on disk.
        if let Err(e) = self
            .session
            .delete(TorrentIdOrHash::Id(job.torrent.id()), false)
            .await
        {
            eprintln!("remove failed: {e:#}");
            emit_error(Some(id), "could not remove");
        }
    }

    fn progress(&self) {
        for (id, job) in &self.jobs {
            let stats = job.torrent.stats();
            let (peers, down_bps, up_bps) = match &stats.live {
                Some(live) => (
                    live.snapshot.peer_stats.live,
                    live.download_speed.as_bytes(),
                    live.upload_speed.as_bytes(),
                ),
                None => (0, 0, 0),
            };
            emit(&Event::Progress {
                id: id.clone(),
                bytes_done: stats.progress_bytes,
                bytes_total: stats.total_bytes,
                peers,
                down_bps,
                up_bps,
            });
        }
    }
}
