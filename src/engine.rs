//! Segmented HTTP download engine.
//!
//! Probe with a one-byte range, split into byte ranges, write each range at its
//! offset, and keep a resumable state file next to the download.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;

use crate::filetype;
use crate::model::ProxySettings;
use crate::planner;

/// Splitting a range with less than this left is not worth another connection.
///
/// About 23 KB: below that a new connection's handshake costs more than the
/// bytes it would take over, so splitting only thrashes.
const MIN_SPLIT_BYTES: u64 = 0x5b43;

/// How long a new connection must have been running, and receiving, before the
/// pool is allowed to grow again, so a slow link is not flooded with
/// connections.
const RAMP_INTERVAL: Duration = Duration::from_secs(2);

/// A connection is "slow" when its rate is under the median rate divided by
/// this factor, once it has run for `SLOW_AGE_SECS`.
const SLOW_FACTOR: f64 = 3.0;
const SLOW_AGE_SECS: f64 = 4.0;

/// Attempts per segment when the caller sets no budget of its own.
const MAX_ATTEMPTS: u32 = 3;

/// Socket timeout when the caller sets none. Matches the Options page's own
/// default rather than reqwest's (which is no timeout at all).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

/// Time allowed to fetch a PAC script over HTTP(S) before the download goes direct.
const PAC_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

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
    #[error("the file changed on the server while it was downloading")]
    ValidatorChanged,
    #[error("the disk is full: free some space, then resume")]
    DiskFull,
    #[error("the connection stalled: no data arrived within the timeout")]
    Stalled,
}

/// A reqwest failure that happened on the wire rather than in the request
/// itself: timeouts, connect failures, send failures and broken bodies.
fn network_failure(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
}

/// Whether a failed segment says the host will not take another connection: a
/// 503 refusal, or a connection the peer explicitly refused. Other network
/// failures (timeouts, resets, dropped bodies) are blips, not a limit, so they
/// do not lower a host's cap.
fn host_refused(e: &Error) -> bool {
    match e {
        Error::Status(503) => true,
        Error::Http(h) => connection_refused(h),
        _ => false,
    }
}

/// Whether the failure's cause chain holds an OS "connection refused".
fn connection_refused(e: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return io.kind() == std::io::ErrorKind::ConnectionRefused;
        }
        source = err.source();
    }
    false
}

impl Error {
    /// True for failures that may clear up on their own, so a retry is worth
    /// trying: network timeouts, connect and send failures, HTTP 408, 429 and
    /// 5xx, FTP 4xx and socket errors, and a connection that ended early.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(e) => network_failure(e),
            Error::Status(code) => matches!(code, 408 | 429 | 500..=599),
            Error::ShortRead { .. } | Error::Stalled => true,
            Error::Ftp(e) => matches!(
                e,
                crate::ftp::FtpError::Timeout
                    | crate::ftp::FtpError::Io(_)
                    | crate::ftp::FtpError::Transient { .. }
                    | crate::ftp::FtpError::ShortRead { .. }
            ),
            Error::Media(crate::media::Error::Http(e)) => network_failure(e),
            Error::Media(crate::media::Error::Status(code)) => {
                matches!(code, 408 | 429 | 500..)
            }
            _ => false,
        }
    }
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
pub(crate) fn same_site(a: Option<&str>, b: Option<&str>) -> bool {
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
pub(crate) fn is_cross_site_sensitive(name: &str) -> bool {
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
    /// The real type of the finished file, from its bytes. `None` until a
    /// normal download finishes and the sniff succeeds.
    pub kind: Option<filetype::Kind>,
    /// The Content-Type the server sent with the probe, trimmed.
    pub content_type: Option<String>,
    /// Why the download paused by itself, such as a full disk. Cleared when it
    /// is resumed. The UI shows it; the user resumes once there is room.
    pub paused_reason: Option<String>,
    /// The digest the server announced with the probe, for the checksum step.
    /// The downloaded bytes do not depend on it.
    pub server_hash: Option<crate::serverhash::ExpectedHash>,
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
    /// Its connection failed for good. The range is left unfinished and is
    /// not handed to a new connection; a later resume clears the flag and retries it.
    #[serde(default)]
    failed: bool,
}

impl Segment {
    fn resume_offset(&self) -> u64 {
        self.start + self.done
    }
}

/// What the server said identifies this version of the file. A resume is only
/// safe while these still match. Every ranged request repeats them in
/// `If-Range`, so the server sends the whole file if they changed meanwhile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Validators {
    fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        Self {
            etag: header_text(headers, "etag"),
            last_modified: header_text(headers, "last-modified"),
        }
    }

    /// The value for `If-Range`: the strong ETag, else Last-Modified. A weak
    /// ETag is not allowed there, so it is skipped.
    fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|e| !e.starts_with("W/"))
            .or(self.last_modified.as_deref())
    }
}

