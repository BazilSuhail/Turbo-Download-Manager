//! FTP transport: control-connection protocol, LIST parsing, and ranged `RETR`.
//!
//! The shape matters for the engine: FTP is segmented exactly like HTTP, but
//! each worker needs **its own control connection and its own data
//! connection**, uses `REST <start>` in place of `Range`, and enforces the
//! segment end locally by closing the data socket — FTP has no end offset on
//! the wire. So [`FtpDownload::fetch_range`] is one whole session per segment.
//!
//! `ftp://` is plaintext. `ftps://` is parsed into [`TlsMode`]: implicit TLS on
//! port 990 by default, or explicit `AUTH TLS` when the port is 21. Both modes
//! then send `PBSZ 0` and `PROT P`, so the data connections are TLS too. The
//! certificate is checked against the platform trust store. A failed handshake
//! or a refused `AUTH` is an error. There is no plaintext fallback.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use rustls_platform_verifier::BuilderVerifierExt;
use suppaftp::tokio_rustls::TlsConnector;
use suppaftp::tokio_rustls::rustls::{self, ClientConfig, pki_types::ServerName};
use tokio::fs::File;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::TcpStream;

/// Cap on a single control read or write: just under the common 30 s idle
/// timeouts, so we give up on a silent control socket before a middlebox does.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(29);

/// Password we send for anonymous logins (the conventional `user@` form).
pub const ANONYMOUS_PASSWORD: &str = "tdm@";

const READ_CHUNK: usize = 64 * 1024;

/// A directory listing is a size fallback, not a feature, so it is capped
/// (~11.7 KB) rather than buffered without bound.
const LIST_LIMIT: usize = 0x2da0;

#[derive(Debug, thiserror::Error)]
pub enum FtpError {
    #[error("network error: {0}")]
    Io(#[from] std::io::Error),
    /// 5xx — the server will answer the same way next time.
    #[error("server refused: {code} {text}")]
    Permanent { code: u16, text: String },
    /// 4xx — transient; worth retrying.
    #[error("server busy: {code} {text}")]
    Transient { code: u16, text: String },
    #[error("expected {want}, got {code} {text}")]
    Unexpected { want: &'static str, code: u16, text: String },
    #[error("server did not support REST, so this range cannot be fetched")]
    NoRest,
    #[error("malformed reply from the server")]
    BadReply,
    #[error("no usable data address in the PASV reply")]
    BadPasv,
    #[error("not a usable FTP URL: {0}")]
    BadUrl(String),
    #[error("control connection timed out")]
    Timeout,
    #[error("asked for an empty byte range")]
    EmptyRange,
    /// The TLS handshake or the certificate check failed. Never retried in plaintext.
    #[error("TLS failed: {0}")]
    Tls(String),
    #[error("connection ended early after {got} of {expected} bytes")]
    ShortRead { got: u64, expected: u64 },
}

impl FtpError {
    /// Whether retrying the same request could succeed. 4xx and anything at
    /// the socket level is retryable; 5xx and our own parse failures are not.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            FtpError::Io(_)
                | FtpError::Transient { .. }
                | FtpError::Timeout
                | FtpError::ShortRead { .. }
        )
    }
}

/// One complete server reply, with the code and the text of every line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    /// Text after the code. Lines of a multi-line reply are joined with `\n`.
    pub text: String,
}

impl Reply {
    /// The reply as it looked on the wire, for parsers that scan the raw line
    /// (the PASV parser does, and it starts at offset 4).
    fn raw(&self) -> String {
        format!("{} {}", self.code, self.text)
    }

    fn into_error(self, want: &'static str) -> FtpError {
        let Reply { code, text } = self;
        match code / 100 {
            5 => FtpError::Permanent { code, text },
            4 => FtpError::Transient { code, text },
            _ => FtpError::Unexpected { want, code, text },
        }
    }
}

/// Assembles one reply from lines, handling the `NNN-...` continuation form.
///
/// A multi-line reply ends only at a line with the *same* code followed by a
/// space; intermediate lines may look like anything, including other codes.
/// Kept separate from the socket so the rule can be tested directly.
#[derive(Default)]
struct ReplyReader {
    /// Set while a `NNN-` block is open.
    open: Option<u16>,
    text: String,
}

impl ReplyReader {
    /// Returns the finished reply when `line` completes it.
    fn push(&mut self, line: &str) -> Result<Option<Reply>, FtpError> {
        let line = line.trim_end_matches(['\r', '\n']);
        match self.open {
            None => {
                let (code, sep) = split_code(line).ok_or(FtpError::BadReply)?;
                self.text.push_str(line.get(4..).unwrap_or(""));
                if sep == b'-' {
                    self.open = Some(code);
                    return Ok(None);
                }
                Ok(Some(self.finish(code)))
            }
            Some(open) => {
                self.text.push('\n');
                if split_code(line) == Some((open, b' ')) {
                    self.text.push_str(line.get(4..).unwrap_or(""));
                    self.open = None;
                    return Ok(Some(self.finish(open)));
                }
                self.text.push_str(line);
                Ok(None)
            }
        }
    }

    fn finish(&mut self, code: u16) -> Reply {
        Reply { code, text: std::mem::take(&mut self.text) }
    }
}

/// `("213 1234")` -> `(213, b' ')`. `None` unless the line really starts with
/// three digits and a space or hyphen.
fn split_code(line: &str) -> Option<(u16, u8)> {
    let b = line.as_bytes();
    if b.len() < 3 || !b[..3].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let sep = match b.get(3) {
        None => b' ',
        Some(&c @ (b' ' | b'-')) => c,
        Some(_) => return None,
    };
    Some((line[..3].parse().ok()?, sep))
}

/// Parse the data address out of a PASV reply.
///
/// A deliberately lenient rule: the reply must start with
/// `'2'`, then from offset 4 the first position that parses as six
/// comma-separated numbers wins. **Parentheses are not required** and nothing
/// else in the line is inspected, so `227 Mode 10,0,0,1,4,1` is accepted.
pub fn parse_pasv_reply(reply: &str) -> Option<SocketAddrV4> {
    if reply.as_bytes().first() != Some(&b'2') {
        return None;
    }
    let bytes = reply.as_bytes();
    (4..bytes.len())
        .filter(|i| bytes[*i].is_ascii_digit() && reply.is_char_boundary(*i))
        .find_map(|i| six_numbers(&reply[i..]))
}

