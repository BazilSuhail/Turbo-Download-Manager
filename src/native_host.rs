//! Browser native-messaging host — our own protocol.
//!
//! Browser-standard framing (4-byte little-endian length, then UTF-8 JSON)
//! carrying plain JSON *objects* with named fields. The extension on the other
//! end is our own, so the schema is ours to define and keep readable.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Chrome caps a message from an extension at 1 MiB. Anything larger is a bug
/// or a hostile peer, so we refuse it rather than allocating.
pub const MAX_MESSAGE_BYTES: u32 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("message of {0} bytes exceeds the {MAX_MESSAGE_BYTES} byte limit")]
    TooLarge(u32),
    #[error("malformed message: {0}")]
    Json(#[from] serde_json::Error),
}

/// A message from the browser extension to us.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Request {
    /// Sent once when the extension connects.
    Hello { version: String },
    /// Take over a download.
    #[serde(rename_all = "camelCase")]
    Download {
        /// Echoed back on the reply so the extension can match them up.
        /// Without it, replies are matched by arrival order, which only holds
        /// while the host answers strictly in sequence — one timeout would
        /// desynchronise the stream permanently.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        referer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cookie: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_agent: Option<String>,
        /// Filename the browser suggested, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    /// Ask whether we would take this URL, without starting anything.
    #[serde(rename_all = "camelCase")]
    Query {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
        url: String,
    },
    /// Raise the running window. Sent by a second GUI launch that is handing
    /// off to the first instead of starting a duplicate.
    Show,
}

impl Request {
    /// The correlation id, if the sender supplied one.
    pub fn request_id(&self) -> Option<u64> {
        match self {
            Request::Download { request_id, .. } | Request::Query { request_id, .. } => *request_id,
            Request::Hello { .. } | Request::Show => None,
        }
    }
}

/// Our reply to the extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    /// Answer to `Hello`, carrying the capture rules the extension should apply.
    #[serde(rename_all = "camelCase")]
    Ready {
        version: String,
        /// Extensions we want handed to us, lowercase and without a dot.
        capture_extensions: Vec<String>,
        /// Hosts to leave alone.
        exception_hosts: Vec<String>,
        /// Whether capture is switched on at all.
        #[serde(default = "default_true")]
        enabled: bool,
        /// Ignore downloads smaller than this, when the browser tells us a size.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_min_size: Option<u64>,
    },
    /// We accepted the download.
    #[serde(rename_all = "camelCase")]
    Accepted {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
    },
    /// We are not taking this one; the browser should handle it.
    #[serde(rename_all = "camelCase")]
    Declined {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
    },
}

fn default_true() -> bool {
    true
}

/// Reads and writes length-prefixed JSON over any byte streams. Generic over
/// the streams so it can be exercised against in-memory buffers in tests
/// rather than only against real pipes.
pub struct Codec<R, W> {
    input: R,
    output: W,
}

impl<R: Read, W: Write> Codec<R, W> {
    pub fn new(input: R, output: W) -> Self {
        Self { input, output }
    }

    /// Read one message. `Ok(None)` means the peer closed the pipe cleanly,
    /// which is the normal way a native-messaging host is shut down.
    pub fn read<T: for<'de> Deserialize<'de>>(&mut self) -> Result<Option<T>, Error> {
        let mut len = [0u8; 4];
        match self.input.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(len);
        if len > MAX_MESSAGE_BYTES {
            return Err(Error::TooLarge(len));
        }
        let mut body = vec![0u8; len as usize];
        self.input.read_exact(&mut body)?;
        Ok(Some(serde_json::from_slice(&body)?))
    }

    pub fn write<T: Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let body = serde_json::to_vec(value)?;
        let len = u32::try_from(body.len()).map_err(|_| Error::TooLarge(u32::MAX))?;
        if len > MAX_MESSAGE_BYTES {
            return Err(Error::TooLarge(len));
        }
        self.output.write_all(&len.to_le_bytes())?;
        self.output.write_all(&body)?;
        self.output.flush()?;
        Ok(())
    }

    /// Serve requests until the peer disconnects. `handle` returns the reply.
    pub fn serve(&mut self, mut handle: impl FnMut(Request) -> Response) -> Result<(), Error> {
        while let Some(request) = self.read::<Request>()? {
            let response = handle(request);
            self.write(&response)?;
        }
        Ok(())
    }
}

/// Derive a filename from a URL when the browser did not suggest one.
/// Strips the query and fragment, and rejects path traversal.
pub fn filename_from_url(url: &str) -> String {
    let stem = url.split(['?', '#']).next().unwrap_or(url);
    let last = stem.rsplit('/').find(|s| !s.is_empty()).unwrap_or("download");
    sanitize_filename(last)
}

