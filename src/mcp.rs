//! Model Context Protocol server core, stdio transport. Built only with the
//! `mcp` feature. The server has no network listener: a host process starts
//! `tdm-mcp` and talks to it over stdin and stdout, one JSON object per line.
//!
//! It exposes one tool, `download_url`, backed by the same engine as `tdm-cli`.

use std::path::{Component, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::engine::{Download, Status};
use crate::model::Settings;
use crate::native_host::filename_from_url;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Longest accepted message, in bytes, without the line end.
pub const MAX_MESSAGE: usize = 1024 * 1024;

const MAX_URL: usize = 8192;
const MAX_CONNECTIONS: u32 = 16;

/// The error code for a line that is not JSON.
pub const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// Checked arguments for `download_url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadArgs {
    pub url: String,
    pub dir: PathBuf,
    pub connections: u32,
}

/// Web and FTP schemes only, no spaces or control characters, short enough.
pub fn url_allowed(url: &str) -> bool {
    if url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "ftp://", "ftps://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Checks the tool arguments. The folder must be absolute and must not contain `..`.
pub fn parse_download_args(args: &Value) -> Result<DownloadArgs, String> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .ok_or("url must be a string")?
        .to_owned();
    if !url_allowed(&url) {
        return Err("url must start with http://, https://, ftp:// or ftps:// and contain no spaces".into());
    }
    let dir = PathBuf::from(args.get("dir").and_then(Value::as_str).ok_or("dir must be a string")?);
    if !dir.is_absolute() || dir.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("dir must be an absolute path without '..'".into());
    }
    let connections = match args.get("connections") {
        None => Settings::default().max_segments,
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=u64::from(MAX_CONNECTIONS)).contains(n))
            .ok_or("connections must be a whole number from 1 to 16")? as u32,
    };
    Ok(DownloadArgs { url, dir, connections })
}

/// The tool list as the server reports it.
pub fn tools_list() -> Value {
    json!({
        "tools": [{
            "name": "download_url",
            "description": "Download one http, https, ftp or ftps URL into an absolute folder. Returns the saved path, or the reason it failed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "maxLength": MAX_URL},
                    "dir": {"type": "string", "description": "Absolute folder to save into; created if missing."},
                    "connections": {"type": "integer", "minimum": 1, "maximum": MAX_CONNECTIONS}
                },
                "required": ["url", "dir"],
                "additionalProperties": false
            }
        }]
    })
}

/// Runs one download to the end and returns the saved path.
pub async fn download_to(args: &DownloadArgs) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(&args.dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", args.dir.display()))?;
    let dest = args.dir.join(filename_from_url(&args.url));
    let handle = Download::new(&args.url, dest.clone(), args.connections);
    handle
        .clone()
        .run()
        .await
        .map_err(|e| format!("{}: {e}", args.url))?;
    let snap = handle.snapshot();
    match snap.status {
        Status::Finished => Ok(dest),
        _ => Err(format!(
            "{}: {}",
            args.url,
            snap.error
                .or(snap.paused_reason)
                .unwrap_or_else(|| format!("stopped with status {:?}", snap.status))
        )),
    }
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

async fn call_tool(params: Option<&Value>) -> Value {
    let name = params.and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "download_url" => match parse_download_args(&args) {
            Err(reason) => tool_result(&reason, true),
            Ok(checked) => match download_to(&checked).await {
                Ok(path) => tool_result(&format!("saved {}", path.display()), false),
                Err(reason) => tool_result(&reason, true),
            },
        },
        other => tool_result(&format!("unknown tool: {other}"), true),
    }
}

/// A JSON-RPC error reply. Use `Value::Null` as the id when the request id is unknown.
pub fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handles one parsed message. Returns `None` for notifications and for replies
/// the client sends us, because those get no answer.
pub async fn handle(request: &Value) -> Option<Value> {
    // Requests carry an id; notifications and client replies do not.
    let id = request.get("id")?.clone();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error_reply(id, INVALID_REQUEST, "missing method"));
    };
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tdm-mcp", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => Ok(call_tool(request.get("params")).await),
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err((code, message)) => error_reply(id, code, &message),
    })
}

