//! Deterministic HTTP lab server with fault injection.
//!
//! The served content is generated from a seeded PRNG keyed on the byte
//! offset, so any range can be regenerated and checked with [`expected_bytes`]
//! without storing the file anywhere.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::Response;
use axum::body::Body;
use axum::serve::Listener;
use futures_util::stream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const BASE_SEED: u64 = 0x5444_4D5F_4C41_4253; // "TDM_LABS"
const LAST_MODIFIED: &str = "Thu, 01 Jan 2026 00:00:00 GMT";
const DEFAULT_CHUNK: u64 = 64 * 1024;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Errors from the lab server.
#[derive(Debug)]
pub enum LabError {
    Io(io::Error),
    InvalidScenario(String),
    InvalidConfig(String),
}

impl fmt::Display for LabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabError::Io(e) => write!(f, "I/O error: {e}"),
            LabError::InvalidScenario(s) => write!(f, "invalid scenario: {s}"),
            LabError::InvalidConfig(s) => write!(f, "invalid config: {s}"),
        }
    }
}

impl std::error::Error for LabError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LabError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for LabError {
    fn from(e: io::Error) -> Self {
        LabError::Io(e)
    }
}

/// Fault-injection scenario served by the lab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// No faults.
    Fast,
    /// Delay `ms` milliseconds before the response headers (first byte).
    Latency { ms: u64 },
    /// Close the connection after `after_bytes` of body, on every request.
    Drops { after_bytes: u64 },
    /// Throttle the body to `bytes_per_sec`.
    Capped { bytes_per_sec: u64 },
    /// Answer 503 while more than `max_in_flight` connections are open.
    Refuse { max_in_flight: u64 },
    /// Ignore the Range header and always answer 200 with the full body.
    NoRange,
    /// Content changes after the first request: ETag and bytes change.
    Changed,
    /// Small chunks with a short delay between them.
    SlowRead,
    /// Every `n`th file request returns 500.
    FailEvery { n: u64 },
}

impl FromStr for Scenario {
    type Err = LabError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || LabError::InvalidScenario(s.to_string());
        let (name, arg) = match s.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (s, None),
        };
        let num = |a: Option<&str>| -> Result<u64, LabError> {
            a.ok_or_else(bad)?.parse::<u64>().map_err(|_| bad())
        };
        let scenario = match (name, arg.is_some()) {
            ("fast", false) => Scenario::Fast,
            ("norange", false) => Scenario::NoRange,
            ("changed", false) => Scenario::Changed,
            ("slowread", false) => Scenario::SlowRead,
            ("latency", true) => Scenario::Latency { ms: num(arg)? },
            ("drops", true) => Scenario::Drops { after_bytes: num(arg)? },
            ("capped", true) => {
                let bytes_per_sec = num(arg)?;
                if bytes_per_sec == 0 {
                    return Err(bad());
                }
                Scenario::Capped { bytes_per_sec }
            }
            ("refuse", true) => Scenario::Refuse { max_in_flight: num(arg)? },
            ("fail_every", true) => {
                let n = num(arg)?;
                if n == 0 {
                    return Err(bad());
                }
                Scenario::FailEvery { n }
            }
            _ => return Err(bad()),
        };
        Ok(scenario)
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scenario::Fast => write!(f, "fast"),
            Scenario::Latency { ms } => write!(f, "latency:{ms}"),
            Scenario::Drops { after_bytes } => write!(f, "drops:{after_bytes}"),
            Scenario::Capped { bytes_per_sec } => write!(f, "capped:{bytes_per_sec}"),
            Scenario::Refuse { max_in_flight } => write!(f, "refuse:{max_in_flight}"),
            Scenario::NoRange => write!(f, "norange"),
            Scenario::Changed => write!(f, "changed"),
            Scenario::SlowRead => write!(f, "slowread"),
            Scenario::FailEvery { n } => write!(f, "fail_every:{n}"),
        }
    }
}

/// Configuration for [`start`].
#[derive(Debug, Clone)]
pub struct LabConfig {
    pub size_bytes: u64,
    pub scenario: Scenario,
    /// TCP port on 127.0.0.1; 0 picks an ephemeral port.
    pub port: u16,
    /// URL path of the served file, starting with '/'.
    pub path: String,
}