/// `h1,h2,h3,h4,p1,p2` at the start of `s`, with port `p1 * 256 + p2`.
///
/// Mirrors `sscanf("%u,%u,%u,%u,%u,%u")`: leading blanks before a number are
/// skipped but the comma must come straight after the digits, which is what
/// stops `Mode 1 (10,0,0,1,4,1)` from being read as `1.0.0.1`.
fn six_numbers(s: &str) -> Option<SocketAddrV4> {
    let mut parts = s.split(',');
    let mut n = [0u8; 6];
    for (i, slot) in n.iter_mut().enumerate() {
        let part = parts.next()?.trim_start();
        let end = part.find(|c: char| !c.is_ascii_digit()).unwrap_or(part.len());
        if end == 0 || (i < 5 && end != part.len()) {
            return None;
        }
        *slot = part[..end].parse::<u16>().ok().filter(|v| *v <= u8::MAX.into())? as u8;
    }
    Some(SocketAddrV4::new(
        Ipv4Addr::new(n[0], n[1], n[2], n[3]),
        u16::from(n[4]) * 256 + u16::from(n[5]),
    ))
}

/// One line of a `LIST` response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtpEntry {
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
}

/// Parse one listing line, dispatching on its first character: EPLF (`+`), Unix `ls -l` (`-bcdlps`), otherwise VMS.
///
/// DOS/IIS listings (`03-07-13 11:19AM <DIR> name`) are deliberately **not**
/// parsed — such servers have to answer `SIZE`. Returning `None` here keeps that behaviour
/// instead of inventing a format the spec says is unsupported.
pub fn parse_list_line(line: &str) -> Option<FtpEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.as_bytes().first()? {
        b'+' => parse_eplf(line),
        b'-' | b'b' | b'c' | b'd' | b'l' | b'p' | b's' => parse_unix(line),
        _ => parse_vms(line),
    }
}

/// `+i8388621.29609,m824255902,r,s10876,\tREADME`
fn parse_eplf(line: &str) -> Option<FtpEntry> {
    let (facts, name) = line[1..].split_once('\t')?;
    let mut entry = FtpEntry { name: name.to_owned(), size: None, is_dir: false };
    if entry.name.is_empty() {
        return None;
    }
    for fact in facts.split(',') {
        match fact.as_bytes().first() {
            Some(b'/') => entry.is_dir = true,
            Some(b's') => entry.size = fact[1..].parse().ok(),
            // `r` (retrievable), `m` (mtime) and `i` (unique id) carry nothing
            // we need; unknown letters are skipped.
            _ => {}
        }
    }
    Some(entry)
}

/// `-rw-r--r--   1 owner group    1234 Jan 01 12:00 name`
///
/// Token 0 is the mode, tokens 1..=3 are skipped, token 4 is the size, then
/// month / day / time-or-year, and the rest of the line is the name. One
/// extra token is skipped when token 2 is six characters starting with `f`,
/// to cope with servers that insert a field there.
fn parse_unix(line: &str) -> Option<FtpEntry> {
    let kind = line.as_bytes()[0];
    let mut rest = line;
    let mut fields = Vec::new();
    while fields.len() < 4 {
        let (tok, tail) = next_token(rest)?;
        fields.push(tok);
        rest = tail;
    }
    if fields[2].len() == 6 && fields[2].starts_with('f') {
        rest = next_token(rest)?.1;
    }
    let (size, rest) = next_token(rest)?;
    // month, day, time-or-year
    let (_, rest) = next_token(rest)?;
    let (_, rest) = next_token(rest)?;
    let (stamp, rest) = next_token(rest)?;
    // Guard against a line that ran out of columns and left us holding the
    // name as the "size": the stamp is always a time or a year.
    if !stamp.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    // Leading blanks in the name are dropped here, which also covers servers
    // that pad the name column.
    let mut name = rest.trim_start();
    if kind == b'l' {
        // A symlink's target is not part of its name.
        name = name.split(" -> ").next().unwrap_or(name);
    }
    if name.is_empty() {
        return None;
    }
    Some(FtpEntry {
        name: name.to_owned(),
        size: size.parse().ok(),
        // `d` is a directory; a symlink (`l`) is something we can `RETR`, so
        // it counts as a file here.
        is_dir: kind == b'd',
        })
}

/// `README.TXT;1      2  12-APR-2001 10:00:00  [GROUP,USER]`
///
/// Requires a `;`. The size column is in blocks, not bytes, so it is dropped
/// rather than reported as a byte count.
fn parse_vms(line: &str) -> Option<FtpEntry> {
    let (name, _) = line.split_once(';')?;
    let name = name.trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    match name.strip_suffix(".DIR") {
        Some(dir) if !dir.is_empty() => {
            Some(FtpEntry { name: dir.to_owned(), size: None, is_dir: true })
        }
        _ => Some(FtpEntry { name: name.to_owned(), size: None, is_dir: false }),
    }
}

/// Next whitespace-delimited token and the remainder after it.
fn next_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// How the control connection is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// `ftp://`: no encryption.
    Plain,
    /// `ftps://host:21`: plaintext greeting, then `AUTH TLS` (RFC 4217).
    Explicit,
    /// `ftps://` on any other port, 990 by default: TLS from the first byte.
    Implicit,
}

/// `ftp://` or `ftps://` `[user[:pass]@]host[:port]/dir/file`, split up.
///
/// `Debug` is hand-written and redacts the password: a derived one would put
/// credentials into every `{:?}`, panic message and log line that touches a
/// URL.
#[derive(Clone, PartialEq, Eq)]
pub struct FtpUrl {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    /// Directory to `CWD` into, without a trailing slash. Empty for a file at
    /// the root.
    pub dir: String,
    pub file: String,
    pub tls: TlsMode,
}