/// What one read from the client produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Eof,
    Text(String),
    /// Refused without being parsed; the reason is safe to send back.
    Refused(&'static str),
}

/// Reads one line of at most `MAX_MESSAGE` bytes. A longer line is read to its
/// end, never held in memory, and reported as refused.
pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Line> {
    let mut buf: Vec<u8> = Vec::new();
    let mut too_long = false;
    let mut saw_any = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if !saw_any {
                return Ok(Line::Eof);
            }
            break;
        }
        saw_any = true;
        let (take, ends) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if buf.len() + take > MAX_MESSAGE {
                too_long = true;
                buf = Vec::new();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        reader.consume(if ends { take + 1 } else { take });
        if ends {
            break;
        }
    }
    if too_long {
        return Ok(Line::Refused("message too long"));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(match String::from_utf8(buf) {
        Ok(text) => Line::Text(text),
        Err(_) => Line::Refused("message is not UTF-8"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_args() -> Value {
        json!({"url": "https://example.com/a.zip", "dir": std::env::temp_dir().display().to_string()})
    }

    #[test]
    fn accepts_a_web_url_and_an_absolute_folder() {
        let parsed = parse_download_args(&good_args()).unwrap();
        assert_eq!(parsed.url, "https://example.com/a.zip");
        assert!(parsed.dir.is_absolute());
    }

    #[test]
    fn refuses_bad_urls() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "https://a.example/x y", "example.com/x"] {
            let args = json!({"url": url, "dir": std::env::temp_dir().display().to_string()});
            assert!(parse_download_args(&args).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn refuses_relative_and_traversal_folders() {
        let url = "https://example.com/a.zip";
        assert!(parse_download_args(&json!({"url": url, "dir": "downloads"})).is_err());
        let up = std::env::temp_dir().join("..").join("x").display().to_string();
        assert!(parse_download_args(&json!({"url": url, "dir": up})).is_err());
    }

    #[test]
    fn checks_the_connection_count() {
        let mut args = good_args();
        args["connections"] = json!(0);
        assert!(parse_download_args(&args).is_err());
        args["connections"] = json!(17);
        assert!(parse_download_args(&args).is_err());
        args["connections"] = json!(4);
        assert_eq!(parse_download_args(&args).unwrap().connections, 4);
    }

    #[test]
    fn schema_names_the_tool_and_its_required_fields() {
        let list = tools_list();
        let tool = &list["tools"][0];
        assert_eq!(tool["name"], "download_url");
        assert_eq!(tool["inputSchema"]["required"], json!(["url", "dir"]));
        assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
    }

    #[tokio::test]
    async fn initialize_reports_protocol_and_tools_capability() {
        let reply = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .await
            .unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn notifications_get_no_reply() {
        let reply = handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
        assert!(reply.is_none());
    }

    #[tokio::test]
    async fn unknown_method_is_an_error_reply() {
        let reply = handle(&json!({"jsonrpc": "2.0", "id": "a", "method": "nope"})).await.unwrap();
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(reply["id"], "a");
    }

    #[tokio::test]
    async fn bad_tool_arguments_come_back_as_a_tool_error_not_a_download() {
        let reply = handle(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "download_url", "arguments": {"url": "file:///x", "dir": "rel"}}
        }))
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_tool_is_a_tool_error() {
        let reply = handle(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "format_disk", "arguments": {}}
        }))
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
    }

    #[tokio::test]
    async fn reads_lines_and_strips_carriage_returns() {
        let mut input = tokio::io::BufReader::new(&b"first\r\nsecond\n"[..]);
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Text("first".into()));
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Text("second".into()));
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Eof);
    }

    #[tokio::test]
    async fn skips_an_oversized_line_and_reads_the_next_one() {
        let mut bytes = vec![b'a'; MAX_MESSAGE + 10];
        bytes.extend_from_slice(b"\nafter\n");
        let mut input = tokio::io::BufReader::new(&bytes[..]);
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Refused("message too long"));
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Text("after".into()));
    }

    #[tokio::test]
    async fn refuses_invalid_utf8() {
        let mut input = tokio::io::BufReader::new(&b"\xff\xfe\n"[..]);
        assert_eq!(read_line(&mut input).await.unwrap(), Line::Refused("message is not UTF-8"));
    }
}