/// Make a browser-supplied name safe to join onto a directory path.
///
/// A suggested filename arrives from a remote server, so it is untrusted:
/// strip directory separators and traversal, drop characters Windows forbids,
/// and never return an empty or reserved name.
pub fn sanitize_filename(name: &str) -> String {
    const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    // Bidirectional overrides are invisible but reorder what the user reads,
    // so `invoice\u{202E}fdp.exe` displays as `invoice` + `exe.pdf`. A name
    // arriving from a remote server has no legitimate need for them.
    const BIDI: &[char] = &[
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200E}', '\u{200F}',
        // Zero-width characters hide a second extension just as effectively.
        '\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}',
    ];
    let cleaned: String = name
        .chars()
        .filter(|c| !FORBIDDEN.contains(c) && !BIDI.contains(c) && !c.is_control())
        .collect();

    // Windows silently drops trailing dots and spaces, so `evil.exe. . .`
    // becomes `evil.exe`. A single `trim_matches('.')` leaves `a. .` as `a. `,
    // so strip both characters together until nothing changes.
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        return "download".to_owned();
    }

    // Reserved device names are still reserved with an extension, and the
    // console streams take a trailing `$`.
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return format!("_{cleaned}");
    }
    cleaned.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(json: &str) -> Vec<u8> {
        let mut v = (json.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(json.as_bytes());
        v
    }

    #[test]
    fn reads_a_length_prefixed_download_request() {
        let json = r#"{"type":"download","url":"https://e.test/a.zip","referer":"https://e.test/"}"#;
        let mut codec = Codec::new(Cursor::new(framed(json)), Vec::new());
        let got: Request = codec.read().unwrap().unwrap();
        assert_eq!(
            got,
            Request::Download {
                request_id: None,
                url: "https://e.test/a.zip".into(),
                referer: Some("https://e.test/".into()),
                cookie: None,
                user_agent: None,
                filename: None,
                size: None,
            }
        );
    }

    #[test]
    fn a_closed_pipe_reads_as_end_of_stream_not_an_error() {
        let mut codec = Codec::new(Cursor::new(Vec::new()), Vec::new());
        assert!(codec.read::<Request>().unwrap().is_none());
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_without_allocating() {
        let mut bytes = (MAX_MESSAGE_BYTES + 1).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let mut codec = Codec::new(Cursor::new(bytes), Vec::new());
        assert!(matches!(codec.read::<Request>(), Err(Error::TooLarge(_))));
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_silent_short_read() {
        let mut bytes = 100u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{\"type\":\"query\"");
        let mut codec = Codec::new(Cursor::new(bytes), Vec::new());
        assert!(codec.read::<Request>().is_err());
    }

    #[test]
    fn written_messages_round_trip_through_the_codec() {
        let mut out = Vec::new();
        {
            let mut codec = Codec::new(Cursor::new(Vec::new()), &mut out);
            codec.write(&Response::Accepted { id: 42, request_id: None }).unwrap();
        }
        let mut codec = Codec::new(Cursor::new(out), Vec::new());
        let back: Response = codec.read().unwrap().unwrap();
        assert_eq!(back, Response::Accepted { id: 42, request_id: None });
    }

    #[test]
    fn serve_answers_every_request_until_the_pipe_closes() {
        let mut input = framed(r#"{"type":"hello","version":"1.0"}"#);
        input.extend(framed(r#"{"type":"query","url":"https://e.test/a.zip"}"#));
        let mut out = Vec::new();
        {
            let mut codec = Codec::new(Cursor::new(input), &mut out);
            codec
                .serve(|req| match req {
                    Request::Hello { .. } => Response::Ready {
                        version: "1.0".into(),
                        capture_extensions: vec!["zip".into()],
                        exception_hosts: vec![],
                    enabled: true,
                    capture_min_size: None,
                    },
                    _ => Response::Declined { reason: "test".into(), request_id: None },
                })
                .unwrap();
        }
        let mut codec = Codec::new(Cursor::new(out), Vec::new());
        assert!(matches!(codec.read::<Response>().unwrap(), Some(Response::Ready { .. })));
        assert!(matches!(codec.read::<Response>().unwrap(), Some(Response::Declined { .. })));
        assert!(codec.read::<Response>().unwrap().is_none());
    }

    #[test]
    fn filenames_come_from_the_url_path_without_query_or_fragment() {
        assert_eq!(filename_from_url("https://e.test/dir/a.zip?x=1#f"), "a.zip");
        assert_eq!(filename_from_url("https://e.test/dir/"), "dir");
        assert_eq!(filename_from_url("https://e.test"), "e.test");
    }

    #[test]
    fn suggested_names_cannot_escape_the_download_directory() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_filename("..\\..\\win.ini"), "win.ini");
        assert_eq!(sanitize_filename("a/b/c.txt"), "abc.txt");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename("   "), "download");
    }

    #[test]
    fn windows_device_names_are_defused() {
        assert_eq!(sanitize_filename("NUL"), "_NUL");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
    }

    #[test]
    fn control_characters_are_stripped_from_suggested_names() {
        assert_eq!(sanitize_filename("a\u{0}b\u{1}.zip"), "ab.zip");
        assert_eq!(sanitize_filename("re\nport.pdf"), "report.pdf");
    }

    /// A right-to-left override makes `invoice<RLO>fdp.exe` render as
    /// `invoiceexe.pdf`, which is how an executable gets opened as a document.
    #[test]
    fn bidi_overrides_cannot_disguise_an_extension() {
        let spoofed = "invoice\u{202E}fdp.exe";
        let safe = sanitize_filename(spoofed);
        assert_eq!(safe, "invoicefdp.exe");
        assert!(!safe.contains('\u{202E}'));
        for c in ['\u{202A}', '\u{2066}', '\u{200E}', '\u{200B}', '\u{FEFF}'] {
            assert!(!sanitize_filename(&format!("a{c}b.zip")).contains(c), "{c:?} survived");
        }
    }

    /// Windows discards trailing dots and spaces, so they must not be a way
    /// to smuggle a different final extension past the check.
    #[test]
    fn trailing_dots_and_spaces_are_fully_stripped() {
        assert_eq!(sanitize_filename("evil.exe. . ."), "evil.exe");
        assert_eq!(sanitize_filename("report.pdf   "), "report.pdf");
        assert_eq!(sanitize_filename("a. ."), "a");
        assert_eq!(sanitize_filename(". . ."), "download");
    }

    #[test]
    fn console_device_names_are_defused_too() {
        assert_eq!(sanitize_filename("CONIN$"), "_CONIN$");
        assert_eq!(sanitize_filename("conout$.txt"), "_conout$.txt");
        assert_eq!(sanitize_filename("CLOCK$"), "_CLOCK$");
    }
}
