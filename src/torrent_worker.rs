//! Wire protocol between the app and the torrent worker process.
//!
//! One JSON object per line (UTF-8). A line is at most `MAX_LINE` bytes, not
//! counting the line end. Every message carries `"proto": 1`; any other value
//! is rejected.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Protocol version carried by every message.
pub const PROTO: u32 = 1;

/// Longest accepted line, in bytes, without the line end.
pub const MAX_LINE: usize = 1024 * 1024;

/// Commands from the app to the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Add {
        id: String,
        source: Source,
        save_dir: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Remove {
        id: String,
    },
    /// `None` means no cap in that direction.
    SetLimits {
        down_bps: Option<u64>,
        up_bps: Option<u64>,
    },
    Shutdown,
}

/// Where a torrent comes from: `{"magnet": "..."}` or `{"torrent_path": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Magnet(String),
    TorrentPath(String),
}

/// Events from the worker to the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready,
    Progress {
        id: String,
        bytes_done: u64,
        bytes_total: u64,
        peers: u32,
        down_bps: u64,
        up_bps: u64,
    },
    Finished {
        id: String,
    },
    Error {
        id: Option<String>,
        message: String,
    },
}

#[derive(Serialize)]
struct OutFrame<'a, T: Serialize> {
    proto: u32,
    #[serde(flatten)]
    body: &'a T,
}

#[derive(Deserialize)]
struct InFrame<T> {
    proto: u32,
    #[serde(flatten)]
    body: T,
}

/// Why a line was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooLong,
    Proto(u32),
    Json(String),
}

/// Encodes one message as a single line, including the trailing newline.
pub fn encode_line<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(&OutFrame {
        proto: PROTO,
        body: msg,
    })
    .expect("protocol messages always serialize");
    line.push('\n');
    line
}

/// Decodes one line (with or without its line end).
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T, DecodeError> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.len() > MAX_LINE {
        return Err(DecodeError::TooLong);
    }
    let frame: InFrame<T> =
        serde_json::from_str(line).map_err(|e| DecodeError::Json(e.to_string()))?;
    if frame.proto != PROTO {
        return Err(DecodeError::Proto(frame.proto));
    }
    Ok(frame.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assert_round_trip<T>(value: T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let line = encode_line(&value);
        assert!(line.ends_with('\n'));
        assert_eq!(decode_line::<T>(&line), Ok(value));
    }

    #[test]
    fn round_trips_every_command() {
        assert_round_trip(Command::Add {
            id: "a".into(),
            source: Source::Magnet("magnet:?xt=urn:btih:00".into()),
            save_dir: "/tmp/out".into(),
        });
        assert_round_trip(Command::Add {
            id: "b".into(),
            source: Source::TorrentPath("/tmp/x.torrent".into()),
            save_dir: "/tmp/out".into(),
        });
        assert_round_trip(Command::Pause { id: "a".into() });
        assert_round_trip(Command::Resume { id: "a".into() });
        assert_round_trip(Command::Remove { id: "a".into() });
        assert_round_trip(Command::SetLimits {
            down_bps: Some(1000),
            up_bps: None,
        });
        assert_round_trip(Command::Shutdown);
    }

    #[test]
    fn round_trips_every_event() {
        assert_round_trip(Event::Ready);
        assert_round_trip(Event::Progress {
            id: "a".into(),
            bytes_done: 5,
            bytes_total: 10,
            peers: 3,
            down_bps: 100,
            up_bps: 0,
        });
        assert_round_trip(Event::Finished { id: "a".into() });
        assert_round_trip(Event::Error {
            id: None,
            message: "bad".into(),
        });
        assert_round_trip(Event::Error {
            id: Some("a".into()),
            message: "bad".into(),
        });
    }

    #[test]
    fn wire_shape_matches_the_protocol() {
        let line = encode_line(&Command::Add {
            id: "a".into(),
            source: Source::Magnet("magnet:?x".into()),
            save_dir: "/d".into(),
        });
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value,
            json!({"proto": 1, "cmd": "add", "id": "a",
                   "source": {"magnet": "magnet:?x"}, "save_dir": "/d"})
        );

        let line = encode_line(&Event::Error {
            id: None,
            message: "m".into(),
        });
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value, json!({"proto": 1, "event": "error", "id": null, "message": "m"}));
    }

    #[test]
    fn rejects_other_proto() {
        assert_eq!(
            decode_line::<Command>(r#"{"proto":2,"cmd":"shutdown"}"#),
            Err(DecodeError::Proto(2))
        );
        assert_eq!(
            decode_line::<Event>(r#"{"proto":0,"event":"ready"}"#),
            Err(DecodeError::Proto(0))
        );
    }

    #[test]
    fn rejects_missing_proto_and_unknown_command() {
        assert!(matches!(
            decode_line::<Command>(r#"{"cmd":"shutdown"}"#),
            Err(DecodeError::Json(_))
        ));
        assert!(matches!(
            decode_line::<Command>(r#"{"proto":1,"cmd":"explode"}"#),
            Err(DecodeError::Json(_))
        ));
    }

    #[test]
    fn rejects_a_line_over_one_mib() {
        let padding = "a".repeat(MAX_LINE);
        let line = format!(r#"{{"proto":1,"cmd":"pause","id":"{padding}"}}"#);
        assert!(line.len() > MAX_LINE);
        assert_eq!(decode_line::<Command>(&line), Err(DecodeError::TooLong));
    }
}
