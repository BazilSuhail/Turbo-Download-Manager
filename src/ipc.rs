//! The link between a native-messaging host process and the running GUI.
//!
//! A browser launches the native-messaging host as a **separate short-lived
//! process**, so it has to reach the already-running application somehow.
//!
//! We use a loopback TCP socket. It needs no registration, no platform IPC
//! plumbing, and no elevation, it works the same on every OS, and it keeps the
//! whole mechanism inside one binary. The GUI binds an ephemeral port on `127.0.0.1` and writes the port
//! and a freshly generated token to a file only the user can read; the host
//! reads that file and must present the token on every frame.
//!
//! The token matters: a loopback port is reachable by **any** process on the
//! machine, including a sandboxed one. Without it, any local program could
//! silently queue downloads. It is not a defence against a process that can
//! read the user's own files — nothing at this layer is — but it does stop a
//! stray local listener-prober from driving the application.
//!
//! The same socket gives us a single instance: a second GUI launch sends
//! [`Request::Show`] to the first and exits (see [`run_cli`]).

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Concurrent host connections allowed at once. The real number in use is 1.
const MAX_CONNECTIONS: usize = 8;

/// How long a connection may stall before it is dropped.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Loopback connects either succeed at once or are refused; this only bounds
/// a stale endpoint file whose port now belongs to a stuck process.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a host waits for a GUI it launched (or found starting) to come up
/// before giving the download back to the browser.
const LAUNCH_WAIT: Duration = Duration::from_secs(10);

/// Set by a `Show` request; the UI polls it with [`take_show_request`].
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

/// When an extension last said Hello, for the sidebar's "Connected" pill.
static LAST_HELLO: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether an extension said Hello within `within`.
pub fn hello_within(within: Duration) -> bool {
    LAST_HELLO.lock().ok().and_then(|t| *t).is_some_and(|t| t.elapsed() < within)
}

use serde::{Deserialize, Serialize};

use crate::native_host::{Codec, Request, Response};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("endpoint file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the application does not appear to be running ({0})")]
    NotRunning(PathBuf),
    #[error("rejected: wrong token")]
    BadToken,
    #[error("protocol: {0}")]
    Protocol(#[from] crate::native_host::Error),
}

/// What the GUI publishes so a host process can find and authenticate to it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// One frame on the local socket: a request plus the caller's token.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    token: String,
    request: Request,
}

pub fn endpoint_path() -> PathBuf {
    crate::store::app_dir().join("endpoint.json")
}

/// Whether another launch asked the running window to come to the front since
/// the last call. The UI calls this on its tick, next to draining the [`Inbox`],
/// and raises/focuses its window when it returns `true`.
pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}

/// A token with enough entropy that guessing it is not worthwhile.
///
/// Built from the OS random source where available, falling back to process
/// and clock state. The fallback is weaker, so it is only used when the real
/// source is unavailable rather than as the normal path.
fn make_token() -> String {
    let mut bytes = [0u8; 24];
    if getrandom(&mut bytes).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let mut seed = now ^ (pid << 64) ^ 0x9E37_79B9_7F4A_7C15;
        for b in bytes.iter_mut() {
            // xorshift; adequate only as a last resort.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = (seed & 0xFF) as u8;
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(windows)]
fn getrandom(buf: &mut [u8]) -> Result<(), ()> {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(alg: *mut core::ffi::c_void, out: *mut u8, len: u32, flags: u32) -> i32;
    }
    const USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    // SAFETY: `buf` is a valid, writable slice of exactly `len` bytes, and a
    // null algorithm handle is what USE_SYSTEM_PREFERRED_RNG requires.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 { Ok(()) } else { Err(()) }
}

#[cfg(not(windows))]
fn getrandom(buf: &mut [u8]) -> Result<(), ()> {
    use std::io::Read as _;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .map_err(|_| ())
}

/// A download handed over by a browser, waiting to be picked up by the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    pub url: String,
    pub filename: Option<String>,
    pub headers: Vec<(String, String)>,
    pub size: Option<u64>,
}