impl fmt::Debug for FtpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl fmt::Display for FtpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl FtpUrl {
    /// Parse an `ftp://` or `ftps://` URL. Missing credentials default to the
    /// anonymous login (`anonymous` / [`ANONYMOUS_PASSWORD`]).
    pub fn parse(url: &str) -> Result<Self, FtpError> {
        let bad = || FtpError::BadUrl(redact_url(url));
        let (ftps, rest) = if let Some(rest) =
            url.strip_prefix("ftp://").or_else(|| url.strip_prefix("FTP://"))
        {
            (false, rest)
        } else if let Some(rest) =
            url.strip_prefix("ftps://").or_else(|| url.strip_prefix("FTPS://"))
        {
            (true, rest)
        } else {
            return Err(bad());
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        // The last `@` separates credentials, so an unescaped `@` in a
        // password does not cut the host off.
        let (creds, hostport) = match authority.rsplit_once('@') {
            Some((creds, hostport)) => (Some(creds), hostport),
            None => (None, authority),
        };
        let (user, pass) = match creds {
            Some(creds) => match creds.split_once(':') {
                Some((u, p)) => (u.to_owned(), p.to_owned()),
                None => (creds.to_owned(), String::new()),
            },
            None => ("anonymous".to_owned(), ANONYMOUS_PASSWORD.to_owned()),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p.parse::<u16>().map_err(|_| bad())?)),
            None => (hostport, None),
        };
        let (tls, port) = match (ftps, port) {
            (false, port) => (TlsMode::Plain, port.unwrap_or(21)),
            // `ftps://host:21` asks for explicit AUTH TLS on the FTP port.
            (true, Some(21)) => (TlsMode::Explicit, 21),
            (true, port) => (TlsMode::Implicit, port.unwrap_or(990)),
        };
        if host.is_empty() || user.is_empty() {
            return Err(bad());
        }
        let path = path.trim_start_matches('/');
        let (dir, file) = match path.rsplit_once('/') {
            Some((d, f)) => (format!("/{d}"), f),
            None => (String::new(), path),
        };
        if file.is_empty() {
            return Err(bad());
        }
        if [user.as_str(), pass.as_str(), dir.as_str(), file].iter().any(|part| has_control_char(part)) {
            return Err(bad());
        }
        Ok(FtpUrl {
            host: host.to_owned(),
            port,
            user,
            pass,
            dir,
            file: file.to_owned(),
            tls,
        })
    }

    /// The URL with any credentials replaced. Use this, never the raw URL, in
    /// anything a user or a log file will see.
    pub fn redacted(&self) -> String {
        let creds = if self.user == "anonymous" { String::new() } else { "***@".to_owned() };
        let scheme = if self.tls == TlsMode::Plain { "ftp" } else { "ftps" };
        format!("{scheme}://{}{}:{}{}/{}", creds, self.host, self.port, self.dir, self.file)
    }
}

/// Strip the userinfo out of a URL-shaped string we could not parse.
///
/// A password may contain `/`, so the first `/` does not mark the end of the
/// userinfo. The last `@` is taken as the separator instead, which is the one
/// `FtpUrl::parse` uses as well.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("//") else {
        return url.to_owned();
    };
    match rest.rfind('@') {
        Some(at) => format!("{scheme}//***@{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// Whether a credential or path part carries a control character. Those have
/// no business in an FTP command line, and a CR or LF there would let a
/// crafted URL inject extra commands into the control connection.
fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| c < '\u{20}' || c == '\u{7f}')
}

/// What the engine needs before it can plan segments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FtpProbe {
    pub size: Option<u64>,
    /// `REST` answered 350, so segmented download is possible. Without it the
    /// file has to be fetched as one stream from 0.
    pub supports_rest: bool,
    /// Raw `MDTM` stamp (`YYYYMMDDhhmmss`), when the server gave one.
    pub modified: Option<String>,
}

/// Any control or data connection: plain TCP, or TLS over TCP.
pub trait Wire: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Wire for T {}
pub type Conn = Box<dyn Wire>;

/// TLS settings for one `ftps://` session. The connector is shared by the
/// control and data connections, so the data channel can resume the control
/// channel's TLS session, which servers that require session reuse need.
struct Tls {
    connector: TlsConnector,
    host: String,
}

impl Tls {
    /// Run the client handshake over `stream`, checking the certificate for `host`.
    async fn handshake<S>(&self, stream: S) -> Result<Conn, FtpError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let name = ServerName::try_from(self.host.clone())
            .map_err(|e| FtpError::Tls(e.to_string()))?;
        let tls = tokio::time::timeout(CONTROL_TIMEOUT, self.connector.connect(name, stream))
            .await
            .map_err(|_| FtpError::Timeout)?
            .map_err(|e| FtpError::Tls(e.to_string()))?;
        let conn: Conn = Box::new(tls);
        Ok(conn)
    }
}

/// A connector that checks certificates against the platform trust store, the
/// same one reqwest uses. Nothing here turns verification off.
fn tls_connector() -> Result<TlsConnector, FtpError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_platform_verifier())
        .map(|builder| builder.with_no_client_auth())
        .map_err(|e| FtpError::Tls(e.to_string()))?;
    Ok(TlsConnector::from(Arc::new(config)))
}

/// A logged-in control connection in binary mode.
pub struct FtpControl {
    reader: BufReader<Conn>,
    timeout: Duration,
    /// Set once `PROT P` is accepted. Data connections are TLS from then on.
    tls: Option<Tls>,
}

impl FtpControl {
    /// Connect, read the greeting, log in, and switch to `TYPE I`.
    ///
    /// For `ftps://`, TLS is in place before `USER` is sent: implicit TLS
    /// handshakes on connect, explicit TLS upgrades after `AUTH TLS`.
    pub async fn connect(url: &FtpUrl) -> Result<Self, FtpError> {
        let tls = match url.tls {
            TlsMode::Plain => None,
            TlsMode::Explicit | TlsMode::Implicit => {
                Some(Tls { connector: tls_connector()?, host: url.host.clone() })
            }
        };
        let stream = tokio::time::timeout(
            CONTROL_TIMEOUT,
            TcpStream::connect((url.host.as_str(), url.port)),
        )
        .await
        .map_err(|_| FtpError::Timeout)??;
        let conn: Conn = match tls.as_ref() {
            Some(tls) if url.tls == TlsMode::Implicit => tls.handshake(stream).await?,
            _ => Box::new(stream),
        };
        let mut control =
            FtpControl { reader: BufReader::new(conn), timeout: CONTROL_TIMEOUT, tls: None };

        let greeting = control.reply().await?;
        if greeting.code != 220 {
            return Err(greeting.into_error("220 greeting"));
        }
        if url.tls == TlsMode::Explicit {
            let reply = control.send("AUTH TLS").await?;
            if reply.code != 234 {
                return Err(reply.into_error("234 after AUTH TLS"));
            }
            let tls = tls.as_ref().ok_or_else(|| FtpError::Tls("no TLS settings".into()))?;
            control = control.upgrade(tls).await?;
        }
        if let Some(tls) = tls {
            control = control.protect(tls).await?;
        }
        let reply = control.send(&format!("USER {}", url.user)).await?;
        match reply.code {
            // 230 means the server wants no password, so PASS is skipped.
            230 => {}
            331 | 332 => {
                let reply = control.send(&format!("PASS {}", url.pass)).await?;
                if reply.code / 100 != 2 {
                    return Err(reply.into_error("230 after PASS"));
                }
            }
            _ => return Err(reply.into_error("230 or 331 after USER")),
        }
        let reply = control.send("TYPE I").await?;
        if reply.code / 100 != 2 {
            return Err(reply.into_error("200 after TYPE I"));
        }
        if !url.dir.is_empty() {
            let reply = control.send(&format!("CWD {}", url.dir)).await?;
            if reply.code / 100 != 2 {
                return Err(reply.into_error("250 after CWD"));
            }
        }
        Ok(control)
    }