fn header_text(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    url: String,
    total: Option<u64>,
    ranged: bool,
    #[serde(default)]
    validators: Validators,
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
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Scheme, host and port of the proxy itself, e.g. `http://proxy.lan:3128`.
    pub url: String,
    pub http: bool,
    pub https: bool,
    /// Sent as Basic credentials. Empty `user` means no authentication.
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &self.url)
            .field("http", &self.http)
            .field("https", &self.https)
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "<redacted>" })
            .finish()
    }
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
    /// The proxy choice from the settings. Manual and system proxies resolve to
    /// a fixed [`ProxyConfig`]; a PAC script is evaluated per URL on each run.
    proxy: ProxySettings,
    /// Attempts per segment, and the per-request socket timeout. Both come
    /// from the Connection page.
    attempts: u32,
    timeout: Duration,
    /// Alternative URLs for the same file, tried in order when a segment keeps
    /// failing on the current one. The primary URL is always tried first.
    mirrors: Vec<String>,
    /// Learned per-host connection limits, shared between downloads. `None`
    /// means the requested count is used as it is.
    hosts: Option<Arc<Mutex<crate::hosts::HostLimits>>>,
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
            proxy: ProxySettings::default(),
            attempts: MAX_ATTEMPTS,
            timeout: DEFAULT_TIMEOUT,
            mirrors: Vec::new(),
            hosts: None,
        }
    }

    /// Share learned per-host connection limits with other downloads. A host
    /// that refuses connections past some count is then started below it.
    pub fn set_host_limits(&mut self, limits: Arc<Mutex<crate::hosts::HostLimits>>) {
        self.hosts = Some(limits);
    }

    /// How many connections this host may take, as far as it is known.
    fn host_cap(&self, host: &str) -> u32 {
        match &self.hosts {
            Some(limits) => limits
                .lock()
                .expect("host limits")
                .cap(host, self.max_segments, self.max_segments),
            None => self.max_segments,
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

    /// Route this download through the proxy the settings choose. Takes effect
    /// on the next `run`, since the HTTP client is built there.
    pub fn set_proxy(&mut self, proxy: ProxySettings) {
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

    /// Point the handle at a new file name, after the file was renamed on disk.
    pub fn set_dest(&mut self, dest: PathBuf) {
        self.dest = dest;
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
        {
            let mut s = self.shared.lock().expect("snapshot lock");
            s.error = None;
            s.paused_reason = None;
        }
        self.set_status(Status::Running);
        let mut result = self.run_inner().await;
        if matches!(result, Err(Error::ValidatorChanged)) {
            // The bytes on disk belong to the old version of the file. Start
            // over once, from a fresh probe; a second change is reported as a failure.
            let _ = tokio::fs::remove_file(self.state_path()).await;
            self.stop.store(false, Ordering::SeqCst);
            result = self.run_inner().await;
        }
        match &result {
            Ok(()) if self.stop.load(Ordering::SeqCst) => self.set_status(Status::Paused),
            Err(Error::DiskFull) => {
                // Not a failure: the sidecar is kept, so resuming picks up
                // where the disk filled.
                let mut s = self.shared.lock().expect("snapshot lock");
                s.status = Status::Paused;
                s.paused_reason = Some(Error::DiskFull.to_string());
            }
            Ok(()) => match self.check_server_hash().await {
                // A file that is not the one the server described stays on
                // disk, but is not reported as finished.
                Err(reason) => {
                    let mut s = self.shared.lock().expect("snapshot lock");
                    s.status = Status::Failed;
                    s.error = Some(reason);
                }
                // Sniffed before Finished is published, so the UI never sees a
                // finished download without its kind.
                Ok(()) => {
                    let kind = self.sniff_finished().await;
                    let mut s = self.shared.lock().expect("snapshot lock");
                    s.kind = kind;
                    s.status = Status::Finished;
                }
            },
            Err(e) => {
                let mut s = self.shared.lock().expect("snapshot lock");
                s.status = Status::Failed;
                s.error = Some(e.to_string());
            }
        }
        match result {
            Err(Error::DiskFull) => Ok(()),
            other => other,
        }
    }

    /// The real type of the finished file. Media output is a concatenation of
    /// segments, not the file the URL names, so it is skipped. A sniff error
    /// leaves the kind unknown rather than failing the download.
    async fn sniff_finished(&self) -> Option<filetype::Kind> {
        if crate::media::detect(&self.url, None, &[]).is_some() {
            return None;
        }
        let path = self.dest.clone();
        tokio::task::spawn_blocking(move || filetype::sniff_file(&path))
            .await
            .ok()?
            .ok()
    }

    /// Hash the finished file with the digest the server announced, if it
    /// announced one. `Err` carries the reason the download must fail.
    async fn check_server_hash(&self) -> Result<(), String> {
        let expected = self.shared.lock().expect("snapshot lock").server_hash.clone();
        let Some(expected) = expected else {
            return Ok(());
        };
        let path = self.dest.clone();
        tokio::task::spawn_blocking(move || verify_server_hash(&path, &expected))
            .await
            .map_err(|e| e.to_string())?
    }

    /// The proxy for this run's URL. A PAC script is fetched and evaluated; if
    /// that fails, the reason is logged and the download goes direct.
    async fn resolve_proxy(&self) -> Option<ProxyConfig> {
        let Some(location) = self.proxy.pac_location() else {
            return match self.proxy.resolved() {
                Ok(p) => p,
                Err(e) => {
                    let host = crate::hosts::HostLimits::host_key(&self.url);
                    tracing::warn!(%host, "proxy settings unusable ({e}), going direct");
                    None
                }
            };
        };
        match self.pac_proxy(location).await {
            Ok(proxy) => proxy,
            Err(reason) => {
                let host = crate::hosts::HostLimits::host_key(&self.url);
                tracing::warn!(%host, %reason, "PAC proxy unusable, going direct");
                None
            }
        }
    }

    /// Fetch the PAC script at `location` (an http(s) URL, a file:// URL or a
    /// local path) and ask it how to reach this download's URL.
    async fn pac_proxy(&self, location: &str) -> Result<Option<ProxyConfig>, String> {
        let remote = reqwest::Url::parse(location)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"));
        let script = match remote {
            Some(remote) => {
                // Direct, not through a proxy: the script is what picks the proxy.
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(PAC_FETCH_TIMEOUT)
                    .build()
                    .map_err(|e| e.to_string())?;
                client
                    .get(remote)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| format!("fetching the PAC script: {e}"))?
                    .text()
                    .await
                    .map_err(|e| format!("reading the PAC script: {e}"))?
            }
            None => {
                let path = reqwest::Url::parse(location)
                    .ok()
                    .filter(|u| u.scheme() == "file")
                    .and_then(|u| u.to_file_path().ok())
                    .unwrap_or_else(|| PathBuf::from(location));
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| format!("reading the PAC script {}: {e}", path.display()))?
            }
        };
        // QuickJS is synchronous and can run long, so it stays off the async workers.
        let proxy = self.proxy.clone();
        let url = self.url.clone();
        tokio::task::spawn_blocking(move || {
            proxy.resolve_pac(&url, &script).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// The HTTP client for this download, with the given redirect policy and
    /// proxy. The timeout is the same for every policy.
    fn build_client(
        &self,
        redirect: reqwest::redirect::Policy,
        proxy: Option<&ProxyConfig>,
    ) -> Result<reqwest::Client, Error> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("TDM/", env!("CARGO_PKG_VERSION")))
            // No whole-request limit: a large segment on a slow link may take far
            // longer than `timeout` to finish. These bound the connect phase and
            // each socket read, so a stalled connection still fails.
            .connect_timeout(self.timeout)
            .read_timeout(self.timeout)
            .redirect(redirect)
            // Keep one idle connection per segment so a segment's next request
            // reuses its socket instead of paying a new TCP (and TLS) handshake.
            .pool_max_idle_per_host(self.max_segments as usize)
            // Send each small request at once instead of waiting to fill a packet.
            .tcp_nodelay(true);
        if let Some(p) = proxy {
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
        Ok(builder.build()?)
    }

    async fn run_inner(&self) -> Result<(), Error> {
        // A streaming manifest is not a file to range over: it is a list of
        // other files. Detected from the URL only — a content-type-only manifest would need a fetch before
        // we know, and that fetch is exactly what `probe` is for.
        let media = crate::media::detect(&self.url, None, &[]).is_some();
        // Media follows redirects itself, since a manifest is commonly moved
        // by a CDN. The file path follows them in `probe` so that credentials
        // can be dropped on a cross-site hop; see `same_site`.
        let policy = if media {
            reqwest::redirect::Policy::limited(MAX_REDIRECTS)
        } else {
            reqwest::redirect::Policy::none()
        };
        let proxy = self.resolve_proxy().await;
        let client = self.build_client(policy, proxy.as_ref())?;

        let configured = self.headers.lock().expect("headers lock").clone();
        let host = crate::hosts::HostLimits::host_key(&self.url);

        if media {
            return self.run_media(&client, &configured).await;
        }

        let saved = self.load_state().await?;

        // FTP takes a different probe and a different fetch, but the same
        // segment planner: `REST` gives us a start offset, and the segment end
        // is enforced by closing the data socket, so a range is a range.
        let is_ftp = self.url.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("ftp://"))
            || self.url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("ftps://"));

        // Probe on every run, including a resume. It resolves the redirect
        // chain and re-confirms the size and range support. Resuming against a
        // file that changed on the server would otherwise write new bytes into
        // old segment offsets and produce a corrupt result that still looks
        // complete.
        let (total, ranged, headers, fetch_url, validators) = if is_ftp {
            let ftp = crate::ftp::FtpDownload::parse(&self.url)?;
            let p = ftp.probe().await?;
            // Without REST the server cannot resume, so the file is one stream.
            (
                p.size,
                p.supports_rest && p.size.is_some(),
                Vec::new(),
                self.url.clone(),
                Validators::default(),
            )
        } else {
            let p = probe(&client, &self.url, &configured).await?;
            {
                let mut s = self.shared.lock().expect("snapshot lock");
                s.content_type = p.content_type;
                s.server_hash = p.server_hash;
            }
            (p.total, p.ranged, p.headers, p.url, p.validators)
        };

        // A saved state that no longer matches the server's answer cannot be
        // trusted: start the file again.
        // The sidecar only describes bytes that are on disk. If the data file
        // is gone, or is not the size the plan assumes, resuming would mark
        // ranges complete that were never written.
        let on_disk = tokio::fs::metadata(&self.dest).await.ok().map(|m| m.len());
        // A sidecar written for another version of the file describes bytes the
        // server no longer has, so it is never resumed.
        // Without an ETag or Last-Modified there is no way to tell that the file
        // is still the same, so a sidecar is never resumed.
        let has_validator = validators.etag.is_some() || validators.last_modified.is_some();
        let resumed = saved.filter(|s| {
            has_validator
                && s.ranged == ranged
                && s.total == total
                && s.validators == validators
                && on_disk.is_some()
                && (!ranged || on_disk == total)
        });
        let fresh = resumed.is_none();
        let segments = match resumed {
            // Ranges that failed last time get another try on resume.
            Some(s) => s
                .segments
                .into_iter()
                .map(|seg| Segment { failed: false, ..seg })
                .collect::<Vec<Segment>>(),
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

        // Refuse before touching the file: a fresh start truncates it, and a
        // download that cannot fit should wait for room rather than fail half way.
        if let Some(size) = total {
            let existing = if fresh { 0 } else { on_disk.unwrap_or(0) };
            let dir = self
                .dest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if let Some(free) = free_bytes(dir)
                && free < size.saturating_sub(existing)
            {
                return Err(Error::DiskFull);
            }
        }

        // One handle for every range. A fresh start must not keep an older,
        // longer file's tail: with no known size there is no `set_len` to cut it off.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(fresh)
            .open(&self.dest)
            .await?;
        // Only the run that creates the file marks it, so a resume leaves the
        // existing marker alone.
        if on_disk.is_none() {
            let referrer = configured
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("referer"))
                .map(|(_, v)| v.as_str());
            mark_of_the_web(&self.dest, &self.url, referrer);
        }
        if let Some(size) = total {
            file.set_len(size).await.map_err(write_error)?;
        }
        let file = Arc::new(tokio::sync::Mutex::new(file));

        let segments = Arc::new(Mutex::new(segments));
        let mut tasks = JoinSet::new();
        let spawn_worker = |tasks: &mut JoinSet<_>, index: usize| {
            let worker = Worker {
                client: client.clone(),
                urls: std::iter::once(fetch_url.clone())
                    .chain(self.mirrors.iter().cloned())
                    .collect(),
                path: self.dest.clone(),
                file: Arc::clone(&file),
                index,
                ranged,
                if_range: validators.if_range().map(str::to_owned),
                validators: validators.clone(),
                timeout: self.timeout,
                segments: Arc::clone(&segments),
                stop: Arc::clone(&self.stop),
                rate: Arc::clone(&self.rate),
                headers: headers.clone(),
                total,
                is_ftp,
                attempts: self.attempts,
            };
            tasks.spawn(async move { (index, worker.run().await) });
        };

        // Every segment that still has work gets a connection. On a fresh
        // ranged download that is exactly one; on a resume it is however many
        // the previous run had split into.
        let initial: Vec<usize> = {
            let segs = segments.lock().expect("segments lock");
            (0..segs.len()).filter(|i| !segs[*i].complete && !segs[*i].failed).collect()
        };
        for index in &initial {
            spawn_worker(&mut tasks, *index);
        }
        let mut live = initial.len();

        let mut tick = tokio::time::interval(TICK);
        let mut first_error: Option<Error> = None;
        let mut last_ramp = Instant::now();
        let mut last_total_done = 0u64;
        // When each segment was first seen, for the slow-connection check.
        let mut started: HashMap<usize, Instant> = HashMap::new();
        // The segment layout last written to the sidecar, so an unchanged
        // checkpoint is not rewritten every tick.
        let mut last_saved: Option<Vec<Segment>> = None;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // A failed checkpoint must not abort a healthy transfer —
                    // the worst case is resuming from an older position, which
                    // is strictly better than losing the download.
                    let done_now = match self.publish(&segments, total, ranged, &validators, &mut last_saved).await {
                        Ok(done) => done,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not checkpoint progress");
                            segments.lock().expect("segments lock").iter().map(|s| s.done).sum()
                        }
                    };

                    // Grow the pool only while bytes are actually arriving, and
                    // never faster than one step per RAMP_INTERVAL. A stalled or
                    // very slow link gains nothing from more sockets.
                    let progressing = done_now > last_total_done;
                    last_total_done = done_now;
                    // Bytes arriving on `live` connections is the host working
                    // at that count, which is what clears a learned cap.
                    if progressing && let Some(limits) = &self.hosts {
                        limits.lock().expect("host limits").on_working(&host, live as u32);
                    }
                    // Connections that have run a while but move far slower
                    // than the rest are split first, so their tails move to
                    // new connections.
                    let mut slow = {
                        let now = Instant::now();
                        let segs = segments.lock().expect("segments lock");
                        let rates: Vec<planner::SegmentRate> = segs
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| !s.complete && !s.failed && s.end != u64::MAX)
                            .map(|(i, s)| {
                                let since = *started.entry(i).or_insert(now);
                                let age = now.duration_since(since).as_secs_f64();
                                planner::SegmentRate {
                                    id: i as u64,
                                    bytes_per_sec: s.done as f64 / age.max(1.0),
                                    age_secs: age,
                                }
                            })
                            .collect();
                        planner::slow_segments(&rates, SLOW_FACTOR, SLOW_AGE_SECS).into_iter()
                    };
                    let room = (self.host_cap(&host) as usize).saturating_sub(live);
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
                                slow.next()
                                    .and_then(|id| split_segment(&mut segs, id as usize))
                                    .or_else(|| split_largest(&mut segs))
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
                    Some(Ok((_, Ok(())))) => {
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
                    Some(Ok((index, Err(e)))) => {
                        // A refusal or a dropped connection tells us the host's
                        // limit: `live` connections, this one included, were open.
                        if host_refused(&e) {
                            if let Some(limits) = &self.hosts {
                                limits.lock().expect("host limits").on_refused(&host, live as u32);
                            }
                        }
                        live = live.saturating_sub(1);
                        // Only this range is lost. The others keep going until
                        // they are done, and the download still ends in this
                        // error because the failed range is unfinished.
                        segments.lock().expect("segments lock")[index].failed = true;
                        // A changed file or a full disk concerns every range,
                        // so those stop the rest.
                        let whole_file = matches!(e, Error::ValidatorChanged | Error::DiskFull);
                        if whole_file {
                            self.stop.store(true, Ordering::SeqCst);
                        }
                        if first_error.is_none() || whole_file {
                            first_error = Some(e);
                        }
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

        // A failed final checkpoint must not hide an earlier real error: a full
        // disk reported here would mask the failure that actually happened first.
        let published = self.publish(&segments, total, ranged, &validators, &mut last_saved).await;
        if let Some(e) = first_error {
            return Err(e);
        }
        published?;
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
        // Segment requests go to the manifest's host and to whatever hosts the
        // manifest names. Credentials are for the manifest's site only.
        let manifest_host = reqwest::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));

        let mut done = 0u64;
        for job in &jobs {
            // Written under a `.part` name and renamed when complete, so an
            // existing file at the output path is not truncated mid-download.
            let part = with_suffix(&job.output, ".part");
            let mut file = tokio::fs::File::create(&part).await?;
            for segment in &job.segments {
                if self.stop.load(Ordering::SeqCst) {
                    file.flush().await?;
                    return Ok(());
                }
                let segment_host = segment.url.host_str();
                let same = same_site(manifest_host.as_deref(), segment_host);
                let mut request = client.get(segment.url.clone());
                for (name, value) in headers {
                    if !same && is_cross_site_sensitive(name) {
                        continue;
                    }
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
            drop(file);
            tokio::fs::rename(&part, &job.output).await?;
        }

        // DASH keeps audio and video apart. Join them if ffmpeg is there;
        // otherwise say so and leave both files, rather than present a silent
        // video as finished.
        if jobs.len() > 1 && jobs.iter().any(|j| j.needs_mux) {
            let (video, audio) = (jobs[0].output.clone(), jobs[1].output.clone());
            let dest = self.dest.clone();
            // ffmpeg can run for minutes; keep it off the async worker threads.
            let status = tokio::task::spawn_blocking({
                let (video, audio) = (video.clone(), audio.clone());
                move || {
                    std::process::Command::new("ffmpeg")
                        .args(["-y", "-i"])
                        .arg(&video)
                        .arg("-i")
                        .arg(&audio)
                        .args(["-c", "copy"])
                        .arg(&dest)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                }
            })
            .await;
            match status {
                Ok(Ok(s)) if s.success() => {
                    let _ = tokio::fs::remove_file(&video).await;
                    let _ = tokio::fs::remove_file(&audio).await;
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
    ///
    /// `last_saved` is the segment layout already on disk. The write is skipped
    /// when nothing has changed since it. Comparing the whole layout, not just
    /// the byte total, also catches a split: it moves a boundary without
    /// changing `done`, and a stale layout on disk would resume the wrong range.
    async fn publish(
        &self,
        segments: &Arc<Mutex<Vec<Segment>>>,
        total: Option<u64>,
        ranged: bool,
        validators: &Validators,
        last_saved: &mut Option<Vec<Segment>>,
    ) -> Result<u64, Error> {
        let snapshot_segments = segments.lock().expect("segments lock").clone();
        let done: u64 = snapshot_segments.iter().map(|s| s.done).sum();
        self.shared.lock().expect("snapshot lock").done = done;

        if last_saved.as_ref() == Some(&snapshot_segments) {
            return Ok(done);
        }

        let state = StateFile {
            url: self.url.clone(),
            total,
            ranged,
            validators: validators.clone(),
            segments: snapshot_segments.clone(),
        };
        let json = serde_json::to_vec_pretty(&state)?;
        let tmp = with_suffix(&self.state_path(), ".tmp");
        tokio::fs::write(&tmp, json).await.map_err(write_error)?;
        tokio::fs::rename(&tmp, self.state_path()).await.map_err(write_error)?;
        *last_saved = Some(snapshot_segments);
        Ok(done)
    }

    async fn load_state(&self) -> Result<Option<StateFile>, Error> {
        let bytes = match tokio::fs::read(self.state_path()).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        // A corrupt sidecar must not make the download fail on every attempt.
        // Starting over costs the bytes already fetched, which is cheaper than
        // a download that can never run.
        let saved: StateFile = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %self.state_path().display(),
                    error = %e,
                    "ignoring unreadable resume state"
                );
                return Ok(None);
            }
        };
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
    /// The digest the server announced, if its headers name one.
    server_hash: Option<crate::serverhash::ExpectedHash>,
    /// The URL the chain actually ended at. Segment requests go here, so a
    /// redirect is resolved once instead of on every connection.
    url: String,
    /// The headers still safe to send, after any cross-site hop stripped the
    /// sensitive ones.
    headers: Vec<(String, String)>,
    /// The Content-Type header of the final response, trimmed; empty is `None`.
    content_type: Option<String>,
    /// The ETag and Last-Modified of the final response.
    validators: Validators,
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

        let validators = Validators::from_headers(resp.headers());
        // A digest only describes the whole file: a 200, or a 206 that covers
        // bytes 0 to total-1. A partial answer's digest is ignored.
        let whole_file = match status {
            200 => true,
            206 => resp
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .is_some_and(|(start, end, total)| {
                    start == 0 && total.is_some_and(|t| end.checked_add(1) == Some(t))
                }),
            _ => false,
        };
        let server_hash =
            if whole_file { crate::serverhash::from_headers(resp.headers()) } else { None };
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
                    content_type: content_type_of(&resp),
                    validators,
                    server_hash,
                })
            }
            200 => Ok(Probe {
                total: resp.content_length(),
                ranged: false,
                url: current.to_string(),
                headers,
                content_type: content_type_of(&resp),
                validators,
                server_hash,
            }),
            other => Err(Error::Status(other)),
        };
    }
    Err(Error::TooManyRedirects(MAX_REDIRECTS))
}