/// A running lab server. Dropping it signals shutdown; call [`RunningLab::shutdown`]
/// to wait for it.
pub struct RunningLab {
    pub addr: SocketAddr,
    pub url: String,
    /// Number of TCP connections accepted so far.
    pub accepted: Arc<AtomicU64>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl RunningLab {
    /// Stop accepting, close idle connections, and wait for the server task.
    pub async fn shutdown(mut self) -> Result<(), LabError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(SHUTDOWN_GRACE, &mut task).await {
            Ok(joined) => {
                joined
                    .map_err(io::Error::other)?
                    .map_err(LabError::Io)?;
            }
            Err(_) => task.abort(),
        }
        Ok(())
    }
}

impl Drop for RunningLab {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Bytes `[start, start + len)` of the deterministic content for a file of
/// `size` bytes. The range is clamped to the file end.
pub fn expected_bytes(size: u64, start: u64, len: u64) -> Vec<u8> {
    let start = start.min(size);
    let len = len.min(size - start);
    let mut out = vec![0u8; len as usize];
    fill(BASE_SEED, start, &mut out);
    out
}

/// Parse a byte count with an optional KiB, MiB or GiB suffix (case-insensitive).
pub fn parse_size(text: &str) -> Result<u64, LabError> {
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (digits, suffix) = t.split_at(split);
    let bad = || LabError::InvalidConfig(format!("bad size '{text}'"));
    let n: u64 = digits.parse().map_err(|_| bad())?;
    let mult: u64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        _ => return Err(bad()),
    };
    n.checked_mul(mult).ok_or_else(bad)
}