/// The queue the server fills and the UI drains on its next tick.
pub type Inbox = Arc<Mutex<Vec<Incoming>>>;

/// Settings the server reports to a connecting extension.
#[derive(Clone, Debug)]
pub struct Advert {
    pub capture_extensions: Vec<String>,
    pub exception_hosts: Vec<String>,
    pub enabled: bool,
}

/// Start listening for host processes. Returns the endpoint it published.
///
/// Runs on its own thread: the work is a blocking accept loop and keeping it
/// off the async runtime avoids any chance of stalling the UI.
pub fn serve(inbox: Inbox, advert: Arc<Mutex<Advert>>) -> Result<Endpoint, Error> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let endpoint = Endpoint { port: listener.local_addr()?.port(), token: make_token() };

    let path = endpoint_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_endpoint(&path, &endpoint)?;

    let expected = endpoint.token.clone();
    // Cap concurrent connections. Each one costs a thread, and the peer is
    // unauthenticated until its first frame arrives — without a cap, any local
    // process could open sockets until the process runs out of threads.
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                // Drop it immediately rather than queueing work for it.
                drop(stream);
                continue;
            }
            // A peer that connects and never speaks must not hold a thread
            // forever; these also bound a half-open connection.
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

            live.fetch_add(1, Ordering::SeqCst);
            // Decrements on drop, so a handler that panics still gives its slot back.
            let slot = LiveSlot(Arc::clone(&live));
            let inbox = Arc::clone(&inbox);
            let advert = Arc::clone(&advert);
            let expected = expected.clone();
            std::thread::spawn(move || {
                let _slot = slot;
                if let Err(e) = handle(stream, inbox, advert, &expected) {
                    tracing::warn!("native host connection ended: {e}");
                }
            });
        }
    });
    Ok(endpoint)
}

/// One occupied slot in the connection limit. Releasing it on drop means a
/// panicking handler cannot leak its slot.
struct LiveSlot(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for LiveSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Publish the endpoint atomically, and on Unix with owner-only permissions.
///
/// The token is the only thing standing between a local process and the
/// ability to queue downloads, so it must not be world-readable, and a reader
/// must never see a half-written file.
fn write_endpoint(path: &std::path::Path, endpoint: &Endpoint) -> Result<(), Error> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // Created owner-only, not chmod-ed afterwards: in between, the token would
    // sit in a file any local user can read.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, &serde_json::to_vec_pretty(endpoint)?)?;
    #[cfg(unix)]
    {
        // A leftover temp file from a crash keeps its old mode on reopen.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    restrict_to_user(&tmp);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// On Windows, drop inherited access and grant only the current user read and
/// write, using the built-in `icacls`. Failure is logged, not fatal.
#[cfg(windows)]
fn restrict_to_user(path: &std::path::Path) {
    let Some(user) = std::env::var_os("USERNAME").filter(|u| !u.is_empty()) else {
        tracing::warn!("USERNAME is not set; the endpoint file keeps its default permissions");
        return;
    };
    let mut grant = user;
    grant.push(":(R,W)");
    match std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(grant)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!("icacls could not restrict the endpoint file ({})", out.status),
        Err(e) => tracing::warn!("could not run icacls on the endpoint file: {e}"),
    }
}

fn handle(
    stream: TcpStream,
    inbox: Inbox,
    advert: Arc<Mutex<Advert>>,
    expected: &str,
) -> Result<(), Error> {
    let read = stream.try_clone()?;
    let mut codec = Codec::new(read, stream);
    while let Some(envelope) = codec.read::<Envelope>()? {
        if !constant_time_eq(envelope.token.as_bytes(), expected.as_bytes()) {
            codec.write(&Response::Error {
                message: "unauthorised".into(),
                request_id: envelope.request.request_id(),
            })?;
            return Err(Error::BadToken);
        }
        let reply = dispatch(envelope.request, &inbox, &advert);
        codec.write(&reply)?;
    }
    Ok(())
}

/// Compare without leaking how many leading bytes matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn dispatch(request: Request, inbox: &Inbox, advert: &Arc<Mutex<Advert>>) -> Response {
    match request {
        Request::Show => {
            SHOW_REQUESTED.store(true, Ordering::SeqCst);
            Response::Accepted { id: 0, request_id: None }
        }
        Request::Hello { .. } => {
            *LAST_HELLO.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
            // Read from disk because the settings are saved on every change and
            // the capture advert does not carry this value.
            let min_kib = crate::store::Store::default_location()
                .load_settings()
                .map_or(0, |s| s.capture_min_size_kib);
            let a = advert.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Response::Ready {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                capture_extensions: a.capture_extensions.clone(),
                exception_hosts: a.exception_hosts.clone(),
                enabled: a.enabled,
                capture_min_size: capture_min_size_bytes(min_kib),
            }
        }
        Request::Query { request_id, url } => {
            let a = advert.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match would_capture(&url, &a) {
                true => Response::Accepted { id: 0, request_id },
                false => Response::Declined { reason: "not a captured type".into(), request_id },
            }
        }
        Request::Download { request_id, url, referer, cookie, user_agent, filename, size } => {
            let mut headers = Vec::new();
            if let Some(v) = referer {
                headers.push(("Referer".to_owned(), v));
            }
            if let Some(v) = cookie {
                headers.push(("Cookie".to_owned(), v));
            }
            if let Some(v) = user_agent {
                headers.push(("User-Agent".to_owned(), v));
            }
            let mut queue = inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.push(Incoming { url, filename, headers, size });
            Response::Accepted { id: queue.len() as u64, request_id }
        }
    }
}

