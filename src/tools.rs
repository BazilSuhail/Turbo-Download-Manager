//! Helper programs (yt-dlp, ffmpeg and similar) run as supervised child
//! processes. No shell is involved. Stdin is closed, output is capped, and the
//! child is killed when the time limit passes or when the future is dropped.
//! A helper binary must match a SHA-256 pin before it runs.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Most output kept from each of stdout and stderr. Anything beyond is read and dropped.
pub const OUTPUT_CAP: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found at {0}")]
    Missing(String),
    #[error("tool does not match its pinned SHA-256")]
    HashMismatch,
    #[error("could not start the tool: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("tool pipe error: {0}")]
    Io(#[source] std::io::Error),
    #[error("tool timed out after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Checks that `path` is a regular file whose SHA-256 equals `pin_hex`
/// (64 hex digits, any case). Call this before every run of a downloaded helper.
pub fn verify_pin(path: &Path, pin_hex: &str) -> Result<(), ToolError> {
    if !path.is_file() {
        return Err(ToolError::Missing(path.display().to_string()));
    }
    let mut file = std::fs::File::open(path).map_err(ToolError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(ToolError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if hex.eq_ignore_ascii_case(pin_hex.trim()) {
        Ok(())
    } else {
        Err(ToolError::HashMismatch)
    }
}

/// Reads up to `OUTPUT_CAP` bytes and drains the rest. Draining keeps the child from
/// blocking on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(pipe: Option<R>) -> std::io::Result<String> {
    let Some(mut pipe) = pipe else { return Ok(String::new()) };
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = pipe.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if kept.len() < OUTPUT_CAP {
            let room = OUTPUT_CAP - kept.len();
            kept.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
    Ok(String::from_utf8_lossy(&kept).into_owned())
}

/// Runs `program` with `args` and waits for it, up to `limit`.
pub async fn run(program: &Path, args: &[String], limit: Duration) -> Result<Output, ToolError> {
    if !program.is_file() {
        return Err(ToolError::Missing(program.display().to_string()));
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(ToolError::Spawn)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let work = async {
        let (out, err, status) =
            tokio::join!(read_capped(stdout), read_capped(stderr), child.wait());
        let status = status.map_err(ToolError::Io)?;
        Ok::<_, ToolError>(Output {
            code: status.code(),
            stdout: out.map_err(ToolError::Io)?,
            stderr: err.map_err(ToolError::Io)?,
        })
    };
    // The result is bound first so the borrow of `child` ends before the kill below.
    let timed = tokio::time::timeout(limit, work).await;
    match timed {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            Err(ToolError::Timeout(limit))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tdm-tools-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn pin_matches_only_the_right_bytes() {
        let path = temp_file("helper.bin", b"helper-bytes");
        let good = sha256_hex(b"helper-bytes");
        assert!(verify_pin(&path, &good).is_ok());
        assert!(verify_pin(&path, &good.to_uppercase()).is_ok(), "case must not matter");
        assert!(matches!(
            verify_pin(&path, &sha256_hex(b"other")),
            Err(ToolError::HashMismatch)
        ));
    }

    #[test]
    fn pin_refuses_a_directory_and_a_missing_file() {
        let dir = std::env::temp_dir();
        assert!(matches!(verify_pin(&dir, &"0".repeat(64)), Err(ToolError::Missing(_))));
        let gone = dir.join("tdm-tools-definitely-missing.bin");
        assert!(matches!(verify_pin(&gone, &"0".repeat(64)), Err(ToolError::Missing(_))));
    }

    /// A tiny program that prints one word, through the platform shell.
    fn shell() -> (std::path::PathBuf, Vec<String>) {
        if cfg!(windows) {
            (std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"), vec!["/C".into(), "echo tdm-ok".into()])
        } else {
            (std::path::PathBuf::from("/bin/sh"), vec!["-c".into(), "echo tdm-ok".into()])
        }
    }

    #[tokio::test]
    async fn runs_a_helper_and_keeps_its_output() {
        let (program, args) = shell();
        let out = run(&program, &args, Duration::from_secs(30)).await.unwrap();
        assert_eq!(out.code, Some(0));
        assert!(out.stdout.contains("tdm-ok"), "stdout was {:?}", out.stdout);
    }

    #[tokio::test]
    async fn kills_a_helper_that_runs_past_its_limit() {
        let (program, args) = if cfg!(windows) {
            (
                std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                vec!["/C".into(), "ping -n 30 127.0.0.1 >NUL".into()],
            )
        } else {
            (std::path::PathBuf::from("/bin/sh"), vec!["-c".into(), "sleep 30".into()])
        };
        let started = std::time::Instant::now();
        let result = run(&program, &args, Duration::from_millis(400)).await;
        assert!(matches!(result, Err(ToolError::Timeout(_))), "got {result:?}");
        assert!(started.elapsed() < Duration::from_secs(20), "the child was not stopped");
    }

    #[tokio::test]
    async fn refuses_a_missing_program_without_starting_anything() {
        let result = run(Path::new("no-such-helper-tdm"), &[], Duration::from_secs(1)).await;
        assert!(matches!(result, Err(ToolError::Missing(_))));
    }

    #[tokio::test]
    async fn caps_the_output_it_keeps() {
        let bytes = vec![b'x'; OUTPUT_CAP * 3];
        let kept = read_capped(Some(&bytes[..])).await.unwrap();
        assert_eq!(kept.len(), OUTPUT_CAP);
    }
}