    /// Replace the plaintext control stream with TLS (`AUTH TLS` succeeded).
    async fn upgrade(self, tls: &Tls) -> Result<Self, FtpError> {
        // Bytes buffered past the 234 reply would be read as plaintext: refuse them.
        if !self.reader.buffer().is_empty() {
            return Err(FtpError::BadReply);
        }
        let conn = tls.handshake(self.reader.into_inner()).await?;
        Ok(FtpControl { reader: BufReader::new(conn), timeout: self.timeout, tls: None })
    }

    /// `PBSZ 0` and `PROT P`: the data connections are TLS from here on.
    async fn protect(mut self, tls: Tls) -> Result<Self, FtpError> {
        for (command, want) in [("PBSZ 0", "200 after PBSZ"), ("PROT P", "200 after PROT P")] {
            let reply = self.send(command).await?;
            if reply.code / 100 != 2 {
                return Err(reply.into_error(want));
            }
        }
        self.tls = Some(tls);
        Ok(self)
    }

    /// Send one command and read its reply.
    ///
    /// The command is never logged: it may be a `PASS` line.
    pub async fn send(&mut self, command: &str) -> Result<Reply, FtpError> {
        let line = format!("{command}\r\n");
        tokio::time::timeout(self.timeout, self.reader.get_mut().write_all(line.as_bytes()))
            .await
            .map_err(|_| FtpError::Timeout)??;
        self.reply().await
    }

    /// Read one reply, following `NNN-` continuations.
    pub async fn reply(&mut self) -> Result<Reply, FtpError> {
        let mut assembler = ReplyReader::default();
        loop {
            let mut raw = Vec::new();
            let read = self.reader.read_until(b'\n', &mut raw);
            let n = tokio::time::timeout(self.timeout, read)
                .await
                .map_err(|_| FtpError::Timeout)??;
            if n == 0 {
                return Err(FtpError::BadReply);
            }
            // A server is free to put a non-UTF-8 filename in a reply; that
            // must not take the connection down.
            let line = String::from_utf8_lossy(&raw);
            if let Some(reply) = assembler.push(&line)? {
                return Ok(reply);
            }
        }
    }

    /// `SIZE`. `None` when the server has no size for us — the caller then
    /// falls back to `LIST`.
    pub async fn size(&mut self, file: &str) -> Result<Option<u64>, FtpError> {
        let reply = self.send(&format!("SIZE {file}")).await?;
        Ok((reply.code == 213).then(|| reply.text.trim().parse().ok()).flatten())
    }

    /// `MDTM`, as a raw `YYYYMMDDhhmmss` stamp.
    pub async fn modified(&mut self, file: &str) -> Result<Option<String>, FtpError> {
        let reply = self.send(&format!("MDTM {file}")).await?;
        Ok((reply.code == 213).then(|| reply.text.trim().to_owned()))
    }

    /// Prove `REST` works by asking for offset 0 and requiring a 350.
    pub async fn supports_rest(&mut self) -> Result<bool, FtpError> {
        Ok(self.send("REST 0").await?.code == 350)
    }

    /// `PASV`, then connect the data socket.
    ///
    /// Active mode (`PORT`) is not implemented; see the module report.
    pub async fn open_data(&mut self) -> Result<Conn, FtpError> {
        let reply = self.send("PASV").await?;
        if reply.code / 100 != 2 {
            return Err(reply.into_error("227 after PASV"));
        }
        let addr = parse_pasv_reply(&reply.raw()).ok_or(FtpError::BadPasv)?;
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| FtpError::Timeout)??;
        // After PROT P the data channel is TLS too, on the control session's connector.
        match self.tls.as_ref() {
            Some(tls) => tls.handshake(stream).await,
            None => {
                let conn: Conn = Box::new(stream);
                Ok(conn)
            }
        }
    }

    /// Read a `LIST` of one path and return the entries we could parse.
    pub async fn list(&mut self, path: &str) -> Result<Vec<FtpEntry>, FtpError> {
        let mut data = self.open_data().await?;
        let reply = self.send(&format!("LIST {path}")).await?;
        if !matches!(reply.code / 100, 1 | 2) {
            return Err(reply.into_error("150 after LIST"));
        }
        let mut raw = Vec::new();
        tokio::time::timeout(
            self.timeout,
            (&mut data).take(LIST_LIMIT as u64).read_to_end(&mut raw),
        )
        .await
        .map_err(|_| FtpError::Timeout)??;
        drop(data);
        let _ = self.reply().await; // the 226 that closes the transfer
        Ok(String::from_utf8_lossy(&raw).lines().filter_map(parse_list_line).collect())
    }

    /// Best-effort `QUIT`. A server that has already gone away is not an error.
    pub async fn quit(mut self) {
        let _ = self.send("QUIT").await;
    }
}

/// One FTP file, fetchable in ranges.
///
/// Each [`fetch_range`](Self::fetch_range) call is a complete session — its
/// own control connection and its own data connection — because FTP has no
/// way to run two transfers over one control connection.
#[derive(Clone, Debug)]
pub struct FtpDownload {
    url: FtpUrl,
}

