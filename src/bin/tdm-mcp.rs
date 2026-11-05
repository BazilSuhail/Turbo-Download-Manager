//! `tdm-mcp`: Model Context Protocol server over stdio. Built with `--features mcp`.
//! Reads one JSON message per line from stdin and writes replies to stdout.
//! Logs go to stderr only, because stdout belongs to the protocol.

use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};

use tdm::mcp::{self, Line};

#[tokio::main]
async fn main() {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    loop {
        let reply = match mcp::read_line(&mut stdin).await {
            Ok(Line::Eof) => break,
            Ok(Line::Refused(reason)) => Some(mcp::error_reply(Value::Null, -32600, reason)),
            Ok(Line::Text(text)) if text.trim().is_empty() => None,
            Ok(Line::Text(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(request) => mcp::handle(&request).await,
                Err(_) => Some(mcp::error_reply(Value::Null, mcp::PARSE_ERROR, "parse error")),
            },
            Err(e) => {
                eprintln!("tdm-mcp: stdin failed: {e}");
                break;
            }
        };
        if let Some(reply) = reply {
            let mut line = reply.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    }
}