/// The response's Content-Type, trimmed. Empty or non-text values are `None`.
fn content_type_of(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
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
        Some(0) => vec![Segment { start: 0, end: 0, done: 0, complete: true, failed: false }],
        Some(size) if ranged && size > 0 => {
            vec![Segment { start: 0, end: size - 1, done: 0, complete: false, failed: false }]
        }
        _ => vec![Segment { start: 0, end: u64::MAX, done: 0, complete: false, failed: false }],
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
    // Candidates are (segment index, first byte still to fetch, end). Failed,
    // finished and open-ended segments are never split.
    let candidates: Vec<(u64, u64, u64)> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.complete && !s.failed && s.end != u64::MAX)
        .map(|(i, s)| (i as u64, s.start.saturating_add(s.done), s.end))
        .collect();
    let victim = planner::pick_split(&candidates)?;
    split_segment(segments, candidates[victim].0 as usize)
}

/// Hand the back half of segment `victim` to a new connection, if enough is
/// left. Returns the new segment's index.
fn split_segment(segments: &mut Vec<Segment>, victim: usize) -> Option<usize> {
    let seg = segments.get(victim)?;
    if seg.complete || seg.failed || seg.end == u64::MAX || remaining(seg) < MIN_SPLIT_BYTES {
        return None;
    }
    let (position, end) = (seg.start.saturating_add(seg.done), seg.end);
    let new_start = planner::split_point(position, end)?;
    // Stay clear of bytes the victim may already have in flight, see `safe_split_floor`.
    if new_start <= safe_split_floor(seg) {
        return None;
    }
    segments[victim].end = new_start - 1;
    segments.push(Segment { start: new_start, end, done: 0, complete: false, failed: false });
    Some(segments.len() - 1)
}