/// Whether a URL matches the advertised capture rules. Mirrors the rule the
/// extension applies, so a `query` answer agrees with what a `download` would do.
pub fn would_capture(url: &str, advert: &Advert) -> bool {
    if !advert.enabled {
        return false;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    if let Some(host) = parsed.host_str() {
        let host = host.to_ascii_lowercase();
        if advert.exception_hosts.iter().any(|h| {
            let h = h.to_ascii_lowercase();
            host == h || host.ends_with(&format!(".{h}"))
        }) {
            return false;
        }
    }
    if advert.capture_extensions.iter().any(|e| e == "*") {
        return true;
    }
    let path = parsed.path();
    let Some((_, ext)) = path.rsplit_once('.') else { return false };
    let ext = ext.to_ascii_lowercase();
    advert.capture_extensions.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
}

/// Client side: forward one browser message to the running GUI and return its
/// reply. Used by `--native-host` mode.
pub fn send(request: &Request) -> Result<Response, Error> {
    send_to(&endpoint_path(), request)
}

fn send_to(path: &Path, request: &Request) -> Result<Response, Error> {
    let bytes = std::fs::read(path).map_err(|_| Error::NotRunning(path.to_owned()))?;
    // `write_endpoint` renames into place, so this is never half-written; a
    // file that still fails to parse is junk from elsewhere: "not running".
    let endpoint: Endpoint =
        serde_json::from_slice(&bytes).map_err(|_| Error::NotRunning(path.to_owned()))?;

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|_| Error::NotRunning(path.to_owned()))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let read = stream.try_clone()?;
    let mut codec = Codec::new(read, stream);
    codec.write(&Envelope { token: endpoint.token, request: request.clone() })?;
    codec
        .read::<Response>()?
        .ok_or_else(|| Error::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)))
}

/// Native-host mode: the browser on stdin/stdout, the GUI over the local socket.
///
/// A download captured while the GUI is closed starts it and waits for it, so
/// the user's click is never dropped. Requests are handled strictly in order,
/// so anything the browser sends meanwhile waits in the pipe and is delivered
/// after it.
pub fn run_host_mode() -> Result<(), Error> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut browser = Codec::new(stdin.lock(), stdout.lock());
    let path = endpoint_path();
    let mut launched = false;

    while let Some(request) = browser.read::<Request>()? {
        let request_id = request.request_id();
        // A failure to reach the GUI is reported to the extension rather than
        // killing the host, so the browser can fall back to its own download.
        let reply = match deliver(&path, &request, &mut launched, &mut launch_gui) {
            Ok(r) => r,
            // The browser says hello at startup, usually before the window is
            // open. Answering from saved settings arms capture now; the first
            // captured download then starts the GUI.
            Err(Error::NotRunning(_)) if matches!(request, Request::Hello { .. }) => {
                offline_ready(&crate::store::Store::default_location())
            }
            Err(e) => Response::Error { message: e.to_string(), request_id },
        };
        browser.write(&reply)?;
    }
    Ok(())
}

