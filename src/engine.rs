//! Segmented HTTP download engine.
//!
//! Probe with a one-byte range, split into byte ranges, write each range at its
//! offset, and keep a resumable state file next to the download.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;

/// Splitting a range with less than this left is not worth another connection.
///
/// About 23 KB: below that a new connection's handshake costs more than the
/// bytes it would take over, so splitting only thrashes.
const MIN_SPLIT_BYTES: u64 = 0x5b43;

/// How long a new connection must have been running, and receiving, before the
/// pool is allowed to grow again, so a slow link is not flooded with
/// connections.
const RAMP_INTERVAL: Duration = Duration::from_secs(2);

/// Attempts per segment when the caller sets no budget of its own.
const MAX_ATTEMPTS: u32 = 3;

/// Socket timeout when the caller sets none. Matches the Options page's own
/// default rather than reqwest's (which is no timeout at all).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("state file error: {0}")]
    State(#[from] serde_json::Error),
    #[error("server answered HTTP {0}")]
    Status(u16),
    #[error("server ignored the byte-range request")]
    RangeIgnored,
    #[error("server returned the wrong byte range: asked for {want}, got {got}")]
    RangeMismatch { want: String, got: String },
    #[error("connection ended early after {got} of {expected} bytes")]
    ShortRead { got: u64, expected: u64 },
    #[error("not a usable URL: {0}")]
    BadUrl(String),
    #[error("ftp: {0}")]
    Ftp(#[from] crate::ftp::FtpError),
    #[error("gave up after {0} redirects")]
    TooManyRedirects(usize),
    #[error("streaming manifest: {0}")]
    Media(#[from] crate::media::Error),
    #[error(
        "the video and audio tracks downloaded separately to {video} and {audio}; \
         joining them needs ffmpeg on PATH, which was not found"
    )]
    NeedsMux { video: String, audio: String },
}

/// How many hops we follow before giving up.
///
/// Ten is the conventional browser limit and is plenty for real downloads.
const MAX_REDIRECTS: usize = 10;

/// Whether two hosts are close enough to keep sending credentials to.
///
/// Equal hosts, or one a dot-anchored subdomain of the other. The anchor
/// matters: with a plain substring test,
/// `evil-example.com` would pass as same-site against `example.com` and its
/// cookies follow the redirect to the attacker. Requiring the leading dot
/// rejects that.
///
/// Limitation, stated plainly: without a public-suffix list this still treats
/// `a.co.uk` and `co.uk` as related. That direction only ever keeps a cookie
/// on a *narrower* host than the one that set it, so it does not leak across
/// registrable domains, which is the case that matters here.
fn same_site(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let (a, b) = (a.trim_end_matches('.'), b.trim_end_matches('.'));
            a.eq_ignore_ascii_case(b)
                || a.to_ascii_lowercase().ends_with(&format!(".{}", b.to_ascii_lowercase()))
                || b.to_ascii_lowercase().ends_with(&format!(".{}", a.to_ascii_lowercase()))
        }
        _ => false,
    }
}

/// Headers that must not follow a redirect to an unrelated host.
fn is_cross_site_sensitive(name: &str) -> bool {
    name.eq_ignore_ascii_case("cookie")
        || name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("referer")
        || name.eq_ignore_ascii_case("proxy-authorization")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Queued,
    Running,
    Paused,
    Finished,
    Failed,
}

/// What the UI reads on each tick.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub status: Status,
    pub total: Option<u64>,
    pub done: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Segment {
    /// First byte of the range (inclusive).
    start: u64,
    /// Last byte of the range (inclusive). `u64::MAX` for an unknown-size stream.
    end: u64,
    /// Bytes already written for this range.
    done: u64,
    complete: bool,
}

impl Segment {
    fn resume_offset(&self) -> u64 {
        self.start + self.done
    }
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    url: String,
    total: Option<u64>,
    ranged: bool,
    segments: Vec<Segment>,
}

/// Caps the average transfer rate.
///
/// Share one of these between several [`Download`]s to cap their *combined*
/// throughput — which is what a user setting a speed limit means. A limiter
/// owned by a single download would let N concurrent downloads run at N times
/// the configured rate.
#[derive(Debug)]
pub struct Limiter {
    /// Bytes per second; 0 means unlimited.
    limit: AtomicU64,
    /// The measurement window, reset whenever the limit changes.
    window: Mutex<Window>,
}

#[derive(Debug)]
struct Window {
    started: Instant,
    sent: u64,
}

impl Limiter {
    /// `bytes_per_second` of `None` means unlimited.
    pub fn new(bytes_per_second: Option<u64>) -> Self {
        Self {
            limit: AtomicU64::new(bytes_per_second.unwrap_or(0)),
            window: Mutex::new(Window { started: Instant::now(), sent: 0 }),
        }
    }

    /// Change the cap, restarting the measurement window.
    ///
    /// The rebase is essential. `sent` accumulates whether or not a limit is
    /// set, so without it, turning a limit on after an unlimited gigabyte
    /// would compare that gigabyte against the new rate and park every worker
    /// for however long that implies — minutes or hours, with nothing shown
    /// in the UI to explain it.
    pub fn set(&self, bytes_per_second: Option<u64>) {
        let mut window = self.window.lock().expect("limiter window");
        window.started = Instant::now();
        window.sent = 0;
        self.limit.store(bytes_per_second.unwrap_or(0), Ordering::SeqCst);
    }

    pub fn get(&self) -> Option<u64> {
        match self.limit.load(Ordering::SeqCst) {
            0 => None,
            n => Some(n),
        }
    }