impl FtpDownload {
    pub fn new(url: FtpUrl) -> Self {
        Self { url }
    }

    pub fn parse(url: &str) -> Result<Self, FtpError> {
        Ok(Self::new(FtpUrl::parse(url)?))
    }

    /// The URL, with credentials removed. Safe to log.
    pub fn redacted_url(&self) -> String {
        self.url.redacted()
    }

    pub fn file_name(&self) -> &str {
        &self.url.file
    }

    /// Establish the size and whether ranged (so multi-connection) download
    /// is possible: `SIZE`, `MDTM`, `LIST` only if `SIZE` failed, then
    /// `REST 0`.
    pub async fn probe(&self) -> Result<FtpProbe, FtpError> {
        let mut control = FtpControl::connect(&self.url).await?;
        let mut size = control.size(&self.url.file).await?;
        let modified = control.modified(&self.url.file).await?;
        if size.is_none() {
            // LIST is a fallback only. The first
            // entry whose name matches, case-insensitively, wins.
            let entries = control.list(&self.url.file).await.unwrap_or_default();
            size = entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(&self.url.file))
                .and_then(|e| e.size);
        }
        let supports_rest = control.supports_rest().await?;
        control.quit().await;
        Ok(FtpProbe { size, supports_rest, modified })
    }

    /// Fetch bytes `[start, end]` (inclusive; `None` means to end of file)
    /// into `file` at offset `start`.
    ///
    /// `progress` is called with the size of each chunk written; returning
    /// `false` abandons the transfer and returns what was written so far, so a
    /// caller can honour a pause or a segment that shrank under it.
    ///
    /// `rate` is awaited between chunks. It belongs here rather than in
    /// `progress` because the callback is synchronous and a speed limit has to
    /// be able to wait.
    pub async fn fetch_range(
        &self,
        start: u64,
        end: Option<u64>,
        file: &mut File,
        rate: Option<&crate::engine::Limiter>,
        mut progress: impl FnMut(u64) -> bool,
    ) -> Result<u64, FtpError> {
        let wanted = match end {
            Some(end) if end < start => return Err(FtpError::EmptyRange),
            Some(end) => Some(end - start + 1),
            None => None,
        };

        let mut control = FtpControl::connect(&self.url).await?;
        let mut data = control.open_data().await?;
        if start > 0 {
            let reply = control.send(&format!("REST {start}")).await?;
            if reply.code != 350 {
                // A server without REST can only be fetched from 0, so the
                // caller has to fall back to a single stream rather than
                // write these bytes at the wrong offset.
                return Err(match reply.code / 100 {
                    4 | 5 => FtpError::NoRest,
                    _ => reply.into_error("350 after REST"),
                });
            }
        }
        let reply = control.send(&format!("RETR {}", self.url.file)).await?;
        if !matches!(reply.code / 100, 1 | 2) {
            return Err(reply.into_error("150 after RETR"));
        }

        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buffer = vec![0u8; READ_CHUNK];
        let mut got = 0u64;
        let mut stopped = false;
        while wanted.is_none_or(|w| got < w) {
            let cap = wanted
                .map(|w| ((w - got).min(READ_CHUNK as u64)) as usize)
                .unwrap_or(READ_CHUNK);
            let n = tokio::time::timeout(self.timeout(), data.read(&mut buffer[..cap]))
                .await
                .map_err(|_| FtpError::Timeout)??;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n]).await?;
            got += n as u64;
            if let Some(rate) = rate {
                rate.account(n as u64).await;
            }
            if !progress(n as u64) {
                stopped = true;
                break;
            }
        }
        file.flush().await?;
        // The segment end lives only on our side: dropping the data socket is
        // what ends the transfer.
        drop(data);

        if let Some(expected) = wanted
            && !stopped
            && got < expected
        {
            return Err(FtpError::ShortRead { got, expected });
        }
        control.quit().await;
        Ok(got)
    }

    fn timeout(&self) -> Duration {
        CONTROL_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn assemble(lines: &[&str]) -> Result<Option<Reply>, FtpError> {
        let mut reader = ReplyReader::default();
        let mut last = None;
        for line in lines {
            last = reader.push(line)?;
        }
        Ok(last)
    }

    #[test]
    fn a_single_line_reply_is_complete_on_its_own() {
        let reply = assemble(&["213 1234\r\n"]).unwrap().unwrap();
        assert_eq!(reply, Reply { code: 213, text: "1234".to_owned() });
    }

    #[test]
    fn a_continuation_block_ends_only_at_the_same_code_with_a_space() {
        // The inner "230 Not the end" must not terminate a 220 block, and the
        // "220-" line must not either.
        let lines = [
            "220-Welcome\r\n",
            "230 Not the end\r\n",
            "220-still going\r\n",
            " indented text\r\n",
            "220 Ready\r\n",
        ];
        let mut reader = ReplyReader::default();
        for line in &lines[..lines.len() - 1] {
            assert_eq!(reader.push(line).unwrap(), None, "{line:?} ended the reply early");
        }
        let reply = reader.push(lines[lines.len() - 1]).unwrap().unwrap();
        assert_eq!(reply.code, 220);
        assert_eq!(reply.text, "Welcome\n230 Not the end\n220-still going\n indented text\nReady");
    }

    #[test]
    fn two_replies_in_a_row_do_not_bleed_into_each_other() {
        let mut reader = ReplyReader::default();
        assert_eq!(reader.push("200 one\r\n").unwrap().unwrap().text, "one");
        assert_eq!(reader.push("200 two\r\n").unwrap().unwrap().text, "two");
    }

    #[test]
    fn a_line_without_a_status_code_is_rejected() {
        assert!(matches!(assemble(&["hello\r\n"]), Err(FtpError::BadReply)));
        assert!(matches!(assemble(&["12 short\r\n"]), Err(FtpError::BadReply)));
        // Four digits is not a code followed by a space or hyphen.
        assert!(matches!(assemble(&["2200 x\r\n"]), Err(FtpError::BadReply)));
    }

    #[test]
    fn a_bare_code_with_no_text_is_still_a_reply() {
        assert_eq!(assemble(&["226"]).unwrap().unwrap(), Reply { code: 226, text: String::new() });
    }

    #[test]
    fn pasv_is_parsed_with_and_without_parentheses() {
        let want = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 2), 5 * 256 + 120);
        assert_eq!(
            parse_pasv_reply("227 Entering Passive Mode (192,168,1,2,5,120)."),
            Some(want)
        );
        // Parentheses are not required at all.
        assert_eq!(parse_pasv_reply("227 Entering Passive Mode 192,168,1,2,5,120"), Some(want));
        assert_eq!(parse_pasv_reply("227 =192,168,1,2,5,120"), Some(want));
    }

    #[test]
    fn pasv_computes_the_port_as_p1_times_256_plus_p2() {
        let addr = parse_pasv_reply("227 (10,0,0,1,255,255)").unwrap();
        assert_eq!(addr.port(), 65535);
        assert_eq!(parse_pasv_reply("227 (10,0,0,1,0,21)").unwrap().port(), 21);
    }

    #[test]
    fn pasv_needs_a_2xx_reply() {
        assert_eq!(parse_pasv_reply("500 (192,168,1,2,5,120)"), None);
        assert_eq!(parse_pasv_reply("425 (192,168,1,2,5,120)"), None);
        assert_eq!(parse_pasv_reply(""), None);
    }

    #[test]
    fn malformed_pasv_replies_are_rejected() {
        // Five numbers is not an address.
        assert_eq!(parse_pasv_reply("227 Entering (192,168,1,2,5)"), None);
        assert_eq!(parse_pasv_reply("227 no numbers here"), None);
        // An octet out of range would be truncated silently; refuse instead.
        assert_eq!(parse_pasv_reply("227 (192,168,1,999,5,120)"), None);
        // A digit that is not followed by a comma must not be read as an
        // octet: this is 10.0.0.1, not 1.0.0.1.
        assert_eq!(
            parse_pasv_reply("227 Mode 1 (10,0,0,1,4,1)"),
            Some(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 1025))
        );
    }

    #[test]
    fn pasv_is_found_in_a_multiline_reply() {
        let reply = assemble(&["227-Entering\r\n", "227 Passive (127,0,0,1,4,0)\r\n"])
            .unwrap()
            .unwrap();
        assert_eq!(
            parse_pasv_reply(&reply.raw()),
            Some(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 1024))
        );
    }

    #[test]
    fn parses_an_eplf_line() {
        assert_eq!(
            parse_list_line("+i8388621.29609,m824255902,r,s10876,\tREADME"),
            Some(FtpEntry { name: "README".to_owned(), size: Some(10876), is_dir: false })
        );
        assert_eq!(
            parse_list_line("+/,m1234,\tpub"),
            Some(FtpEntry { name: "pub".to_owned(), size: None, is_dir: true })
        );
        // No tab, no filename.
        assert_eq!(parse_list_line("+r,s10, README"), None);
    }

    #[test]
    fn parses_a_unix_ls_line() {
        assert_eq!(
            parse_list_line("-rw-r--r--   1 owner group     10876 Jan 15 12:00 README.txt"),
            Some(FtpEntry { name: "README.txt".to_owned(), size: Some(10876), is_dir: false })
        );
        assert_eq!(
            parse_list_line("drwxr-xr-x   2 owner group      4096 Mar  3  2021 pub"),
            Some(FtpEntry { name: "pub".to_owned(), size: Some(4096), is_dir: true })
        );
        // Spaces inside a filename survive.
        assert_eq!(
            parse_list_line("-rw-r--r-- 1 o g 7 Jan 15 12:00 my big file.bin")
                .unwrap()
                .name,
            "my big file.bin"
        );
        // A symlink keeps its own name, not the target.
        assert_eq!(
            parse_list_line("lrwxrwxrwx 1 o g 7 Jan 15 12:00 latest.zip -> v2.zip"),
            Some(FtpEntry { name: "latest.zip".to_owned(), size: Some(7), is_dir: false })
        );
        // The six-character `f...` owner field shifts the size column along.
        assert_eq!(
            parse_list_line("-rw-rw-rw-  1 folder  grp extra  512 Jan 15 12:00 odd.bin"),
            Some(FtpEntry { name: "odd.bin".to_owned(), size: Some(512), is_dir: false })
        );
        // Too few columns: no name to be had.
        assert_eq!(parse_list_line("-rw-r--r-- 1 owner group 10876"), None);
    }

    #[test]
    fn parses_a_vms_line() {
        assert_eq!(
            parse_list_line("README.TXT;1      2  12-APR-2001 10:00:00  [GROUP,USER]"),
            Some(FtpEntry { name: "README.TXT".to_owned(), size: None, is_dir: false })
        );
        assert_eq!(
            parse_list_line("PUB.DIR;1         1  12-APR-2001 10:00:00  [GROUP,USER]"),
            Some(FtpEntry { name: "PUB".to_owned(), size: None, is_dir: true })
        );
    }

    #[test]
    fn a_dos_iis_listing_line_is_not_parsed() {
        // No <DIR> or AM/PM handling, so these must fail and leave the caller
        // relying on SIZE.
        assert_eq!(parse_list_line("03-07-13  11:19AM       <DIR>          pub"), None);
        assert_eq!(parse_list_line("03-07-13  11:19AM              10876 README.txt"), None);
        assert_eq!(parse_list_line(""), None);
    }

    #[test]
    fn parses_a_url_without_credentials() {
        let url = FtpUrl::parse("ftp://ftp.example.com/pub/files/archive.zip").unwrap();
        assert_eq!(url.host, "ftp.example.com");
        assert_eq!(url.port, 21);
        assert_eq!(url.user, "anonymous");
        assert_eq!(url.pass, ANONYMOUS_PASSWORD);
        assert_eq!(url.dir, "/pub/files");
        assert_eq!(url.file, "archive.zip");
    }

    #[test]
    fn parses_a_url_with_credentials_and_a_port() {
        let url = FtpUrl::parse("ftp://bob:s3cret@example.com:2121/x.bin").unwrap();
        assert_eq!((url.host.as_str(), url.port), ("example.com", 2121));
        assert_eq!((url.user.as_str(), url.pass.as_str()), ("bob", "s3cret"));
        assert_eq!(url.dir, "");
        assert_eq!(url.file, "x.bin");

        // A username with no password, and an `@` inside the password.
        let url = FtpUrl::parse("ftp://bob@example.com/x.bin").unwrap();
        assert_eq!((url.user.as_str(), url.pass.as_str()), ("bob", ""));
        let url = FtpUrl::parse("ftp://bob:p@ss@example.com/x.bin").unwrap();
        assert_eq!((url.user.as_str(), url.pass.as_str()), ("bob", "p@ss"));
    }

    #[test]
    fn a_control_character_in_a_credential_or_path_is_rejected() {
        // A CRLF in the password would end the USER/PASS line early and let
        // the rest be read as a command.
        assert!(FtpUrl::parse("ftp://bob:a\r\nDELE x@example.com/x.bin").is_err());
        assert!(FtpUrl::parse("ftp://bob:pass@example.com/x\u{7f}.bin").is_err());
        assert!(FtpUrl::parse("ftp://bob:pass@example.com/a\r\nb/x.bin").is_err());
    }

    #[test]
    fn a_password_containing_a_slash_is_redacted_too() {
        let redacted = redact_url("ftp://bob:pa/ss@h/x");
        assert!(!redacted.contains("bob"), "username leaked in {redacted:?}");
        assert!(!redacted.contains("pa/ss"), "password leaked in {redacted:?}");
        assert_eq!(redacted, "ftp://***@h/x");
        // A URL with no userinfo is returned as it was.
        assert_eq!(redact_url("ftp://h/x"), "ftp://h/x");
    }

    #[test]
    fn rejects_urls_we_cannot_fetch_from() {
        for bad in [
            "http://example.com/x.bin",
            "ftp://example.com/",
            "ftp://example.com",
            "ftp://example.com:notaport/x.bin",
            "ftp:///x.bin",
        ] {
            assert!(FtpUrl::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn ftps_defaults_to_implicit_tls_on_port_990() {
        let url = FtpUrl::parse("ftps://ftp.example.com/pub/x.bin").unwrap();
        assert_eq!((url.tls, url.port), (TlsMode::Implicit, 990));
        assert_eq!((url.host.as_str(), url.dir.as_str(), url.file.as_str()), ("ftp.example.com", "/pub", "x.bin"));
        assert_eq!(url.redacted(), "ftps://ftp.example.com:990/pub/x.bin");
    }

    #[test]
    fn ftps_keeps_an_explicit_port_and_port_21_means_explicit_tls() {
        let url = FtpUrl::parse("ftps://bob:s3cret@example.com:2990/x.bin").unwrap();
        assert_eq!((url.tls, url.port), (TlsMode::Implicit, 2990));
        assert_eq!((url.user.as_str(), url.pass.as_str()), ("bob", "s3cret"));
        let url = FtpUrl::parse("ftps://example.com:21/x.bin").unwrap();
        assert_eq!((url.tls, url.port), (TlsMode::Explicit, 21));
    }

    #[test]
    fn ftp_urls_stay_plaintext() {
        let url = FtpUrl::parse("ftp://example.com/x.bin").unwrap();
        assert_eq!((url.tls, url.port), (TlsMode::Plain, 21));
        assert_eq!(url.redacted(), "ftp://example.com:21/x.bin");
        assert_eq!(FtpUrl::parse("ftp://example.com:990/x.bin").unwrap().tls, TlsMode::Plain);
    }

    #[test]
    fn an_unknown_scheme_is_rejected() {
        for bad in [
            "sftp://example.com/x.bin",
            "gopher://example.com/x.bin",
            "ftpss://example.com/x.bin",
            "ftps:/example.com/x.bin",
        ] {
            assert!(matches!(FtpUrl::parse(bad), Err(FtpError::BadUrl(_))), "{bad} should be rejected");
        }
    }

    fn ftps_url(port: u16, tls: TlsMode) -> FtpUrl {
        FtpUrl {
            host: "127.0.0.1".into(),
            port,
            user: "bob".into(),
            pass: "s3cret".into(),
            dir: String::new(),
            file: "x.bin".into(),
            tls,
        }
    }

    #[tokio::test]
    async fn explicit_ftps_never_falls_back_to_plaintext_when_auth_tls_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ftps_url(listener.local_addr().unwrap().port(), TlsMode::Explicit);
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            reader.get_mut().write_all(b"220 ready\r\n").await.unwrap();
            let mut sent = String::new();
            reader.read_line(&mut sent).await.unwrap(); // AUTH TLS
            reader.get_mut().write_all(b"502 no TLS here\r\n").await.unwrap();
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).await.unwrap();
            sent + &String::from_utf8_lossy(&rest)
        });
        assert!(FtpControl::connect(&url).await.is_err());
        let sent = server.await.unwrap();
        assert!(sent.starts_with("AUTH TLS\r\n"), "{sent:?}");
        assert!(!sent.contains("USER") && !sent.contains("PASS"), "credentials in clear: {sent:?}");
    }

    #[tokio::test]
    async fn explicit_ftps_fails_when_the_server_answers_234_then_speaks_plaintext() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ftps_url(listener.local_addr().unwrap().port(), TlsMode::Explicit);
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            reader.get_mut().write_all(b"220 ready\r\n").await.unwrap();
            let mut sent = String::new();
            reader.read_line(&mut sent).await.unwrap(); // AUTH TLS
            reader.get_mut().write_all(b"234 go ahead\r\n220 plain now\r\n").await.unwrap();
            // Close our side so the client's TLS handshake fails at once instead of waiting.
            reader.get_mut().shutdown().await.unwrap();
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).await.unwrap();
            // The client's TLS ClientHello is binary; a plaintext USER must never appear.
            rest
        });
        // The client refuses the plaintext bytes that follow the 234 (BadReply) before any TLS;
        // a TLS failure is also acceptable. Either way the connection must fail.
        let got = FtpControl::connect(&url).await.err();
        assert!(matches!(got, Some(FtpError::BadReply | FtpError::Tls(_))), "expected a refusal, got {got:?}");
        let rest = server.await.unwrap();
        assert!(!rest.windows(4).any(|w| w == b"USER"), "credentials in clear");
    }

    #[tokio::test]
    async fn implicit_ftps_fails_against_a_plaintext_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ftps_url(listener.local_addr().unwrap().port(), TlsMode::Implicit);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"220 ready\r\n").await.unwrap();
            let mut rest = Vec::new();
            socket.read_to_end(&mut rest).await.unwrap();
            rest
        });
        assert!(matches!(FtpControl::connect(&url).await, Err(FtpError::Tls(_))));
        let rest = server.await.unwrap();
        assert!(!rest.windows(4).any(|w| w == b"USER"), "credentials in clear");
    }

    #[test]
    fn the_tls_connector_builds_with_certificate_checks_on() {
        // The config exposes no getter for its verifier, so this only proves the
        // platform-verifier path builds. Nothing in this module disables it.
        assert!(tls_connector().is_ok());
    }

    #[test]
    fn credentials_never_reach_a_message_or_a_log_line() {
        let url = FtpUrl::parse("ftp://bob:s3cret@example.com/pub/x.bin").unwrap();
        for text in [url.redacted(), format!("{url}"), format!("{url:?}")] {
            assert!(!text.contains("s3cret"), "password leaked in {text:?}");
            assert!(!text.contains("bob"), "username leaked in {text:?}");
            assert!(text.contains("example.com"), "host should still be visible");
        }

        // And from a URL too malformed to parse, which goes into BadUrl.
        let err = FtpUrl::parse("ftp://bob:s3cret@example.com/").unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("s3cret"), "password leaked in {text:?}");
        assert!(!text.contains("bob"), "username leaked in {text:?}");
        assert!(text.contains("example.com"));
    }

    #[test]
    fn error_codes_are_split_into_fatal_and_retryable() {
        let fatal = Reply { code: 550, text: "No such file".to_owned() }.into_error("x");
        assert!(matches!(fatal, FtpError::Permanent { .. }));
        assert!(!fatal.is_retryable());

        let busy = Reply { code: 421, text: "Too many users".to_owned() }.into_error("x");
        assert!(matches!(busy, FtpError::Transient { .. }));
        assert!(busy.is_retryable());

        assert!(FtpError::Timeout.is_retryable());
        assert!(!FtpError::BadPasv.is_retryable());
    }

    // ---- networked tests against a minimal in-process server ----

    /// Enough of an FTP server to answer a probe and a ranged RETR.
    async fn serve(listener: TcpListener, content: Vec<u8>) {
        let (socket, _) = listener.accept().await.expect("accept control");
        let (read_half, mut write) = socket.into_split();
        let mut reader = BufReader::new(read_half);
        // Multi-line greeting on purpose: the client must assemble it.
        write.write_all(b"220-Fake\r\n220 ready\r\n").await.unwrap();

        let mut data_listener: Option<TcpListener> = None;
        let mut rest = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let line = line.trim_end().to_owned();
            let (verb, arg) = match line.split_once(' ') {
                Some((v, a)) => (v.to_uppercase(), a.to_owned()),
                None => (line.to_uppercase(), String::new()),
            };
            let reply = match verb.as_str() {
                "USER" => "331 need password\r\n".to_owned(),
                "PASS" => "230 logged in\r\n".to_owned(),
                "TYPE" | "NOOP" => "200 ok\r\n".to_owned(),
                "CWD" => "250 here\r\n".to_owned(),
                "SIZE" => format!("213 {}\r\n", content.len()),
                "MDTM" => "213 20200102030405\r\n".to_owned(),
                "REST" => {
                    rest = arg.parse().unwrap();
                    "350 positioned\r\n".to_owned()
                }
                "PASV" => {
                    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = l.local_addr().unwrap().port();
                    data_listener = Some(l);
                    // Multi-line on purpose: the client must assemble it.
                    format!(
                        "227-Entering\r\n227 Passive Mode (127,0,0,1,{},{})\r\n",
                        port / 256,
                        port % 256
                    )
                }
                "RETR" => {
                    let listener = data_listener.take().expect("PASV before RETR");
                    write.write_all(b"150 opening\r\n").await.unwrap();
                    let (mut data, _) = listener.accept().await.unwrap();
                    data.write_all(&content[rest..]).await.unwrap();
                    drop(data);
                    rest = 0;
                    "226 done\r\n".to_owned()
                }
                "QUIT" => {
                    write.write_all(b"221 bye\r\n").await.unwrap();
                    return;
                }
                _ => "502 not implemented\r\n".to_owned(),
            };
            write.write_all(reply.as_bytes()).await.unwrap();
        }
    }

    /// Start the server and hand back a `FtpDownload` pointed at it.
    async fn fixture(content: Vec<u8>) -> FtpDownload {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(serve(listener, content));
        FtpDownload::parse(&format!("ftp://bob:s3cret@127.0.0.1:{port}/pub/data.bin")).unwrap()
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[tokio::test]
    async fn a_ranged_fetch_returns_exactly_the_requested_bytes() {
        let content = pattern(1000);
        let download = fixture(content.clone()).await;
        let dir = std::env::temp_dir().join(format!("tdm-ftp-range-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin");

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await
            .unwrap();
        file.set_len(1000).await.unwrap();

        let mut chunks = 0;
        let got = download
            .fetch_range(100, Some(199), &mut file, None, |_| {
                chunks += 1;
                true
            })
            .await
            .unwrap();
        drop(file);

        assert_eq!(got, 100, "wrong byte count");
        assert!(chunks > 0, "progress was never reported");
        let written = tokio::fs::read(&path).await.unwrap();
        assert_eq!(&written[100..200], &content[100..200], "wrong bytes, or wrong offset");
        assert!(written[..100].iter().all(|b| *b == 0), "wrote before the range start");
        assert!(written[200..].iter().all(|b| *b == 0), "wrote past the range end");
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn probe_reports_the_size_and_rest_support() {
        let download = fixture(pattern(4096)).await;
        let probe = download.probe().await.unwrap();
        assert_eq!(probe.size, Some(4096));
        assert!(probe.supports_rest, "REST 0 answered 350, so this must be true");
        assert_eq!(probe.modified.as_deref(), Some("20200102030405"));
    }

    #[tokio::test]
    async fn an_empty_range_is_refused_before_connecting() {
        let download = FtpDownload::parse("ftp://127.0.0.1:1/x.bin").unwrap();
        let mut file = tokio::fs::File::from_std(tempfile_std());
        let err = download.fetch_range(10, Some(5), &mut file, None, |_| true).await.unwrap_err();
        assert!(matches!(err, FtpError::EmptyRange));
    }

    fn tempfile_std() -> std::fs::File {
        let path = std::env::temp_dir().join(format!("tdm-ftp-empty-{}", std::process::id()));
        std::fs::File::create(path).unwrap()
    }
}