/// The `Ready` the GUI would send, built from the settings on disk.
fn offline_ready(store: &crate::store::Store) -> Response {
    let s = store.load_settings().unwrap_or_default();
    Response::Ready {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        capture_extensions: s.capture_extensions,
        exception_hosts: s.exception_hosts,
        enabled: true,
        capture_min_size: capture_min_size_bytes(s.capture_min_size_kib),
    }
}

/// The browser-side size floor in bytes for a setting in KiB; `None` when off.
fn capture_min_size_bytes(kib: u64) -> Option<u64> {
    (kib > 0).then(|| kib.saturating_mul(1024))
}

/// Send one request, starting the GUI first if it carries a download and
/// nothing is listening. `launch` runs at most once per host process, so a
/// GUI that fails to start is not spawned over and over.
///
/// `Hello` and `Query` never launch: the extension sends `Hello` as soon as
/// the browser starts, and opening the window then would be unwelcome.
fn deliver(
    path: &Path,
    request: &Request,
    launched: &mut bool,
    launch: &mut dyn FnMut() -> std::io::Result<()>,
) -> Result<Response, Error> {
    let first = send_to(path, request);
    if !matches!(first, Err(Error::NotRunning(_)))
        || !matches!(request, Request::Download { .. } | Request::Show)
    {
        return first;
    }
    if !*launched {
        *launched = true;
        launch()?;
    }
    let deadline = Instant::now() + LAUNCH_WAIT;
    let mut delay = Duration::from_millis(100);
    loop {
        std::thread::sleep(delay);
        match send_to(path, request) {
            Err(Error::NotRunning(_)) if Instant::now() < deadline => {
                delay = (delay * 2).min(Duration::from_secs(1));
            }
            other => return other,
        }
    }
}

/// Start the GUI as an independent process: no inherited stdio (that is the
/// browser's pipe) and outside the host's lifetime.
fn launch_gui() -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Chrome runs hosts inside a job object that is killed when the port
        // closes; breaking away keeps the GUI alive. A job may forbid that,
        // in which case the spawn fails and we retry without it.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group, so a browser signalling the host's group on
        // disconnect does not take the GUI with it.
        cmd.process_group(0);
    }
    cmd.spawn().map(drop)
}

/// Whether these arguments are a browser starting us as a native host.
///
/// Browsers do not pass `--native-host`: Chromium passes the caller's origin
/// (`chrome-extension://<id>/`, plus `--parent-window=` on Windows) and
/// Firefox passes the manifest path and the add-on id.
pub fn is_host_invocation(args: &[String]) -> bool {
    let first = args.first().map(String::as_str).unwrap_or("");
    first == "--native-host"
        || first.starts_with("chrome-extension://")
        || args.get(1).is_some_and(|a| a == crate::register::FIREFOX_EXTENSION_ID)
}

/// Ask a running GUI to show itself. `true` means one answered and this
/// process should exit instead of opening a second window.
///
/// shortcut: two launches in the same instant can both miss each other and
/// both start (the later one owns the endpoint file); a lock file or named
/// mutex closes that if it is ever seen in practice.
pub fn hand_off_to_running() -> bool {
    matches!(send(&Request::Show), Ok(Response::Accepted { .. }))
}