    /// Record `bytes` just written and wait until the average rate is back
    /// under the limit.
    pub(crate) async fn account(&self, bytes: u64) {
        let limit = self.limit.load(Ordering::SeqCst);
        let wait = {
            let mut window = self.window.lock().expect("limiter window");
            window.sent = window.sent.saturating_add(bytes);
            if limit == 0 {
                // Unlimited: keep the window fresh so switching a limit on
                // later starts measuring from now, not from the whole session.
                window.started = Instant::now();
                window.sent = 0;
                None
            } else {
                let due = Duration::from_secs_f64(window.sent as f64 / limit as f64);
                let elapsed = window.started.elapsed();
                (due > elapsed).then(|| due - elapsed)
            }
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
    }
}

/// Proxy to route requests through.
///
/// `http` and `https` are separate because the Proxy settings have a tick per
/// protocol. FTP is not here: our FTP is raw sockets, which a plain
/// HTTP proxy cannot carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Scheme, host and port of the proxy itself, e.g. `http://proxy.lan:3128`.
    pub url: String,
    pub http: bool,
    pub https: bool,
    /// Sent as Basic credentials. Empty `user` means no authentication.
    pub user: String,
    pub password: String,
}

/// Handle for one download. Cheap to clone; clones share progress and the stop flag.
#[derive(Clone)]
pub struct Download {
    url: String,
    dest: PathBuf,
    max_segments: u32,
    stop: Arc<AtomicBool>,
    /// One-way latch set by [`Download::stop_and_discard`]: once the file is
    /// being deleted, a `run` that has not started yet must not come back to
    /// life and re-create it.
    discarded: Arc<AtomicBool>,
    shared: Arc<Mutex<Snapshot>>,
    rate: Arc<Limiter>,
    headers: Arc<Mutex<Vec<(String, String)>>>,
    proxy: Option<ProxyConfig>,
    /// Attempts per segment, and the per-request socket timeout. Both come
    /// from the Connection page.
    attempts: u32,
    timeout: Duration,
    /// Alternative URLs for the same file, tried in order when a segment keeps
    /// failing on the current one. The primary URL is always tried first.
    mirrors: Vec<String>,
}