struct Worker {
    client: reqwest::Client,
    /// The primary URL first, then any mirrors. A transient failure moves the
    /// worker to the next entry before it retries.
    urls: Vec<String>,
    path: PathBuf,
    /// The one handle every range writes through. Each write seeks to its own
    /// offset while holding the lock.
    file: Arc<tokio::sync::Mutex<tokio::fs::File>>,
    index: usize,
    ranged: bool,
    /// Sent as `If-Range` on every ranged request. `None` when the server gave no validator.
    if_range: Option<String>,
    /// The validators from the probe. Every 206 must still carry them.
    validators: Validators,
    /// Socket and per-read stall timeout, from the Connection page.
    timeout: Duration,
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
            // Bytes written by this attempt. A non-ranged attempt restarts at zero.
            let before = if self.ranged { self.done() } else { 0 };
            match self.fetch_once(&url).await {
                Ok(()) => return Ok(()),
                // A server that ignores Range, or answers with the wrong one,
                // will not start honouring it on retry. Retrying would just
                // write the same wrong bytes again, so fail loudly instead.
                Err(
                    e @ (Error::RangeIgnored
                    | Error::RangeMismatch { .. }
                    | Error::ValidatorChanged
                    | Error::DiskFull),
                ) => return Err(e),
                // 4xx other than 408/429 is a permanent client error; retrying
                // a 404 only wastes time. Everything else — including a 503 or
                // a rate limit — is worth another attempt.
                Err(Error::Status(code))
                    if (400..500).contains(&code) && code != 408 && code != 429 =>
                {
                    return Err(Error::Status(code));
                }
                Err(e) => {
                    // An attempt that wrote bytes was progress, so the budget
                    // counts consecutive attempts without progress only.
                    if self.done() > before {
                        attempt = 0;
                    }
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

    /// Bytes this segment has written so far.
    fn done(&self) -> u64 {
        self.segments.lock().expect("segments lock")[self.index].done
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
        // A non-ranged retry restarts the body at offset 0, so the progress
        // recorded for the previous attempt no longer describes what is on
        // disk. Without this reset the counter double-counts and the download
        // reports more bytes than the file holds. It runs before the FTP branch
        // too: `fetch_ftp` adds to the same counter.
        if !self.ranged {
            self.segments.lock().expect("segments lock")[self.index].done = 0;
        }
        if self.is_ftp {
            return self.fetch_ftp(url, &segment, from).await;
        }
        let mut request = self.client.get(url);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if self.ranged {
            request = request.header("Range", format!("bytes={}-{}", from, segment.end));
            if let Some(validator) = &self.if_range {
                request = request.header("If-Range", validator);
            }
        }
        let resp = request.send().await?;
        let status = resp.status().as_u16();
        if self.ranged && status != 206 {
            // Only a 200 means "I ignored your Range" (or, with If-Range sent,
            // "the file changed"). Anything else is an ordinary HTTP failure,
            // and reporting a transient 503 as RangeIgnored would make it
            // permanent and kill the download.
            return Err(match (status, &self.if_range) {
                (200, Some(_)) => Error::ValidatorChanged,
                (200, None) => Error::RangeIgnored,
                _ => Error::Status(status),
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
            // The range is right, but the bytes must also come from the version
            // we probed. A changed validator, or none where we had one, means the
            // file was replaced: the caller starts over rather than mix versions.
            let now = Validators::from_headers(resp.headers());
            let etag_changed = self.validators.etag.is_some() && now.etag != self.validators.etag;
            let modified_changed = self.validators.last_modified.is_some()
                && now.last_modified != self.validators.last_modified;
            if etag_changed || modified_changed {
                return Err(Error::ValidatorChanged);
            }
        }

        let mut pos = from;
        let mut body = resp.bytes_stream();
        // The limit is per read, not per body: a read that waits longer than
        // the timeout is a stall, however large the segment is.
        while let Some(chunk) = tokio::time::timeout(self.timeout, body.next())
            .await
            .map_err(|_| Error::Stalled)?
        {
            if self.stop.load(Ordering::SeqCst) {
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
                // Flush before recording progress, never after. A buffered
                // write is not on disk yet; if the 500 ms checkpoint ran in
                // between it would persist a `done` ahead of the bytes that
                // actually exist, and a resume would skip them.
                {
                    let mut file = self.file.lock().await;
                    file.seek(std::io::SeekFrom::Start(pos)).await.map_err(write_error)?;
                    file.write_all(&chunk[..take]).await.map_err(write_error)?;
                    file.flush().await.map_err(write_error)?;
                }
                pos += take as u64;
                let mut segments = self.segments.lock().expect("segments lock");
                segments[self.index].done += take as u64;
            }
            if shrunk {
                break;
            }
            self.rate.account(take as u64).await;
        }

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

/// Free bytes on the volume holding `dir`. `None` where the check is not
/// available, in which case the download just tries and relies on `DiskFull`.
#[cfg(windows)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and lives across the call, and `free` is
    // a valid out-pointer. The two optional out-pointers may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut())
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Whether an OS write error means the volume is out of space: ENOSPC on Unix,
/// ERROR_DISK_FULL (or ERROR_HANDLE_DISK_FULL) on Windows.
fn is_disk_full(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    const FULL: &[i32] = &[112, 39];
    #[cfg(not(windows))]
    const FULL: &[i32] = &[28];
    e.raw_os_error().is_some_and(|code| FULL.contains(&code))
}

/// A write failure as an engine error: a full disk pauses, anything else is an I/O error.
fn write_error(e: std::io::Error) -> Error {
    if is_disk_full(&e) { Error::DiskFull } else { Error::Io(e) }
}

/// Hash the file at `path` with the algorithm the server announced and compare
/// it with the announced digest.
fn verify_server_hash(
    path: &Path,
    expected: &crate::serverhash::ExpectedHash,
) -> Result<(), String> {
    let actual = crate::checksum::hash_file(path, expected.algo)
        .map_err(|e| format!("could not read the file to check its server hash: {e}"))?;
    if crate::serverhash::matches(expected, &actual) {
        Ok(())
    } else {
        Err(format!(
            "server hash mismatch: the file is not the {:?} digest the server announced",
            expected.algo
        ))
    }
}

/// Tag a newly created file as downloaded from the internet (Windows Mark of
/// the Web), so Windows warns before it runs. Failure is logged, not fatal.
#[cfg(windows)]
fn mark_of_the_web(dest: &Path, url: &str, referrer: Option<&str>) {
    let mut body = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(r) = referrer.filter(|r| !r.is_empty()) {
        body.push_str(&format!("ReferrerUrl={r}\r\n"));
    }
    body.push_str(&format!("HostUrl={url}\r\n"));
    let mut ads = dest.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    if let Err(e) = std::fs::write(PathBuf::from(ads), body) {
        let host = crate::hosts::HostLimits::host_key(url);
        tracing::warn!(%host, "could not write Mark of the Web: {e}");
    }
}

#[cfg(not(windows))]
fn mark_of_the_web(_dest: &Path, _url: &str, _referrer: Option<&str>) {}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_statuses_are_retried() {
        for code in [408, 429, 500, 502, 503, 504, 599] {
            assert!(Error::Status(code).is_transient(), "{code} should be transient");
        }
        for code in [200, 206, 301, 400, 401, 403, 404, 416, 600] {
            assert!(!Error::Status(code).is_transient(), "{code} should not be transient");
        }
        assert!(Error::ShortRead { got: 1, expected: 2 }.is_transient());
        assert!(!Error::BadUrl("x".into()).is_transient());
        assert!(!Error::RangeIgnored.is_transient());
    }

    #[tokio::test]
    async fn network_failures_are_transient_and_client_errors_are_not() {
        // A socket that accepts but never answers: the request times out.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let silent_url = format!("http://{}/", silent.local_addr().expect("addr"));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .expect("client");
        let timeout = client.get(&silent_url).send().await.expect_err("should time out");
        assert!(Error::Http(timeout).is_transient());
        drop(silent);

        // A port nothing listens on any more: the connect is refused.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let closed_url = format!("http://{}/", closed.local_addr().expect("addr"));
        drop(closed);
        let refused = reqwest::Client::new().get(&closed_url).send().await.expect_err("refused");
        assert!(Error::Http(refused).is_transient());

        assert!(Error::Ftp(crate::ftp::FtpError::Timeout).is_transient());
        assert!(Error::Ftp(crate::ftp::FtpError::Io(std::io::Error::other("reset"))).is_transient());
        assert!(
            Error::Ftp(crate::ftp::FtpError::Transient { code: 421, text: "busy".into() })
                .is_transient()
        );
        assert!(Error::Ftp(crate::ftp::FtpError::ShortRead { got: 1, expected: 2 }).is_transient());
        assert!(Error::Media(crate::media::Error::Status(503)).is_transient());
        assert!(Error::Media(crate::media::Error::Status(429)).is_transient());

        assert!(!Error::Media(crate::media::Error::Status(404)).is_transient());
        assert!(!Error::Ftp(crate::ftp::FtpError::Permanent { code: 550, text: "no".into() })
            .is_transient());
        assert!(!Error::Ftp(crate::ftp::FtpError::BadUrl("x".into())).is_transient());
        assert!(!Error::Ftp(crate::ftp::FtpError::NoRest).is_transient());
    }

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
        let seg = Segment { start: 0, end: u64::MAX, done: 0, complete: false, failed: false };
        assert_eq!(remaining(&seg), 0);
        // And a segment whose done has run past its end reports nothing left,
        // rather than wrapping to a huge number.
        let past = Segment { start: 0, end: 10, done: 50, complete: false, failed: false };
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
            Segment { start: 0, end: 999_999, done: 999_000, complete: false, failed: false },
            // Smaller range, barely started: this is the one that needs help.
            Segment { start: 1_000_000, end: 1_500_000, done: 10, complete: false, failed: false },
        ];
        let new = split_largest(&mut segs).expect("splittable");
        assert_eq!(segs[new].end, 1_500_000, "split the wrong segment");
    }

    #[test]
    fn a_completed_segment_is_never_chosen() {
        let mut segs = vec![
            Segment { start: 0, end: 9_999_999, done: 10_000_000, complete: true, failed: false },
            Segment { start: 10_000_000, end: 10_000_100, done: 0, complete: false, failed: false },
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

    /// A scratch directory unique to this test, not yet created.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tdm-{name}-{}", std::process::id()))
    }

    /// Serve `content` over HTTP on 127.0.0.1, honouring `Range: bytes=a-b`.
    /// Returns the URL of the file. The server thread lives until the test
    /// process exits.
    fn serve_range_file(content: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let content = content.clone();
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut range = None;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            if let Some((a, b)) = v.split_once('-') {
                                range = Some((a.trim().parse::<usize>().unwrap(), b.trim().parse::<usize>().unwrap()));
                            }
                        }
                    }
                    let len = content.len();
                    let (start, end) = range.unwrap_or((0, len - 1));
                    let body = &content[start..=end];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{len}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                });
            }
        });
        format!("http://{addr}/file.bin")
    }

    #[tokio::test]
    async fn a_multibyte_url_prefix_is_an_error_not_a_panic() {
        // Byte 6 falls inside the two-byte `é`, so slicing the prefix panics.
        let dest = scratch("multibyte").join("x.bin");
        let handle = Download::new("ftp:/é", dest.clone(), 1);
        assert!(handle.run().await.is_err(), "a bad URL must fail the run");
        assert_eq!(handle.snapshot().status, Status::Failed);
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_sidecar_without_its_data_file_does_not_resume() {
        // The sidecar claims the whole file is done, but the file itself is
        // gone. Trusting it would "complete" a download that wrote nothing.
        let content: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let len = content.len() as u64;
        let url = serve_range_file(content.clone());

        let dir = scratch("no-data-file");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("file.bin");
        let _ = std::fs::remove_file(&dest);
        let state = StateFile {
            url: url.clone(),
            total: Some(len),
            ranged: true,
            validators: Validators::default(),
            segments: vec![Segment { start: 0, end: len - 1, done: len, complete: true, failed: false }],
        };
        std::fs::write(with_suffix(&dest, ".tdmstate"), serde_json::to_vec(&state).unwrap()).unwrap();

        Download::new(url, dest.clone(), 1).run().await.expect("download");
        assert_eq!(std::fs::read(&dest).unwrap(), content, "the bytes were fetched from zero");
        assert!(!with_suffix(&dest, ".tdmstate").exists(), "a finished download drops its sidecar");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_corrupt_sidecar_is_ignored_so_the_download_starts_fresh() {
        let dir = scratch("garbage-sidecar");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("g.bin");
        std::fs::write(with_suffix(&dest, ".tdmstate"), b"\x00 not json at all {").unwrap();

        let saved = Download::new("http://x/y", dest.clone(), 1).load_state().await;
        assert!(matches!(saved, Ok(None)), "garbage must read as no state, not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One version of a file that a test server can serve.
    #[derive(Clone)]
    struct Version {
        etag: String,
        content: Vec<u8>,
    }

    /// Serve `first` on 127.0.0.1 with its ETag, honouring `If-Range` the way a
    /// real server does: a mismatch gets the whole file back as a 200. When
    /// `flip` is set, the first range request after the probe swaps the file
    /// to `flip` before answering, as if it had been replaced mid-download.
    /// Every request head is recorded in `heads`.
    fn serve_versioned(
        first: Version,
        flip: Option<Version>,
        heads: Arc<Mutex<Vec<String>>>,
    ) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new((first, flip)));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = Arc::clone(&state);
                let heads = Arc::clone(&heads);
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        head.push_str(&line);
                    }
                    heads.lock().unwrap().push(head.clone());

                    let range = head.lines().find_map(|l| {
                        let r = l.to_ascii_lowercase().strip_prefix("range: bytes=")?.to_owned();
                        let (a, b) = r.split_once('-')?;
                        Some((a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?))
                    });
                    // The ETag is case-sensitive, so take If-Range from the original line.
                    let if_range = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("if-range:"))
                        .map(|l| l["if-range:".len()..].trim().to_owned());

                    let current = {
                        let mut guard = state.lock().unwrap();
                        if range.is_some() && range != Some((0, 0)) {
                            if let Some(next) = guard.1.take() {
                                guard.0 = next;
                            }
                        }
                        guard.0.clone()
                    };
                    let len = current.content.len();
                    let if_range_holds = if_range.as_deref().is_none_or(|v| v == current.etag);
                    let (status, start, end) = match range {
                        Some((s, e)) if if_range_holds => ("206 Partial Content", s, e.min(len - 1)),
                        _ => ("200 OK", 0, len - 1),
                    };
                    let content_range = if status.starts_with("206") {
                        format!("Content-Range: bytes {start}-{end}/{len}\r\n")
                    } else {
                        String::new()
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\n{content_range}Content-Length: {}\r\nETag: {}\r\nConnection: close\r\n\r\n",
                        end - start + 1,
                        current.etag
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(&current.content[start..=end]);
                });
            }
        });
        format!("http://{addr}/file.bin")
    }

    /// Answer every connection with `response`, whatever it asked for.
    fn serve_fixed(response: &'static [u8]) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    let _ = stream.write_all(response);
                });
            }
        });
        format!("http://{addr}/file.bin")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sidecar_from_another_version_of_the_file_is_discarded() {
        // Same size, different ETag: the sidecar claims every byte is done, so
        // resuming it would keep the old version's bytes in the new file.
        let old = Version { etag: "\"a\"".into(), content: vec![b'a'; 50_000] };
        let new = Version { etag: "\"b\"".into(), content: vec![b'b'; 50_000] };
        let url = serve_versioned(new.clone(), None, Default::default());

        let dir = scratch("other-version");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("file.bin");
        std::fs::write(&dest, &old.content).unwrap();
        let state = StateFile {
            url: url.clone(),
            total: Some(50_000),
            ranged: true,
            validators: Validators { etag: Some(old.etag.clone()), last_modified: None },
            segments: vec![Segment { start: 0, end: 49_999, done: 50_000, complete: true, failed: false }],
        };
        std::fs::write(with_suffix(&dest, ".tdmstate"), serde_json::to_vec(&state).unwrap()).unwrap();

        Download::new(url, dest.clone(), 1).run().await.expect("download");
        assert!(std::fs::read(&dest).unwrap() == new.content, "bytes from the old version were kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_ranged_request_sends_the_etag_as_if_range() {
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let url = serve_versioned(
            Version { etag: "\"v1\"".into(), content: content.clone() },
            None,
            Arc::clone(&heads),
        );
        let dir = scratch("if-range");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("file.bin");
        let _ = std::fs::remove_file(&dest);

        Download::new(url, dest.clone(), 4).run().await.expect("download");
        assert!(std::fs::read(&dest).unwrap() == content);

        let heads = heads.lock().unwrap();
        let ranged: Vec<&String> = heads
            .iter()
            .filter(|h| !h.to_ascii_lowercase().contains("range: bytes=0-0"))
            .collect();
        assert!(!ranged.is_empty(), "no ranged request was made");
        for head in ranged {
            assert!(
                head.lines().any(|l| l.eq_ignore_ascii_case("if-range: \"v1\"")),
                "ranged request without If-Range: {head}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_that_changes_mid_download_is_fetched_again_from_scratch() {
        // The server answers the segment request with the whole new file (a 200
        // to a ranged request under If-Range). The download must start over
        // from a fresh probe, not splice the new bytes onto the old ones.
        let old = Version {
            etag: "\"a\"".into(),
            content: (0..300_000u32).map(|i| (i % 251) as u8).collect(),
        };
        let new = Version {
            etag: "\"b\"".into(),
            content: (0..300_000u32).map(|i| (i % 241) as u8).collect(),
        };
        let url = serve_versioned(old, Some(new.clone()), Default::default());
        let dir = scratch("changed-midway");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("file.bin");
        let _ = std::fs::remove_file(&dest);

        let download = Download::new(url, dest.clone(), 1);
        download.run().await.expect("download after a restart");
        assert!(std::fs::read(&dest).unwrap() == new.content, "bytes from both versions");
        assert_eq!(download.snapshot().status, Status::Finished);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn only_a_503_or_a_refused_connection_counts_against_a_host() {
        // A timeout is a slow host, not a full one, so it must not lower the cap.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let silent_url = format!("http://{}/", silent.local_addr().expect("addr"));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .expect("client");
        let timeout = client.get(&silent_url).send().await.expect_err("should time out");
        assert!(!host_refused(&Error::Http(timeout)));
        drop(silent);

        // A port nothing listens on: the peer explicitly refused.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let closed_url = format!("http://{}/", closed.local_addr().expect("addr"));
        drop(closed);
        let refused = reqwest::Client::new().get(&closed_url).send().await.expect_err("refused");
        assert!(host_refused(&Error::Http(refused)));

        assert!(host_refused(&Error::Status(503)));
        assert!(!host_refused(&Error::Status(500)));
        assert!(!host_refused(&Error::Stalled));
    }

    #[test]
    fn a_full_disk_error_is_recognised_by_its_os_code() {
        let full = if cfg!(windows) { 112 } else { 28 };
        assert!(is_disk_full(&std::io::Error::from_raw_os_error(full)));
        assert!(matches!(write_error(std::io::Error::from_raw_os_error(full)), Error::DiskFull));
        assert!(!is_disk_full(&std::io::Error::from_raw_os_error(2)));
        assert!(matches!(write_error(std::io::Error::from_raw_os_error(2)), Error::Io(_)));
        assert!(!Error::DiskFull.is_transient(), "a full disk must not be retried in a loop");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_too_big_for_the_disk_pauses_instead_of_failing() {
        // The server claims a file far larger than any disk. The preflight
        // must refuse before anything is allocated.
        let url = serve_fixed(
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000000000000000\r\n\
              Content-Length: 1\r\nConnection: close\r\n\r\nx",
        );
        let dir = scratch("no-room");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("huge.bin");
        let _ = std::fs::remove_file(&dest);

        let download = Download::new(url, dest.clone(), 1);
        download.run().await.expect("a full disk pauses the download rather than failing it");
        let snap = download.snapshot();
        assert_eq!(snap.status, Status::Paused);
        assert!(snap.paused_reason.is_some(), "the UI needs a reason to show");
        assert!(!dest.exists(), "nothing was allocated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_must_match_the_digest_the_server_announced() {
        use crate::serverhash::{ExpectedHash, HashAlgo};
        let dir = scratch("server-hash");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        // MD5 of "abc", as raw bytes.
        let md5 = |last: u8| ExpectedHash {
            algo: HashAlgo::Md5,
            bytes: vec![
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, last,
            ],
        };

        assert!(verify_server_hash(&path, &md5(0x72)).is_ok());
        let reason = verify_server_hash(&path, &md5(0x73)).unwrap_err();
        assert!(reason.contains("server hash mismatch"), "got {reason}");
        assert!(path.exists(), "a file that fails the check is kept for the user");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
