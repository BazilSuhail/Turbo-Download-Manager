//! Media helper jobs (ffmpeg and yt-dlp) run through `tools`. Argument vectors
//! are built here, so no user text reaches a shell. Every run checks the
//! helper's SHA-256 pin before it starts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::DownloadSettings;
use crate::tools::{self, ToolError};

const FFMPEG_LIMIT: Duration = Duration::from_secs(30 * 60);
const YT_DLP_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Mp3 { input: PathBuf, output: PathBuf },
    Mp4Copy { input: PathBuf, output: PathBuf },
    YtDlp { url: String, dir: PathBuf },
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no path is set for this helper")]
    NotConfigured,
    #[error("required file or folder is missing: {0}")]
    PathMissing(String),
    #[error("helper does not match its pinned SHA-256")]
    HashMismatch,
    #[error("URL must start with http:// or https://")]
    BadUrl,
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("refused: {0}")]
    Refused(String),
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// ffmpeg arguments for a job. yt-dlp jobs have none and return `None`.
pub fn ffmpeg_args(job: &Job) -> Option<Vec<String>> {
    match job {
        Job::Mp3 { input, output } => Some(vec![
            "-i".into(),
            text(input),
            "-vn".into(),
            "-acodec".into(),
            "libmp3lame".into(),
            "-q:a".into(),
            "2".into(),
            "-y".into(),
            text(output),
        ]),
        Job::Mp4Copy { input, output } => Some(vec![
            "-i".into(),
            text(input),
            "-c".into(),
            "copy".into(),
            "-y".into(),
            text(output),
        ]),
        Job::YtDlp { .. } => None,
    }
}

/// yt-dlp arguments. `--no-exec` and `--no-config` are required: they stop the
/// tool from running commands or reading a config file that could change its behaviour.
pub fn yt_dlp_args(url: &str, dir: &Path) -> Vec<String> {
    // The folder is part of the output template, where `%` starts a field.
    // A literal `%` in the folder name is doubled so it is not read as a field.
    let folder = text(dir).replace('%', "%%");
    vec![
        "--no-playlist".into(),
        "--no-exec".into(),
        "--no-config".into(),
        "-o".into(),
        format!("{folder}/%(title)s.%(ext)s"),
        url.to_string(),
    ]
}

/// Accepts only http and https URLs. A URL with a leading dash can never pass
/// this check, because it has no scheme, so it cannot be read as an option.
fn check_url(url: &str) -> Result<(), MediaError> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(())
    } else {
        Err(MediaError::BadUrl)
    }
}

fn refuse_dash(p: &Path) -> Result<(), MediaError> {
    if text(p).starts_with('-') {
        Err(MediaError::Refused(format!("path starts with '-': {}", text(p))))
    } else {
        Ok(())
    }
}

/// Checks the job's inputs and builds its argument vector and time limit.
fn plan(job: &Job) -> Result<(Vec<String>, Duration), MediaError> {
    match job {
        Job::YtDlp { url, dir } => {
            check_url(url)?;
            refuse_dash(dir)?;
            if !dir.is_dir() {
                return Err(MediaError::PathMissing(text(dir)));
            }
            Ok((yt_dlp_args(url, dir), YT_DLP_LIMIT))
        }
        Job::Mp3 { input, output } | Job::Mp4Copy { input, output } => {
            refuse_dash(input)?;
            refuse_dash(output)?;
            if !input.is_file() {
                return Err(MediaError::PathMissing(text(input)));
            }
            let args = ffmpeg_args(job)
                .ok_or_else(|| MediaError::Refused("no ffmpeg arguments for this job".into()))?;
            Ok((args, FFMPEG_LIMIT))
        }
    }
}

/// Runs one helper job after its pin check. Stops early when the helper path,
/// the pinned hash or the job's inputs are missing or unsafe.
pub async fn run_job(settings: &DownloadSettings, job: Job) -> Result<tools::Output, MediaError> {
    let (helper, pin) = match &job {
        Job::YtDlp { .. } => (settings.yt_dlp_path.as_deref(), settings.yt_dlp_sha256.as_str()),
        _ => (settings.ffmpeg_path.as_deref(), settings.ffmpeg_sha256.as_str()),
    };
    let helper = helper.ok_or(MediaError::NotConfigured)?;
    if pin.trim().is_empty() {
        return Err(MediaError::Refused("no pinned hash".into()));
    }
    let (args, limit) = plan(&job)?;
    tools::verify_pin(helper, pin).map_err(|e| match e {
        ToolError::HashMismatch => MediaError::HashMismatch,
        other => MediaError::Tool(other),
    })?;
    Ok(tools::run(helper, &args, limit).await?)
}