/// Every non-GUI entry point, in one place so `main` stays a one-liner:
///
/// ```ignore
/// let args: Vec<String> = std::env::args().skip(1).collect();
/// if let Some(code) = tdm::ipc::run_cli(&args) { std::process::exit(code) }
/// ```
///
/// Returns the exit code when the process should end without a window
/// (native-host mode, `--register-host [ids]`, `--unregister-host`, or a plain
/// launch handed off to an already-running instance), `None` to start the GUI.
pub fn run_cli(args: &[String]) -> Option<i32> {
    if is_host_invocation(args) {
        return Some(match run_host_mode() {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!("native host: {e}");
                1
            }
        });
    }
    match args.first().map(String::as_str) {
        Some("--register-host") => Some(match crate::register::install(&args[1..]) {
            Ok(path) => {
                println!("registered {} -> {}", crate::register::HOST_NAME, path.display());
                0
            }
            Err(e) => {
                eprintln!("registration failed: {e}");
                1
            }
        }),
        Some("--unregister-host") => Some(match crate::register::uninstall() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("unregistration failed: {e}");
                1
            }
        }),
        None => hand_off_to_running().then_some(0),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advert() -> Advert {
        Advert {
            capture_extensions: vec!["zip".into(), "mp4".into()],
            exception_hosts: vec!["intranet.test".into()],
            enabled: true,
        }
    }

    #[test]
    fn tokens_are_long_and_do_not_repeat() {
        let a = make_token();
        let b = make_token();
        assert_eq!(a.len(), 48, "expected 24 bytes as hex");
        assert_ne!(a, b, "two tokens in a row were identical");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn capture_rule_accepts_listed_extensions_only() {
        let a = advert();
        assert!(would_capture("https://e.test/a.zip", &a));
        assert!(would_capture("https://e.test/path/clip.MP4", &a));
        assert!(!would_capture("https://e.test/page.html", &a));
        assert!(!would_capture("https://e.test/noextension", &a));
    }

    #[test]
    fn the_star_rule_takes_every_http_download() {
        let a = Advert { capture_extensions: vec!["*".into()], exception_hosts: vec![], enabled: true };
        assert!(would_capture("https://github.com/x/releases/download/v1/det_500m.onnx", &a));
        assert!(would_capture("https://example.com/download.php?id=7", &a));
        assert!(!would_capture("ftp://example.com/a.onnx", &a));
    }

    #[test]
    fn capture_rule_skips_exception_hosts_and_their_subdomains() {
        let a = advert();
        assert!(!would_capture("https://intranet.test/a.zip", &a));
        assert!(!would_capture("https://files.intranet.test/a.zip", &a));
        // A lookalike is not the same host.
        assert!(would_capture("https://notintranet.test/a.zip", &a));
    }

    #[test]
    fn capture_rule_rejects_non_http_schemes() {
        let a = advert();
        for url in ["file:///c:/a.zip", "ftp://e.test/a.zip", "data:text/plain,a.zip"] {
            assert!(!would_capture(url, &a), "{url} should not be captured");
        }
    }

    #[test]
    fn capture_is_off_when_disabled() {
        let mut a = advert();
        a.enabled = false;
        assert!(!would_capture("https://e.test/a.zip", &a));
    }

    #[test]
    fn an_empty_capture_list_captures_nothing() {
        let a = Advert {
            capture_extensions: vec![],
            exception_hosts: vec![],
            enabled: true,
        };
        assert!(!would_capture("https://e.test/a.zip", &a));
    }

    #[test]
    fn a_download_request_lands_in_the_inbox_with_its_headers() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let reply = dispatch(
            Request::Download {
                request_id: Some(7),
                url: "https://e.test/a.zip".into(),
                referer: Some("https://e.test/".into()),
                cookie: Some("s=1".into()),
                user_agent: Some("ua".into()),
                filename: Some("a.zip".into()),
                size: Some(99),
            },
            &inbox,
            &advert,
        );
        assert!(matches!(reply, Response::Accepted { request_id: Some(7), .. }));

        let queue = inbox.lock().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].url, "https://e.test/a.zip");
        assert_eq!(queue[0].size, Some(99));
        let names: Vec<&str> = queue[0].headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Referer", "Cookie", "User-Agent"]);
    }

    #[test]
    fn the_correlation_id_is_echoed_on_every_reply_kind() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let accepted = dispatch(
            Request::Query { request_id: Some(1), url: "https://e.test/a.zip".into() },
            &inbox,
            &advert,
        );
        assert!(matches!(accepted, Response::Accepted { request_id: Some(1), .. }));

        let declined = dispatch(
            Request::Query { request_id: Some(2), url: "https://e.test/a.html".into() },
            &inbox,
            &advert,
        );
        assert!(matches!(declined, Response::Declined { request_id: Some(2), .. }));
    }

    #[test]
    fn hello_reports_the_current_capture_rules() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        match dispatch(Request::Hello { version: "1.0".into() }, &inbox, &advert) {
            Response::Ready { capture_extensions, exception_hosts, enabled, .. } => {
                assert_eq!(capture_extensions, vec!["zip", "mp4"]);
                assert_eq!(exception_hosts, vec!["intranet.test"]);
                assert!(enabled);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn browser_launch_arguments_are_recognised() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_host_invocation(&a(&["--native-host"])));
        assert!(is_host_invocation(&a(&[
            "chrome-extension://glkodjlchifmdbokmofigopbgdbabmge/",
            "--parent-window=1234"
        ])));
        assert!(is_host_invocation(&a(&["/home/u/.mozilla/x.json", "tdm@tdm.app"])));
        assert!(!is_host_invocation(&a(&[])));
        assert!(!is_host_invocation(&a(&["--register-host"])));
        assert!(!is_host_invocation(&a(&["/some/file.zip", "other@addon"])));
    }

    #[test]
    fn a_show_request_raises_the_flag_once_and_queues_nothing() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let reply = dispatch(Request::Show, &inbox, &advert);
        assert!(matches!(reply, Response::Accepted { .. }));
        assert!(take_show_request());
        assert!(!take_show_request(), "the flag must reset once read");
        assert!(inbox.lock().unwrap().is_empty());
    }

    fn temp_endpoint(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tdm-ipc-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("endpoint.json")
    }

    fn download(id: u64) -> Request {
        Request::Download {
            request_id: Some(id),
            url: "https://e.test/late.zip".into(),
            referer: None,
            cookie: None,
            user_agent: None,
            filename: None,
            size: None,
        }
    }

    #[test]
    fn a_download_waits_for_the_gui_it_launched() {
        let path = temp_endpoint("launch");
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let srv_inbox = Arc::clone(&inbox);
        let srv_path = path.clone();
        let mut launches = 0;
        let mut launch = || {
            launches += 1;
            // The "GUI" takes a while to come up, as a real one does.
            let (inbox, path) = (Arc::clone(&srv_inbox), srv_path.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(400));
                let listener =
                    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
                let endpoint = Endpoint {
                    port: listener.local_addr().unwrap().port(),
                    token: make_token(),
                };
                write_endpoint(&path, &endpoint).unwrap();
                let (stream, _) = listener.accept().unwrap();
                let _ = handle(stream, inbox, Arc::new(Mutex::new(advert())), &endpoint.token);
            });
            Ok(())
        };
        let mut launched = false;
        let reply = deliver(&path, &download(5), &mut launched, &mut launch).expect("delivered");
        assert!(matches!(reply, Response::Accepted { request_id: Some(5), .. }), "{reply:?}");
        assert_eq!(launches, 1);
        assert_eq!(inbox.lock().unwrap()[0].url, "https://e.test/late.zip");
    }

    #[test]
    fn a_closed_gui_still_answers_hello_from_saved_settings() {
        let dir = std::env::temp_dir().join(format!("tdm-offline-{}", std::process::id()));
        let store = crate::store::Store::at(&dir);
        let s = crate::model::Settings { capture_extensions: vec!["iso".into()], ..Default::default() };
        store.save_settings(&s).expect("saved");
        match offline_ready(&store) {
            Response::Ready { capture_extensions, enabled, .. } => {
                assert_eq!(capture_extensions, vec!["iso".to_owned()]);
                assert!(enabled);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hello_never_launches_the_gui() {
        let path = temp_endpoint("hello");
        let mut launch = || -> std::io::Result<()> { panic!("hello must not launch the GUI") };
        let mut launched = false;
        let r = deliver(&path, &Request::Hello { version: "1".into() }, &mut launched, &mut launch);
        assert!(matches!(r, Err(Error::NotRunning(_))), "{r:?}");
        assert!(!launched);
    }

    #[test]
    fn a_failed_launch_is_reported_and_not_retried() {
        let path = temp_endpoint("fail");
        let mut calls = 0;
        let mut launch = || {
            calls += 1;
            Err(std::io::Error::other("no exe"))
        };
        let mut launched = false;
        assert!(deliver(&path, &download(1), &mut launched, &mut launch).is_err());
        assert!(launched);
        assert_eq!(calls, 1);
    }

    /// End to end over a real loopback socket, including the token check.
    #[test]
    fn a_host_process_can_hand_a_download_to_the_server() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "unit-test-token".to_owned();

        let expected = token.clone();
        let srv_inbox = Arc::clone(&inbox);
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = handle(stream, srv_inbox, advert, &expected);
        });

        let stream = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).unwrap();
        let read = stream.try_clone().unwrap();
        let mut codec = Codec::new(read, stream);
        codec
            .write(&Envelope {
                token,
                request: Request::Download {
                    request_id: Some(42),
                    url: "https://e.test/big.zip".into(),
                    referer: None,
                    cookie: None,
                    user_agent: None,
                    filename: None,
                    size: None,
                },
            })
            .unwrap();
        let reply: Response = codec.read().unwrap().unwrap();
        assert!(matches!(reply, Response::Accepted { request_id: Some(42), .. }));
        assert_eq!(inbox.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_wrong_token_is_refused_and_queues_nothing() {
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let port = listener.local_addr().unwrap().port();

        let srv_inbox = Arc::clone(&inbox);
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = handle(stream, srv_inbox, advert, "the-real-token");
        });

        let stream = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).unwrap();
        let read = stream.try_clone().unwrap();
        let mut codec = Codec::new(read, stream);
        codec
            .write(&Envelope {
                token: "guessed".into(),
                request: Request::Download {
                    request_id: None,
                    url: "https://e.test/a.zip".into(),
                    referer: None,
                    cookie: None,
                    user_agent: None,
                    filename: None,
                    size: None,
                },
            })
            .unwrap();
        let reply: Response = codec.read().unwrap().unwrap();
        assert!(matches!(reply, Response::Error { .. }), "got {reply:?}");
        assert!(inbox.lock().unwrap().is_empty(), "an unauthorised request was queued");
    }

    #[test]
    fn the_size_floor_round_trips_through_ready() {
        let ready = |min| Response::Ready {
            version: "1".into(),
            capture_extensions: vec!["*".into()],
            exception_hosts: vec![],
            enabled: true,
            capture_min_size: min,
        };
        let on = ready(capture_min_size_bytes(512));
        let json = serde_json::to_string(&on).unwrap();
        assert!(json.contains("\"captureMinSize\":524288"), "{json}");
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), on);

        // Off is left out of the frame, so an older extension sees no floor.
        let off = ready(capture_min_size_bytes(0));
        let json = serde_json::to_string(&off).unwrap();
        assert!(!json.contains("captureMinSize"), "{json}");
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), off);
    }

    #[test]
    fn the_size_floor_is_kib_and_zero_means_off() {
        assert_eq!(capture_min_size_bytes(0), None);
        assert_eq!(capture_min_size_bytes(1), Some(1024));
        assert_eq!(capture_min_size_bytes(u64::MAX), Some(u64::MAX), "saturates instead of wrapping");
    }

    #[test]
    fn a_hello_frame_is_read_and_answered_with_ready() {
        let hello: Request = serde_json::from_str(r#"{"type":"hello","version":"1.0"}"#).unwrap();
        assert_eq!(hello, Request::Hello { version: "1.0".into() });
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(advert()));
        let reply = dispatch(hello, &inbox, &advert);
        let json = serde_json::to_string(&reply).unwrap();
        assert!(json.contains("\"type\":\"ready\""), "{json}");
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), reply);
    }
}