impl Download {
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>, max_segments: u32) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            max_segments: max_segments.max(1),
            stop: Arc::new(AtomicBool::new(false)),
            discarded: Arc::new(AtomicBool::new(false)),
            shared: Arc::new(Mutex::new(Snapshot::default())),
            rate: Arc::new(Limiter::new(None)),
            headers: Arc::new(Mutex::new(Vec::new())),
            proxy: None,
            attempts: MAX_ATTEMPTS,
            timeout: DEFAULT_TIMEOUT,
            mirrors: Vec::new(),
        }
    }

    /// Mirror URLs for the same file. When a segment fails on one URL, its
    /// retries move on to the next, so a dead or slow CDN node does not stall
    /// the whole download. Mirrors are not probed, so the file is still
    /// checked against the primary's size and, if set, the SHA-256 at the end.
    pub fn set_mirrors(&mut self, mirrors: Vec<String>) {
        self.mirrors = mirrors
            .into_iter()
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty() && *m != self.url)
            .collect();
    }

    /// The mirror URLs set with [`Download::set_mirrors`].
    pub fn mirrors(&self) -> &[String] {
        &self.mirrors
    }

    /// Retries per segment and the socket timeout, as set on the Connection
    /// page. `retries` is the number of *extra* tries after the first, so 0
    /// means one attempt and no retry.
    pub fn set_limits(&mut self, retries: u32, timeout_secs: u32) {
        self.attempts = retries.saturating_add(1);
        self.timeout = Duration::from_secs(u64::from(timeout_secs.max(1)));
    }

    /// Route this download through a proxy. Takes effect on the next `run`,
    /// since the HTTP client is built there.
    pub fn set_proxy(&mut self, proxy: Option<ProxyConfig>) {
        self.proxy = proxy;
    }

    /// Share a limiter with other downloads so the cap applies to their
    /// combined rate rather than to each one separately.
    pub fn set_limiter(&mut self, limiter: Arc<Limiter>) {
        self.rate = limiter;
    }

    /// Add or replace a request header sent with the probe and every segment request.
    /// Typical uses are `Referer`, `Cookie`, and `User-Agent`.
    pub fn set_header(&self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let mut headers = self.headers.lock().expect("headers lock");
        headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
        headers.push((name, value.into()));
    }

    /// Limit the transfer to `bytes_per_second` on average. `None` removes the limit.
    /// Takes effect immediately for a running download.
    pub fn set_speed_limit(&self, bytes_per_second: Option<u64>) {
        self.rate.set(bytes_per_second);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().expect("snapshot lock").clone()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn dest(&self) -> &Path {
        &self.dest
    }

    /// Delete the partial file and its resume sidecar.
    ///
    /// Removing a download from the list must not leave its bytes behind —
    /// a half-finished multi-gigabyte file that nothing references again is
    /// invisible waste. Missing files are not an error.
    pub async fn discard_partial(&self) -> Result<(), Error> {
        for path in [self.state_path(), self.dest.clone()] {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Ask running tasks to stop after their current chunk. Progress is saved.
    pub fn pause(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stop the download, wait for its workers to let go of the file, then
    /// delete it.
    ///
    /// Deleting while a worker still has the file open fails outright on
    /// Windows, and a worker that checkpoints after the delete resurrects the
    /// sidecar it just removed — so pausing and deleting in the same breath
    /// loses the race about as often as it wins it. The latch closes the other
    /// half of the window: a `run` already spawned but not yet started cannot
    /// re-create the file behind us.
    pub async fn stop_and_discard(&self) -> Result<(), Error> {
        self.discarded.store(true, Ordering::SeqCst);
        self.pause();
        // Bounded, because a worker wedged in a socket read would otherwise
        // leave the file undeletable forever. Past the deadline we try anyway
        // and report whatever the filesystem says.
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.snapshot().status == Status::Running && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.discard_partial().await
    }

    fn set_status(&self, status: Status) {
        self.shared.lock().expect("snapshot lock").status = status;
    }

    fn state_path(&self) -> PathBuf {
        with_suffix(&self.dest, ".tdmstate")
    }

    /// Run until finished, paused, or failed. Resumes from the state file if present.
    pub async fn run(&self) -> Result<(), Error> {
        // Removed while this run was queued: do nothing rather than re-create
        // the file that `stop_and_discard` is deleting.
        if self.discarded.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.stop.store(false, Ordering::SeqCst);
        self.set_status(Status::Running);
        let result = self.run_inner().await;
        match &result {
            Ok(()) if self.stop.load(Ordering::SeqCst) => self.set_status(Status::Paused),
            Ok(()) => self.set_status(Status::Finished),
            Err(e) => {
                let mut s = self.shared.lock().expect("snapshot lock");
                s.status = Status::Failed;
                s.error = Some(e.to_string());
            }
        }
        result
    }

    async fn run_inner(&self) -> Result<(), Error> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("TDM/", env!("CARGO_PKG_VERSION")))
            // Applies to the whole request including the body, so it bounds a
            // stalled transfer, not just a slow handshake.
            .timeout(self.timeout)
            // We follow redirects ourselves in `probe` so that credentials can
            // be dropped on a cross-site hop; see `same_site`.
            .redirect(reqwest::redirect::Policy::none());
        if let Some(p) = &self.proxy {
            let wanted = [
                p.http.then(|| reqwest::Proxy::http(&p.url)),
                p.https.then(|| reqwest::Proxy::https(&p.url)),
            ];
            for proxy in wanted.into_iter().flatten() {
                let mut proxy = proxy?;
                if !p.user.is_empty() {
                    proxy = proxy.basic_auth(&p.user, &p.password);
                }
                builder = builder.proxy(proxy);
            }
        }
        let client = builder.build()?;

        let configured = self.headers.lock().expect("headers lock").clone();

        // A streaming manifest is not a file to range over: it is a list of
        // other files. Detected from the URL only — a content-type-only manifest would need a fetch before
        // we know, and that fetch is exactly what `probe` is for.
        if crate::media::detect(&self.url, None, &[]).is_some() {
            return self.run_media(&client, &configured).await;
        }

        let saved = self.load_state().await?;

        // FTP takes a different probe and a different fetch, but the same
        // segment planner: `REST` gives us a start offset, and the segment end
        // is enforced by closing the data socket, so a range is a range.
        let is_ftp = self.url.len() >= 6 && self.url[..6].eq_ignore_ascii_case("ftp://");

        // Probe on every run, including a resume. It resolves the redirect
        // chain and re-confirms the size and range support. Resuming against a
        // file that changed on the server would otherwise write new bytes into
        // old segment offsets and produce a corrupt result that still looks
        // complete.
        let (total, ranged, headers, fetch_url) = if is_ftp {
            let ftp = crate::ftp::FtpDownload::parse(&self.url)?;
            let p = ftp.probe().await?;
            // Without REST the server cannot resume, so the file is one stream.
            (p.size, p.supports_rest && p.size.is_some(), Vec::new(), self.url.clone())
        } else {
            let p = probe(&client, &self.url, &configured).await?;
            (p.total, p.ranged, p.headers, p.url)
        };

        // A saved state that no longer matches the server's answer cannot be
        // trusted: start the file again.
        let resumed = saved.filter(|s| s.ranged == ranged && s.total == total);
        let fresh = resumed.is_none();
        let segments = match resumed {
            Some(s) => s.segments,
            None => plan_segments(total, ranged),
        };
        self.shared.lock().expect("snapshot lock").total = total;

        // The destination may sit in a category folder that does not exist
        // yet. Without this, every categorised download fails with "cannot
        // find the file specified" on a machine where the folder was never
        // created.
        if let Some(parent) = self.dest.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            // A fresh start must not keep an older, longer file's tail: with
            // no known size there is no `set_len` to cut it off.
            .truncate(fresh)
            .open(&self.dest)
            .await?;
        if let Some(size) = total {
            file.set_len(size).await?;
        }
        drop(file);

        let segments = Arc::new(Mutex::new(segments));
        let mut tasks = JoinSet::new();
        let spawn_worker = |tasks: &mut JoinSet<_>, index: usize| {
            tasks.spawn(
                Worker {
                    client: client.clone(),
                    urls: std::iter::once(fetch_url.clone())
                        .chain(self.mirrors.iter().cloned())
                        .collect(),
                    path: self.dest.clone(),
                    index,
                    ranged,
                    segments: Arc::clone(&segments),
                    stop: Arc::clone(&self.stop),
                    rate: Arc::clone(&self.rate),
                    headers: headers.clone(),
                    total,
                    is_ftp,
                    attempts: self.attempts,
                }
                .run(),
            );
        };

        // Every segment that still has work gets a connection. On a fresh
        // ranged download that is exactly one; on a resume it is however many
        // the previous run had split into.
        let initial: Vec<usize> = {
            let segs = segments.lock().expect("segments lock");
            (0..segs.len()).filter(|i| !segs[*i].complete).collect()
        };
        for index in &initial {
            spawn_worker(&mut tasks, *index);
        }
        let mut live = initial.len();

        let mut tick = tokio::time::interval(TICK);
        let mut first_error: Option<Error> = None;
        let mut last_ramp = Instant::now();
        let mut last_total_done = 0u64;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // A failed checkpoint must not abort a healthy transfer —
                    // the worst case is resuming from an older position, which
                    // is strictly better than losing the download.
                    let done_now = match self.publish(&segments, total, ranged).await {
                        Ok(done) => done,
                        Err(e) => {
                            eprintln!("could not checkpoint progress: {e}");
                            segments.lock().expect("segments lock").iter().map(|s| s.done).sum()
                        }
                    };

                    // Grow the pool only while bytes are actually arriving, and
                    // never faster than one step per RAMP_INTERVAL. A stalled or
                    // very slow link gains nothing from more sockets.
                    let progressing = done_now > last_total_done;
                    last_total_done = done_now;
                    let room = (self.max_segments as usize).saturating_sub(live);
                    if ranged && ! self.stop.load(Ordering::SeqCst)
                        && progressing
                        && room > 0
                        && last_ramp.elapsed() >= RAMP_INTERVAL
                    {
                        // Double the pool (1, 2, 4, 8 ...).
                        let want = live.min(room);
                        let mut added = 0;
                        for _ in 0..want {
                            let new_index = {
                                let mut segs = segments.lock().expect("segments lock");
                                split_largest(&mut segs)
                            };
                            match new_index {
                                Some(i) => { spawn_worker(&mut tasks, i); added += 1; }
                                None => break,
                            }
                        }
                        if added > 0 {
                            live += added;
                            last_ramp = Instant::now();
                        }
                    }
                }
                joined = tasks.join_next() => match joined {
                    None => break,
                    Some(Ok(Ok(()))) => {
                        live = live.saturating_sub(1);
                        // A connection that finished early takes the back half
                        // of whatever range has the most left to do.
                        if ranged && !self.stop.load(Ordering::SeqCst) {
                            let new_index = {
                                let mut segs = segments.lock().expect("segments lock");
                                split_largest(&mut segs)
                            };
                            if let Some(i) = new_index {
                                spawn_worker(&mut tasks, i);
                                live += 1;
                            }
                        }
                    }
                    Some(Ok(Err(e))) => {
                        live = live.saturating_sub(1);
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                        self.stop.store(true, Ordering::SeqCst);
                    }
                    Some(Err(join_err)) => {
                        live = live.saturating_sub(1);
                        // A panicked worker is treated as a failed download, not a silent success.
                        first_error.get_or_insert(Error::Io(std::io::Error::other(join_err)));
                        self.stop.store(true, Ordering::SeqCst);
                    }
                },
            }
        }

        self.publish(&segments, total, ranged).await?;
        if let Some(e) = first_error {
            return Err(e);
        }
        let all_done = segments.lock().expect("segments lock").iter().all(|s| s.complete);
        if all_done {
            let _ = tokio::fs::remove_file(self.state_path()).await;
        }
        Ok(())
    }

    /// Download a streaming manifest: resolve it to a flat segment list, then
    /// append the segments in order.
    ///
    /// One connection, in playback order — not because it is faster but
    /// because the output is a concatenation, so the bytes have to arrive in
    /// sequence to be written in sequence.
    ///
    /// shortcut: a paused media download restarts from the first segment,
    /// since there is no sidecar for a segment list. Upgrade when a resumable
    /// media transfer is worth a second state format.
    async fn run_media(
        &self,
        client: &reqwest::Client,
        headers: &[(String, String)],
    ) -> Result<(), Error> {
        let jobs = crate::media::resolve(client, &self.url, headers, &self.dest).await?;
        if let Some(parent) = self.dest.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        // The segment count is known; the byte total is not, so progress is
        // reported against the bytes that have actually arrived.
        let mut done = 0u64;
        for job in &jobs {
            let mut file = tokio::fs::File::create(&job.output).await?;
            for segment in &job.segments {
                if self.stop.load(Ordering::SeqCst) {
                    file.flush().await?;
                    return Ok(());
                }
                let mut request = client.get(segment.url.clone());
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                let response = request.send().await?;
                let status = response.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(Error::Status(status));
                }
                let bytes = response.bytes().await?;
                file.write_all(&bytes).await?;
                done = done.saturating_add(bytes.len() as u64);
                self.rate.account(bytes.len() as u64).await;
                let mut snapshot = self.shared.lock().expect("snapshot lock");
                snapshot.done = done;
                snapshot.total = None;
            }
            file.flush().await?;
        }

        // DASH keeps audio and video apart. Join them if ffmpeg is there;
        // otherwise say so and leave both files, rather than present a silent
        // video as finished.
        if jobs.len() > 1 && jobs.iter().any(|j| j.needs_mux) {
            let (video, audio) = (&jobs[0].output, &jobs[1].output);
            let status = std::process::Command::new("ffmpeg")
                .args(["-y", "-i"])
                .arg(video)
                .arg("-i")
                .arg(audio)
                .args(["-c", "copy"])
                .arg(&self.dest)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {
                    let _ = tokio::fs::remove_file(video).await;
                    let _ = tokio::fs::remove_file(audio).await;
                }
                _ => {
                    return Err(Error::NeedsMux {
                        video: video.display().to_string(),
                        audio: audio.display().to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Copy segment progress into the shared snapshot and checkpoint it to
    /// disk. Returns the total bytes fetched so far.
    async fn publish(
        &self,
        segments: &Arc<Mutex<Vec<Segment>>>,
        total: Option<u64>,
        ranged: bool,
    ) -> Result<u64, Error> {
        let snapshot_segments = segments.lock().expect("segments lock").clone();
        let done: u64 = snapshot_segments.iter().map(|s| s.done).sum();
        self.shared.lock().expect("snapshot lock").done = done;

        let state = StateFile {
            url: self.url.clone(),
            total,
            ranged,
            segments: snapshot_segments,
        };
        let json = serde_json::to_vec_pretty(&state)?;
        let tmp = with_suffix(&self.state_path(), ".tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, self.state_path()).await?;
        Ok(done)
    }

    async fn load_state(&self) -> Result<Option<StateFile>, Error> {
        let bytes = match tokio::fs::read(self.state_path()).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let saved: StateFile = serde_json::from_slice(&bytes)?;
        // A state file for a different URL must not resume into this file.
        if saved.url != self.url {
            return Ok(None);
        }
        Ok(Some(saved))
    }
}

struct Probe {
    total: Option<u64>,
    ranged: bool,
    /// The URL the chain actually ended at. Segment requests go here, so a
    /// redirect is resolved once instead of on every connection.
    url: String,
    /// The headers still safe to send, after any cross-site hop stripped the
    /// sensitive ones.
    headers: Vec<(String, String)>,
}

/// Ask for bytes 0-0 to learn the size, whether byte ranges work, and where
/// the URL finally lands.
///
/// Redirects are followed here rather than by the HTTP client so that we
/// control what happens to credentials when a hop crosses to another host.
async fn probe(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<Probe, Error> {
    let mut current =
        reqwest::Url::parse(url).map_err(|_| Error::BadUrl(url.to_owned()))?;
    let mut headers = headers.to_vec();

    for _ in 0..=MAX_REDIRECTS {
        let mut request = client.get(current.clone()).header("Range", "bytes=0-0");
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let resp = request.send().await?;
        let status = resp.status().as_u16();

        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = resp
                .headers()
                .get("Location")
                .and_then(|v| v.to_str().ok())
                .ok_or(Error::Status(status))?;
            // Relative locations resolve against the URL we just fetched.
            let next = current
                .join(location)
                .map_err(|_| Error::BadUrl(location.to_owned()))?;
            if !same_site(current.host_str(), next.host_str()) {
                headers.retain(|(name, _)| !is_cross_site_sensitive(name));
            }
            current = next;
            continue;
        }

        return match status {
            206 => {
                let total = resp
                    .headers()
                    .get("Content-Range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range_total);
                // A 206 whose total is `*` leaves us no end to plan segments
                // against; treating it as ranged would build a single
                // open-ended segment and then fail its own end check on every
                // response. Fall back to one plain stream.
                Ok(Probe {
                    total,
                    ranged: total.is_some(),
                    url: current.to_string(),
                    headers,
                })
            }
            200 => Ok(Probe {
                total: resp.content_length(),
                ranged: false,
                url: current.to_string(),
                headers,
            }),
            other => Err(Error::Status(other)),
        };
    }
    Err(Error::TooManyRedirects(MAX_REDIRECTS))
}

/// Parse the total from `bytes 0-0/12345`.
fn parse_content_range_total(value: &str) -> Option<u64> {
    parse_content_range(value).and_then(|(_, _, total)| total)
}

/// Parse a `Content-Range: bytes <start>-<end>/<total>` header.
///
/// `total` is `None` when the server sends `*`. Returns `None` if the header
/// is not a byte range we understand — which the caller must treat as a
/// failure rather than as "no constraint", because writing a segment to the
/// wrong offset silently corrupts the output file.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start, end, total))
}

/// The starting plan: one range covering the whole file.
///
/// We do not divide a file up front: open one connection and grow the pool
/// only while transfers are actually progressing, handing each new connection
/// the back half of whatever range has the most left. Splitting eagerly into N equal parts
/// means one slow part holds up the whole download; splitting on demand means
/// a connection that finishes early takes work off the slowest one.
fn plan_segments(total: Option<u64>, ranged: bool) -> Vec<Segment> {
    match total {
        Some(0) => vec![Segment { start: 0, end: 0, done: 0, complete: true }],
        Some(size) if ranged && size > 0 => {
            vec![Segment { start: 0, end: size - 1, done: 0, complete: false }]
        }
        _ => vec![Segment { start: 0, end: u64::MAX, done: 0, complete: false }],
    }
}

/// Bytes still to fetch for a segment.
fn remaining(seg: &Segment) -> u64 {
    if seg.complete || seg.end == u64::MAX {
        return 0;
    }
    seg.end
        .saturating_add(1)
        .saturating_sub(seg.start.saturating_add(seg.done))
}

/// The furthest byte a worker may already have in flight for this segment.
///
/// A worker computes how much of a chunk it may keep, releases the segment
/// lock to write, then re-takes the lock to record it. A split landing inside
/// that window hands away bytes the worker is about to write, so both
/// connections fetch them — wasted transfer rather than a wrong file, since
/// both write identical bytes to identical offsets, but worth avoiding.
///
/// The margin scales with what is left: a fixed one large enough for a big
/// transfer would block every legitimate split near the 23 KiB floor.
fn safe_split_floor(seg: &Segment) -> u64 {
    let margin = (remaining(seg) / 4).min(64 * 1024);
    seg.start.saturating_add(seg.done).saturating_add(margin)
}

/// Hand the back half of the neediest range to a new connection.
///
/// Returns the index of the new segment, or `None` when nothing has enough
/// left to be worth splitting. The victim keeps the front half and stops early
/// once it reaches its new end.
fn split_largest(segments: &mut Vec<Segment>) -> Option<usize> {
    let (victim, left) = segments
        .iter()
        .enumerate()
        .map(|(i, s)| (i, remaining(s)))
        .max_by_key(|(_, left)| *left)?;
    if left < MIN_SPLIT_BYTES {
        return None;
    }
    let end = segments[victim].end;
    let new_start = end - left / 2;
    // Guard the degenerate case where rounding would produce an empty or
    // backwards range, and stay clear of bytes the victim may already have in
    // flight — see `safe_split_floor`.
    if new_start <= safe_split_floor(&segments[victim]) || new_start > end {
        return None;
    }
    segments[victim].end = new_start - 1;
    segments.push(Segment { start: new_start, end, done: 0, complete: false });
    Some(segments.len() - 1)
}

struct Worker {
    client: reqwest::Client,
    /// The primary URL first, then any mirrors. A transient failure moves the
    /// worker to the next entry before it retries.
    urls: Vec<String>,
    path: PathBuf,
    index: usize,
    ranged: bool,
    segments: Arc<Mutex<Vec<Segment>>>,
    stop: Arc<AtomicBool>,
    rate: Arc<Limiter>,
    headers: Vec<(String, String)>,
    /// Total size, when the probe established one, so a segment response can
    /// be checked against it.
    total: Option<u64>,
    is_ftp: bool,
    attempts: u32,
}

impl Worker {
    /// Fetch one segment, retrying from the last written byte on transient errors.
    async fn run(self) -> Result<(), Error> {
        let mut attempt = 0;
        // Index into `urls`. Starts at the primary and advances on each
        // transient failure, wrapping around the mirror list.
        let mut current = 0;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let url = self.urls[current].clone();
            match self.fetch_once(&url).await {
                Ok(()) => return Ok(()),
                // A server that ignores Range, or answers with the wrong one,
                // will not start honouring it on retry. Retrying would just
                // write the same wrong bytes again, so fail loudly instead.
                Err(e @ (Error::RangeIgnored | Error::RangeMismatch { .. })) => return Err(e),
                // 4xx other than 408/429 is a permanent client error; retrying
                // a 404 only wastes time. Everything else — including a 503 or
                // a rate limit — is worth another attempt.
                Err(Error::Status(code))
                    if (400..500).contains(&code) && code != 408 && code != 429 =>
                {
                    return Err(Error::Status(code));
                }
                Err(e) => {
                    attempt += 1;
                    if attempt >= self.attempts {
                        return Err(e);
                    }
                    // Switch to the next mirror before retrying, so a bad node
                    // is not hit again on every attempt.
                    current = (current + 1) % self.urls.len();
                    tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
                }
            }
        }
    }

    /// Fetch this segment over FTP.
    ///
    /// The shape matches the HTTP path: write at the true offset, honour the
    /// shared speed limit, stop early if the coordinator takes our tail.
    async fn fetch_ftp(&self, url: &str, segment: &Segment, from: u64) -> Result<(), Error> {
        let ftp = crate::ftp::FtpDownload::parse(url)?;
        let mut file = OpenOptions::new().write(true).open(&self.path).await?;
        file.seek(std::io::SeekFrom::Start(from)).await?;

        let end = if self.ranged { Some(segment.end) } else { None };
        let segments = Arc::clone(&self.segments);
        let stop = Arc::clone(&self.stop);
        let index = self.index;

        ftp.fetch_range(from, end, &mut file, Some(&self.rate), move |written| {
            let mut segs = segments.lock().expect("segments lock");
            let seg = &mut segs[index];
            seg.done += written;
            // Keep going only while we still own the bytes ahead of us.
            !stop.load(Ordering::SeqCst)
                && seg.start.saturating_add(seg.done) <= seg.end
        })
        .await?;

        let mut segments = self.segments.lock().expect("segments lock");
        let seg = &mut segments[index];
        if self.ranged {
            let expected = seg.end.saturating_sub(seg.start).saturating_add(1);
            if seg.done < expected {
                return Err(Error::ShortRead { got: seg.done, expected });
            }
        }
        seg.complete = true;
        Ok(())
    }

    async fn fetch_once(&self, url: &str) -> Result<(), Error> {
        let segment = self.segments.lock().expect("segments lock")[self.index].clone();
        if segment.complete {
            return Ok(());
        }
        let from = if self.ranged { segment.resume_offset() } else { 0 };
        if self.is_ftp {
            return self.fetch_ftp(url, &segment, from).await;
        }
        let mut request = self.client.get(url);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if self.ranged {
            request = request.header("Range", format!("bytes={}-{}", from, segment.end));
        }
        let resp = request.send().await?;
        let status = resp.status().as_u16();
        if self.ranged && status != 206 {
            // Only a 200 means "I ignored your Range". Anything else is an
            // ordinary HTTP failure, and reporting a transient 503 as
            // RangeIgnored would make it permanent and kill the download.
            return Err(if status == 200 {
                Error::RangeIgnored
            } else {
                Error::Status(status)
            });
        }
        if !self.ranged && status != 200 {
            return Err(Error::Status(status));
        }

        // A 206 is not enough on its own: the server must return the exact
        // range we asked for. A server that answers 206 with a different
        // offset — seen in the wild behind caches and rewriting proxies —
        // would otherwise have its bytes written at our offset, silently
        // corrupting the file. Many clients only check the end; we check all
        // three.
        if self.ranged {
            let want = format!("bytes={}-{}", from, segment.end);
            let header = resp
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let Some(raw) = header else {
                return Err(Error::RangeMismatch { want, got: "no Content-Range".to_owned() });
            };
            let Some((start, end, total)) = parse_content_range(&raw) else {
                return Err(Error::RangeMismatch { want, got: raw });
            };
            let total_disagrees = matches!((total, self.total), (Some(t), Some(k)) if t != k);
            if start != from || end != segment.end || total_disagrees {
                return Err(Error::RangeMismatch { want, got: raw });
            }
        }

        let mut file = OpenOptions::new().write(true).open(&self.path).await?;
        file.seek(std::io::SeekFrom::Start(from)).await?;
        // A non-ranged retry restarts the body at offset 0, so the progress
        // recorded for the previous attempt no longer describes what is on
        // disk. Without this reset the counter double-counts and the download
        // reports more bytes than the file holds.
        if !self.ranged {
            self.segments.lock().expect("segments lock")[self.index].done = 0;
        }
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            if self.stop.load(Ordering::SeqCst) {
                file.flush().await?;
                return Ok(());
            }
            let chunk = chunk?;

            // Our end may have moved while this request was in flight: the
            // coordinator can hand the back half of this range to a new
            // connection. Anything past the new end belongs to that worker, so
            // write only up to it and stop. Writing the whole chunk would
            // duplicate bytes the other connection is already fetching.
            let (take, shrunk) = {
                let segments = self.segments.lock().expect("segments lock");
                let seg = &segments[self.index];
                // `end` is u64::MAX for an open-ended stream, so the +1 has to
                // saturate rather than wrap.
                let wanted = seg
                    .end
                    .saturating_add(1)
                    .saturating_sub(seg.start.saturating_add(seg.done));
                let take = (chunk.len() as u64).min(wanted) as usize;
                (take, take < chunk.len())
            };

            if take > 0 {
                file.write_all(&chunk[..take]).await?;
                // Flush before recording progress, never after. `write_all`
                // on a tokio File only queues into its buffer; if the
                // 500 ms checkpoint ran between the write and the flush it
                // would persist a `done` ahead of the bytes that actually
                // exist, and a resume would skip them — silent corruption
                // that looks like a complete file.
                file.flush().await?;
                let mut segments = self.segments.lock().expect("segments lock");
                segments[self.index].done += take as u64;
            }
            if shrunk {
                file.flush().await?;
                break;
            }
            self.rate.account(take as u64).await;
        }
        file.flush().await?;

        let mut segments = self.segments.lock().expect("segments lock");
        let seg = &mut segments[self.index];
        if self.ranged {
            // Saturating: `end` is the u64::MAX sentinel for an open-ended
            // stream, where `end + 1` would wrap and panic in debug.
            let expected = seg.end.saturating_add(1).saturating_sub(seg.start);
            if seg.done < expected {
                return Err(Error::ShortRead {
                    got: seg.done,
                    expected,
                });
            }
        }
        seg.complete = true;
        Ok(())
    }
}

/// Indices of queued downloads that may start now, given how many are already running.
pub fn next_to_start(statuses: &[Status], max_active: usize) -> Vec<usize> {
    let active = statuses.iter().filter(|s| **s == Status::Running).count();
    let free = max_active.saturating_sub(active);
    statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == Status::Queued)
        .map(|(i, _)| i)
        .take(free)
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_discarded_download_cannot_re_create_its_own_file() {
        // The real race: Remove-with-delete fires while `run` is still queued.
        // Without the latch, that run starts afterwards and makes the file
        // again — a row the user deleted, back on disk and downloading.
        let dir = std::env::temp_dir().join(format!("tdm-discard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("gone.bin");
        std::fs::write(&dest, b"partial").unwrap();
        std::fs::write(with_suffix(&dest, ".tdmstate"), b"{}").unwrap();

        // A URL that cannot resolve: if `run` got as far as the network the
        // test would be slow, and if it got as far as the file it would fail.
        let handle = Download::new("http://127.0.0.1:1/x", dest.clone(), 4);
        handle.stop_and_discard().await.expect("discard");
        assert!(!dest.exists(), "the file is gone");
        assert!(!with_suffix(&dest, ".tdmstate").exists(), "the sidecar is gone");

        handle.run().await.expect("a discarded run is a no-op, not an error");
        assert!(!dest.exists(), "the deleted file came back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_connection_page_sets_the_retry_budget() {
        let mut d = Download::new("http://x/y", "y", 4);
        // Retries are extra tries, so 0 must still make one attempt — a
        // budget of 0 would mean a download that never even tries.
        d.set_limits(0, 30);
        assert_eq!(d.attempts, 1);
        d.set_limits(5, 30);
        assert_eq!(d.attempts, 6);
        assert_eq!(d.timeout, Duration::from_secs(30));
        // A zero timeout is reqwest's "no timeout"; clamp it to a second.
        d.set_limits(1, 0);
        assert_eq!(d.timeout, Duration::from_secs(1));
    }

    #[test]
    fn parses_total_from_content_range() {
        assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    #[test]
    fn parses_all_three_fields_of_a_content_range() {
        assert_eq!(parse_content_range("bytes 0-99/200"), Some((0, 99, Some(200))));
        assert_eq!(parse_content_range("bytes 100-199/*"), Some((100, 199, None)));
        // Tolerate the spacing servers actually send.
        assert_eq!(parse_content_range("  bytes   5-9 / 10 "), Some((5, 9, Some(10))));
    }

    #[test]
    fn same_site_rejects_the_lookalike_host_a_substring_check_would_accept() {
        // A substring test accepts this pair, which is how a cookie for
        // example.com ends up at an attacker's host.
        assert!(!same_site(Some("example.com"), Some("evil-example.com")));
        assert!(!same_site(Some("example.com"), Some("example.com.evil.net")));
        assert!(!same_site(Some("example.com"), Some("notexample.com")));
    }

    #[test]
    fn same_site_accepts_a_host_and_its_subdomains() {
        assert!(same_site(Some("example.com"), Some("example.com")));
        assert!(same_site(Some("example.com"), Some("cdn.example.com")));
        assert!(same_site(Some("cdn.example.com"), Some("example.com")));
        assert!(same_site(Some("EXAMPLE.com"), Some("cdn.example.COM")));
        // A trailing root dot is the same host.
        assert!(same_site(Some("example.com."), Some("example.com")));
    }

    #[test]
    fn a_missing_host_is_never_same_site() {
        assert!(!same_site(None, Some("example.com")));
        assert!(!same_site(Some("example.com"), None));
        assert!(!same_site(None, None));
    }

    #[test]
    fn credentials_are_the_headers_dropped_across_sites() {
        for h in ["Cookie", "cookie", "Authorization", "Referer", "Proxy-Authorization"] {
            assert!(is_cross_site_sensitive(h), "{h} should be stripped");
        }
        for h in ["User-Agent", "Accept", "Range", "Accept-Encoding"] {
            assert!(!is_cross_site_sensitive(h), "{h} should be kept");
        }
    }

    #[test]
    fn rejects_content_ranges_we_must_not_guess_at() {
        // An inverted range would make the length calculation negative.
        assert_eq!(parse_content_range("bytes 99-0/200"), None);
        // A unit we do not understand must not be read as bytes.
        assert_eq!(parse_content_range("items 0-99/200"), None);
        assert_eq!(parse_content_range("bytes 0-99"), None);
        assert_eq!(parse_content_range("bytes abc-def/200"), None);
        assert_eq!(parse_content_range(""), None);
    }

    /// Walk every segment and assert they tile `size` exactly: no gap, no
    /// overlap, nothing past the end. Any split that breaks this corrupts the
    /// output file, so it is checked after every operation below.
    fn assert_tiles(segs: &[Segment], size: u64) {
        let mut ranges: Vec<(u64, u64)> = segs.iter().map(|s| (s.start, s.end)).collect();
        ranges.sort_unstable();
        assert_eq!(ranges[0].0, 0, "first segment does not start at 0");
        for pair in ranges.windows(2) {
            assert_eq!(
                pair[0].1 + 1,
                pair[1].0,
                "gap or overlap between {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(ranges.last().unwrap().1, size - 1, "last segment misses the end");
    }

    #[test]
    fn a_ranged_download_starts_as_a_single_whole_file_segment() {
        let size = 10 * 1024 * 1024 + 7;
        let segs = plan_segments(Some(size), true);
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].start, segs[0].end), (0, size - 1));
    }

    #[test]
    fn splitting_hands_over_the_back_half_and_still_tiles_the_file() {
        let size = 10 * 1024 * 1024;
        let mut segs = plan_segments(Some(size), true);
        // Pretend the first connection has fetched a quarter of the file.
        segs[0].done = size / 4;

        let new = split_largest(&mut segs).expect("a 10 MiB file must be splittable");
        assert_eq!(new, 1);
        assert_eq!(segs[1].end, size - 1, "the new segment should own the tail");
        assert!(segs[1].start > segs[0].start + segs[0].done, "the split must be ahead of progress");
        assert_tiles(&segs, size);
    }

    #[test]
    fn repeated_splits_keep_tiling_the_file() {
        let size = 64 * 1024 * 1024;
        let mut segs = plan_segments(Some(size), true);
        for _ in 0..8 {
            if split_largest(&mut segs).is_none() {
                break;
            }
            assert_tiles(&segs, size);
        }
        assert!(segs.len() > 4, "only {} segments after 8 attempts", segs.len());
    }

    #[test]
    fn a_range_below_the_threshold_is_not_split() {
        let mut segs = plan_segments(Some(MIN_SPLIT_BYTES - 1), true);
        assert!(split_largest(&mut segs).is_none());
        assert_eq!(segs.len(), 1, "a tiny file must stay on one connection");

        // Exactly at the threshold it is allowed.
        let mut segs = plan_segments(Some(MIN_SPLIT_BYTES * 4), true);
        assert!(split_largest(&mut segs).is_some());
    }

    /// A split must never land on bytes a worker may already be writing.
    /// Without the in-flight margin, both connections fetch the overlap.
    #[test]
    fn a_split_stays_clear_of_bytes_already_in_flight() {
        let size = 64 * 1024 * 1024;
        let mut segs = plan_segments(Some(size), true);
        // The worker is near the midpoint, which is exactly where the naive
        // rule would try to split.
        segs[0].done = size / 2 - 1000;

        if let Some(new) = split_largest(&mut segs) {
            assert!(
                segs[new].start > segs[0].start + segs[0].done,
                "split at {} overlaps the worker's position {}",
                segs[new].start,
                segs[0].start + segs[0].done
            );
            assert!(
                segs[new].start >= safe_split_floor(&segs[0]).min(segs[new].start),
                "split ignored the in-flight margin"
            );
        }
    }

    /// Across the whole range of remaining sizes, a split either does not
    /// happen or lands strictly beyond the in-flight margin. Neither a tiny
    /// segment nor a huge one may violate that.
    #[test]
    fn every_split_respects_the_in_flight_margin() {
        let size = 10 * 1024 * 1024;
        for left in [
            MIN_SPLIT_BYTES,
            MIN_SPLIT_BYTES + 1000,
            100 * 1024,
            1024 * 1024,
            8 * 1024 * 1024,
        ] {
            let mut segs = plan_segments(Some(size), true);
            segs[0].done = size - left;
            let floor = safe_split_floor(&segs[0]);
            let position = segs[0].start + segs[0].done;

            if let Some(new) = split_largest(&mut segs) {
                assert!(
                    segs[new].start > floor,
                    "left={left}: split at {} is inside the margin (floor {floor})",
                    segs[new].start
                );
                assert!(segs[new].start > position, "left={left}: split behind the worker");
                assert_tiles(&segs, size);
            }
        }
    }

    #[test]
    fn a_segment_at_the_hard_floor_is_still_splittable_but_below_it_is_not() {
        let size = 10 * 1024 * 1024;
        let mut at_floor = plan_segments(Some(size), true);
        at_floor[0].done = size - MIN_SPLIT_BYTES;
        assert!(split_largest(&mut at_floor).is_some(), "the floor itself should split");

        let mut below = plan_segments(Some(size), true);
        below[0].done = size - (MIN_SPLIT_BYTES - 1);
        assert!(split_largest(&mut below).is_none(), "below the floor must not split");
    }

    #[test]
    fn remaining_does_not_overflow_on_an_open_ended_segment() {
        let seg = Segment { start: 0, end: u64::MAX, done: 0, complete: false };
        assert_eq!(remaining(&seg), 0);
        // And a segment whose done has run past its end reports nothing left,
        // rather than wrapping to a huge number.
        let past = Segment { start: 0, end: 10, done: 50, complete: false };
        assert_eq!(remaining(&past), 0);
    }

    #[tokio::test]
    async fn turning_a_limit_on_later_does_not_stall_on_past_traffic() {
        // A gigabyte moved while unlimited must not be charged against a
        // limit set afterwards.
        let limiter = Limiter::new(None);
        for _ in 0..64 {
            limiter.account(16 * 1024 * 1024).await;
        }
        limiter.set(Some(1024 * 1024));

        let started = Instant::now();
        limiter.account(1024).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the limiter parked for {:?} on traffic that predates the limit",
            started.elapsed()
        );
        assert_eq!(limiter.get(), Some(1024 * 1024));
    }

    #[test]
    fn an_almost_finished_segment_is_not_split_again() {
        let size = 100 * 1024 * 1024;
        let mut segs = plan_segments(Some(size), true);
        segs[0].done = size - 10; // 10 bytes left
        assert!(split_largest(&mut segs).is_none());
    }

    #[test]
    fn splitting_picks_the_segment_with_the_most_left_not_the_largest() {
        let mut segs = vec![
            // Big range, but nearly done.
            Segment { start: 0, end: 999_999, done: 999_000, complete: false },
            // Smaller range, barely started: this is the one that needs help.
            Segment { start: 1_000_000, end: 1_500_000, done: 10, complete: false },
        ];
        let new = split_largest(&mut segs).expect("splittable");
        assert_eq!(segs[new].end, 1_500_000, "split the wrong segment");
    }

    #[test]
    fn a_completed_segment_is_never_chosen() {
        let mut segs = vec![
            Segment { start: 0, end: 9_999_999, done: 10_000_000, complete: true },
            Segment { start: 10_000_000, end: 10_000_100, done: 0, complete: false },
        ];
        // Only 101 bytes are genuinely outstanding, which is under the floor.
        assert!(split_largest(&mut segs).is_none());
    }

    #[test]
    fn queue_starts_only_as_many_as_free_slots() {
        use Status::*;
        let statuses = [Running, Queued, Finished, Queued, Queued];
        assert_eq!(next_to_start(&statuses, 2), vec![1]);
        assert_eq!(next_to_start(&statuses, 4), vec![1, 3, 4]);
        assert!(next_to_start(&statuses, 1).is_empty());
    }

    #[test]
    fn unknown_size_is_a_single_open_ended_stream() {
        let segs = plan_segments(None, false);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].end, u64::MAX);
        // An open-ended stream has no known tail to hand to anyone.
        let mut segs = segs;
        assert!(split_largest(&mut segs).is_none());
    }
}