/// One-line description of a job for the UI log.
pub fn job_summary(job: &Job) -> String {
    match job {
        Job::Mp3 { input, output } => format!("Convert {} to MP3 at {}", text(input), text(output)),
        Job::Mp4Copy { input, output } => format!("Copy streams of {} into {}", text(input), text(output)),
        Job::YtDlp { url, dir } => format!("Download {url} with yt-dlp into {}", text(dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mp3_args_are_exact() {
        let job = Job::Mp3 { input: "in.mp4".into(), output: "out.mp3".into() };
        assert_eq!(
            ffmpeg_args(&job).unwrap(),
            v(&["-i", "in.mp4", "-vn", "-acodec", "libmp3lame", "-q:a", "2", "-y", "out.mp3"])
        );
    }

    #[test]
    fn mp4_copy_args_are_exact() {
        let job = Job::Mp4Copy { input: "in.mkv".into(), output: "out.mp4".into() };
        assert_eq!(ffmpeg_args(&job).unwrap(), v(&["-i", "in.mkv", "-c", "copy", "-y", "out.mp4"]));
    }

    #[test]
    fn yt_dlp_has_no_ffmpeg_args() {
        let job = Job::YtDlp { url: "https://example.com/v".into(), dir: "C:/dl".into() };
        assert!(ffmpeg_args(&job).is_none());
    }

    #[test]
    fn yt_dlp_args_are_exact() {
        assert_eq!(
            yt_dlp_args("https://example.com/v", Path::new("C:/dl")),
            v(&[
                "--no-playlist",
                "--no-exec",
                "--no-config",
                "-o",
                "C:/dl/%(title)s.%(ext)s",
                "https://example.com/v",
            ])
        );
    }

    #[test]
    fn yt_dlp_template_escapes_percent_in_the_folder() {
        assert_eq!(
            yt_dlp_args("https://example.com/v", Path::new("C:/100%/dl"))[4],
            "C:/100%%/dl/%(title)s.%(ext)s"
        );
    }

    #[test]
    fn non_http_urls_and_dash_urls_are_refused() {
        assert!(matches!(check_url("--exec=calc"), Err(MediaError::BadUrl)));
        assert!(matches!(check_url("-o x"), Err(MediaError::BadUrl)));
        assert!(matches!(check_url("ftp://example.com/a"), Err(MediaError::BadUrl)));
        assert!(matches!(check_url("file:///C:/secret"), Err(MediaError::BadUrl)));
        assert!(check_url("https://example.com/v").is_ok());
        assert!(check_url("HTTP://example.com/v").is_ok());
    }

    #[test]
    fn dash_paths_are_refused_before_running() {
        let job = Job::Mp3 { input: "-evil.mp4".into(), output: "out.mp3".into() };
        assert!(matches!(plan(&job), Err(MediaError::Refused(_))));
        let job = Job::YtDlp { url: "--exec x".into(), dir: "C:/dl".into() };
        assert!(matches!(plan(&job), Err(MediaError::BadUrl)));
    }

    #[tokio::test]
    async fn no_path_means_not_configured() {
        let settings = DownloadSettings::default();
        let yt = Job::YtDlp { url: "https://example.com/v".into(), dir: "C:/dl".into() };
        assert!(matches!(run_job(&settings, yt).await, Err(MediaError::NotConfigured)));
        let mp3 = Job::Mp3 { input: "in.mp4".into(), output: "out.mp3".into() };
        assert!(matches!(run_job(&settings, mp3).await, Err(MediaError::NotConfigured)));
    }

    #[tokio::test]
    async fn empty_pin_is_refused() {
        let mut settings = DownloadSettings::default();
        settings.ffmpeg_path = Some(PathBuf::from("ffmpeg.exe"));
        settings.ffmpeg_sha256 = String::new();
        let job = Job::Mp3 { input: "in.mp4".into(), output: "out.mp3".into() };
        match run_job(&settings, job).await {
            Err(MediaError::Refused(msg)) => assert_eq!(msg, "no pinned hash"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn summary_names_the_job() {
        let job = Job::Mp3 { input: "a.mp4".into(), output: "a.mp3".into() };
        assert!(job_summary(&job).contains("MP3"));
    }
}