/// Start a lab server on 127.0.0.1.
pub async fn start(config: LabConfig) -> Result<RunningLab, LabError> {
    if !config.path.starts_with('/') {
        return Err(LabError::InvalidConfig(format!("path must start with '/': {}", config.path)));
    }
    let listener = TcpListener::bind(("127.0.0.1", config.port)).await?;
    let addr = listener.local_addr()?;

    let accepted = Arc::new(AtomicU64::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(Shared {
        size: config.size_bytes,
        scenario: config.scenario,
        path: config.path.clone(),
        requests: AtomicU64::new(0),
        inflight: inflight.clone(),
    });
    let app = axum::Router::new().fallback(handle).with_state(shared);
    let counting = CountingListener {
        inner: listener,
        accepted: accepted.clone(),
        inflight,
    };

    let (tx, rx) = oneshot::channel::<()>();
    let server = axum::serve(counting, app).with_graceful_shutdown(async move {
        let _ = rx.await;
    });
    let task = tokio::spawn(async move { server.await });
    tracing::debug!(%addr, scenario = %config.scenario, "tdm-labserver listening");

    Ok(RunningLab {
        addr,
        url: format!("http://{addr}{}", config.path),
        accepted,
        shutdown_tx: Some(tx),
        task: Some(task),
    })
}

// ---------------------------------------------------------------------------
// Content generation

fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4F_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seed for a content generation. Generation 0 is the stable content;
/// the `changed` scenario moves to new generations after the first request.
fn generation_seed(generation: u64) -> u64 {
    if generation == 0 {
        BASE_SEED
    } else {
        splitmix64(BASE_SEED ^ generation.wrapping_mul(0x2545_F491_4F6C_DD1D))
    }
}

/// Fill `out` with content starting at byte `offset`. Each aligned 8-byte
/// block is one PRNG output, so any offset can be regenerated directly.
fn fill(seed: u64, offset: u64, out: &mut [u8]) {
    let mut pos = offset;
    let mut i = 0usize;
    while i < out.len() {
        let block = splitmix64(seed ^ (pos / 8)).to_le_bytes();
        let within = (pos % 8) as usize;
        let take = (8 - within).min(out.len() - i);
        out[i..i + take].copy_from_slice(&block[within..within + take]);
        i += take;
        pos += take as u64;
    }
}

// ---------------------------------------------------------------------------
// Connection tracking

struct CountingListener {
    inner: TcpListener,
    accepted: Arc<AtomicU64>,
    inflight: Arc<AtomicUsize>,
}

impl Listener for CountingListener {
    type Io = Tracked;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, addr) = Listener::accept(&mut self.inner).await;
        self.accepted.fetch_add(1, Ordering::SeqCst);
        self.inflight.fetch_add(1, Ordering::SeqCst);
        let io = Tracked {
            inner: stream,
            inflight: self.inflight.clone(),
        };
        (io, addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A TCP stream that decrements the in-flight counter when it is dropped.
struct Tracked {
    inner: TcpStream,
    inflight: Arc<AtomicUsize>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AsyncRead for Tracked {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tracked {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// HTTP handling

struct Shared {
    size: u64,
    scenario: Scenario,
    path: String,
    requests: AtomicU64,
    inflight: Arc<AtomicUsize>,
}

enum RangeReq {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// Parse a single `bytes=` range. Multi-range and malformed headers are
/// ignored (full body), as RFC 9110 permits.
fn parse_range(header_value: &str, size: u64) -> RangeReq {
    let Some(spec) = header_value.trim().strip_prefix("bytes=") else {
        return RangeReq::Full;
    };
    if spec.contains(',') {
        return RangeReq::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeReq::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    if size == 0 {
        return RangeReq::Unsatisfiable;
    }
    if a.is_empty() {
        // Suffix range: last N bytes.
        let Ok(n) = b.parse::<u64>() else {
            return RangeReq::Full;
        };
        if n == 0 {
            return RangeReq::Unsatisfiable;
        }
        return RangeReq::Partial {
            start: size.saturating_sub(n),
            end: size - 1,
        };
    }
    let Ok(start) = a.parse::<u64>() else {
        return RangeReq::Full;
    };
    let end = if b.is_empty() {
        size - 1
    } else {
        let Ok(e) = b.parse::<u64>() else {
            return RangeReq::Full;
        };
        if e < start {
            return RangeReq::Full;
        }
        e.min(size - 1)
    };
    if start >= size {
        return RangeReq::Unsatisfiable;
    }
    RangeReq::Partial { start, end }
}

struct BodyState {
    seed: u64,
    offset: u64,
    remaining: u64,
    sent: u64,
    chunk: u64,
    delay: Option<Duration>,
    drop_after: Option<u64>,
}

async fn next_body_item(mut st: BodyState) -> Option<(io::Result<Vec<u8>>, BodyState)> {
    if st.remaining == 0 {
        return None;
    }
    if let Some(limit) = st.drop_after {
        if st.sent >= limit {
            let err = io::Error::new(io::ErrorKind::ConnectionAborted, "lab: connection dropped by scenario");
            return Some((Err(err), st));
        }
    }
    if let Some(delay) = st.delay {
        tokio::time::sleep(delay).await;
    }
    let mut take = st.chunk.min(st.remaining);
    if let Some(limit) = st.drop_after {
        take = take.min(limit - st.sent);
    }
    let mut buf = vec![0u8; take as usize];
    fill(st.seed, st.offset, &mut buf);
    st.offset += take;
    st.remaining -= take;
    st.sent += take;
    Some((Ok(buf), st))
}

fn plain(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .expect("static response is valid")
}

async fn handle(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    if req.uri().path() != shared.path {
        return plain(StatusCode::NOT_FOUND);
    }
    let head = match *req.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => return plain(StatusCode::METHOD_NOT_ALLOWED),
    };
    // 1-based index of this file request.
    let count = shared.requests.fetch_add(1, Ordering::SeqCst) + 1;
    let scenario = shared.scenario;

    match scenario {
        Scenario::Refuse { max_in_flight } if shared.inflight.load(Ordering::SeqCst) as u64 > max_in_flight => {
            // Close the socket so the refused client frees its slot.
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONNECTION, "close")
                .header(header::CONTENT_LENGTH, 0)
                .body(Body::empty())
                .expect("static response is valid");
        }
        Scenario::FailEvery { n } if count % n == 0 => {
            return plain(StatusCode::INTERNAL_SERVER_ERROR);
        }
        _ => {}
    }
    if let Scenario::Latency { ms } = scenario {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    let generation = match scenario {
        Scenario::Changed if count > 1 => count - 1,
        _ => 0,
    };
    let seed = generation_seed(generation);
    let etag = format!("\"{seed:016x}\"");
    let size = shared.size;

    // If-Range: a mismatched validator means the client gets the full body.
    let if_range_ok = req
        .headers()
        .get(header::IF_RANGE)
        .is_none_or(|v| v.to_str().ok() == Some(etag.as_str()));
    let range = match req.headers().get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(value) if scenario != Scenario::NoRange && if_range_ok => parse_range(value, size),
        _ => RangeReq::Full,
    };

    let (status, start, len, content_range) = match range {
        RangeReq::Full => (StatusCode::OK, 0, size, None),
        RangeReq::Partial { start, end } => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{size}")),
        ),
        RangeReq::Unsatisfiable => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .header(header::CONTENT_LENGTH, 0)
                .body(Body::empty())
                .expect("static response is valid");
        }
    };

    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::LAST_MODIFIED, LAST_MODIFIED)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len);
    if let Some(cr) = content_range {
        builder = builder.header(header::CONTENT_RANGE, cr);
    }
    if head {
        return builder.body(Body::empty()).expect("valid response");
    }

    let (chunk, delay, drop_after) = match scenario {
        Scenario::SlowRead => (1024, Some(Duration::from_millis(1)), None),
        Scenario::Capped { bytes_per_sec } => {
            let chunk = (bytes_per_sec / 10).clamp(1, 256 * 1024);
            (chunk, Some(Duration::from_secs_f64(chunk as f64 / bytes_per_sec as f64)), None)
        }
        Scenario::Drops { after_bytes } => (DEFAULT_CHUNK, None, Some(after_bytes)),
        _ => (DEFAULT_CHUNK, None, None),
    };
    let state = BodyState {
        seed,
        offset: start,
        remaining: len,
        sent: 0,
        chunk,
        delay,
        drop_after,
    };
    builder
        .body(Body::from_stream(stream::unfold(state, next_body_item)))
        .expect("valid response")
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn expected_bytes_is_stable_and_clamped() {
        let a = expected_bytes(1 << 20, 4096, 1000);
        let b = expected_bytes(1 << 20, 4096, 1000);
        assert_eq!(a, b);
        assert_eq!(a.len(), 1000);
        // Regenerating a sub-range matches the same slice of a larger read.
        let whole = expected_bytes(1 << 20, 0, 8192);
        assert_eq!(&whole[4096..5096], &a[..]);
        // Clamped at end of file.
        assert_eq!(expected_bytes(100, 90, 50).len(), 10);
    }

    #[test]
    fn scenario_parsing_round_trips() {
        for s in ["fast", "latency:5", "drops:1024", "capped:4096", "refuse:2", "norange", "changed", "slowread", "fail_every:3"] {
            let sc: Scenario = s.parse().expect(s);
            assert_eq!(sc.to_string(), s);
        }
        assert!("capped:0".parse::<Scenario>().is_err());
        assert!("bogus".parse::<Scenario>().is_err());
    }

    #[test]
    fn parse_size_accepts_suffixes() {
        assert_eq!(parse_size("16MiB").unwrap(), 16 << 20);
        assert_eq!(parse_size("1gib").unwrap(), 1 << 30);
        assert_eq!(parse_size("4KiB").unwrap(), 4096);
        assert_eq!(parse_size("123").unwrap(), 123);
        assert!(parse_size("12MB").is_err());
    }

    #[tokio::test]
    async fn middle_range_matches_expected_bytes() {
        let size = 1u64 << 20;
        let lab = start(LabConfig {
            size_bytes: size,
            scenario: Scenario::Fast,
            port: 0,
            path: "/file.bin".into(),
        })
        .await
        .unwrap();

        let (start_b, end_b) = (300_000u64, 300_999u64);
        let mut sock = TcpStream::connect(lab.addr).await.unwrap();
        let req = format!(
            "GET /file.bin HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes={start_b}-{end_b}\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).await.unwrap();

        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("header end");
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(head.contains(&format!("content-range: bytes {start_b}-{end_b}/{size}")), "{head}");
        assert!(head.to_ascii_lowercase().contains("accept-ranges: bytes"), "{head}");
        let body = &raw[split + 4..];
        assert_eq!(body, expected_bytes(size, start_b, end_b - start_b + 1).as_slice());

        lab.shutdown().await.unwrap();
    }
}
