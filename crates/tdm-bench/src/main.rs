//! Black-box ranged-download benchmark against an in-process tdm-labserver.
//!
//! Each run starts a fresh lab server, downloads the file with N ranged
//! connections, resumes segments after drops, retries 5xx responses with a
//! limit, and checks the SHA-256 of the result against the expected bytes.
//! Output is one JSON object per run, then one summary object.

use std::error::Error;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use clap::Parser;
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, StatusCode};
use serde_json::json;
use sha2::{Digest, Sha256};
use tdm_labserver::{LabConfig, Scenario, expected_bytes, parse_size, start};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinHandle;

/// Consecutive failed attempts (without progress) before a segment gives up.
const MAX_CONSECUTIVE_FAILURES: u32 = 50;
/// Failed probe attempts before the run gives up.
const MAX_PROBE_ATTEMPTS: u32 = 50;
const HASH_CHUNK: u64 = 4 << 20;
const BENCH_PATH: &str = "/bench.bin";

#[derive(Parser, Debug)]
#[command(name = "tdm-bench", about = "Ranged-download benchmark against tdm-labserver")]
struct Cli {
    /// Lab fault scenario: fast, latency:<ms>, drops:<bytes>, capped:<bps>, refuse:<n>,
    /// norange, changed, slowread, fail_every:<n>.
    #[arg(long, default_value = "fast")]
    scenario: String,
    /// File size in bytes, or with a KiB, MiB or GiB suffix.
    #[arg(long, default_value = "16MiB")]
    size: String,
    /// Number of parallel ranged connections.
    #[arg(long, default_value_t = 8)]
    connections: usize,
    /// Number of runs (each starts a fresh lab server).
    #[arg(long, default_value_t = 7)]
    runs: usize,
}

#[derive(Default)]
struct Measured {
    elapsed_ms: f64,
    retries: u64,
    accepted: u64,
    sha256: String,
    expected_sha256: String,
    verified: bool,
    error: Option<String>,
}

struct Outcome {
    retries: u64,
    error: Option<String>,
}

/// Result of one attempt at a byte range.
enum Step {
    Done,
    /// Transient failure: retry from the reported position.
    Retry(String),
    /// Permanent failure: do not retry.
    Fatal(String),
}

async fn probe(client: &Client, url: &str) -> Result<(u64, String, u64), String> {
    let mut retries = 0u64;
    let mut last = String::new();
    for attempt in 1..=MAX_PROBE_ATTEMPTS {
        match client.head(url).send().await {
            Ok(resp) if resp.status() == StatusCode::OK => {
                let size = resp
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("probe: missing Content-Length")?;
                let etag = resp
                    .headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("probe: missing ETag")?
                    .to_string();
                return Ok((size, etag, retries));
            }
            Ok(resp) if resp.status().is_server_error() => last = format!("probe HTTP {}", resp.status()),
            Ok(resp) => return Err(format!("probe: unexpected HTTP {}", resp.status())),
            Err(e) => last = format!("probe: {e}"),
        }
        retries += 1;
        tokio::time::sleep(backoff(attempt)).await;
    }
    Err(format!("probe failed after {MAX_PROBE_ATTEMPTS} attempts: {last}"))
}

fn backoff(consecutive: u32) -> Duration {
    Duration::from_millis((20 * consecutive as u64).min(300))
}

/// Fetch bytes `[pos, end)` of the file. Returns the position reached, which
/// advances even when the attempt fails part-way through.
async fn fetch_range(
    client: &Client,
    url: &str,
    etag: &str,
    file: &mut tokio::fs::File,
    mut pos: u64,
    end: u64,
) -> (u64, Step) {
    let range = format!("bytes={pos}-{}", end - 1);
    let resp = match client.get(url).header(header::RANGE, range).send().await {
        Ok(r) => r,
        Err(e) => return (pos, Step::Retry(e.to_string())),
    };
    let status = resp.status();
    if status.is_server_error() {
        return (pos, Step::Retry(format!("HTTP {status}")));
    }
    if status != StatusCode::PARTIAL_CONTENT && status != StatusCode::OK {
        return (pos, Step::Fatal(format!("unexpected HTTP {status}")));
    }
    let got_etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if got_etag != etag {
        return (pos, Step::Fatal(format!("content changed: ETag {etag} -> {got_etag}")));
    }
    // Body byte index that corresponds to `cursor`: 206 starts at `pos`,
    // a 200 (range ignored) starts at 0 and is skipped forward.
    let mut cursor = if status == StatusCode::PARTIAL_CONTENT {
        let want = format!("bytes {pos}-");
        let cr = resp
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v: &HeaderValue| v.to_str().ok())
            .unwrap_or("");
        if !cr.starts_with(&want) {
            return (pos, Step::Fatal(format!("bad Content-Range '{cr}', wanted start {pos}")));
        }
        pos
    } else {
        0
    };

    let mut resp = resp;
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return (pos, Step::Retry(e.to_string())),
        };
        let chunk_end = cursor + chunk.len() as u64;
        let lo = pos.max(cursor);
        let hi = end.min(chunk_end);
        if lo < hi {
            let slice = &chunk[(lo - cursor) as usize..(hi - cursor) as usize];
            let io = async {
                file.seek(std::io::SeekFrom::Start(lo)).await?;
                file.write_all(slice).await
            };
            if let Err(e) = io.await {
                return (pos, Step::Fatal(format!("write failed: {e}")));
            }
            pos = hi;
        }
        cursor = chunk_end;
        if pos >= end {
            return (pos, Step::Done);
        }
    }
    if pos >= end {
        (pos, Step::Done)
    } else {
        (pos, Step::Retry(format!("body ended at {pos} of {end}")))
    }
}

/// Download one segment, resuming after drops and retrying transient errors.
async fn download_segment(
    client: Client,
    url: String,
    etag: String,
    path: PathBuf,
    seg_start: u64,
    seg_end: u64,
) -> Result<u64, String> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut pos = seg_start;
    let mut retries = 0u64;
    let mut consecutive = 0u32;
    loop {
        let (reached, step) = fetch_range(&client, &url, &etag, &mut file, pos, seg_end).await;
        if reached > pos {
            consecutive = 0;
        }
        pos = reached;
        match step {
            Step::Done => return Ok(retries),
            Step::Fatal(msg) => return Err(msg),
            Step::Retry(msg) => {
                retries += 1;
                consecutive += 1;
                if consecutive > MAX_CONSECUTIVE_FAILURES {
                    return Err(format!("gave up at {pos} after {consecutive} failures: {msg}"));
                }
                tokio::time::sleep(backoff(consecutive)).await;
            }
        }
    }
}

async fn download(client: &Client, url: &str, size: u64, connections: usize, path: &Path) -> Outcome {
    let (probe_size, etag, probe_retries) = match probe(client, url).await {
        Ok(p) => p,
        Err(e) => return Outcome { retries: 0, error: Some(e) },
    };
    if probe_size != size {
        return Outcome {
            retries: probe_retries,
            error: Some(format!("server size {probe_size} != requested {size}")),
        };
    }
    if let Err(e) = std::fs::File::create(path).and_then(|f| f.set_len(size)) {
        return Outcome { retries: probe_retries, error: Some(format!("create temp file: {e}")) };
    }

    let n = (connections as u64).min(size).max(1);
    let mut handles: Vec<JoinHandle<Result<u64, String>>> = Vec::new();
    for i in 0..n {
        let seg_start = i * size / n;
        let seg_end = (i + 1) * size / n;
        if seg_start == seg_end {
            continue;
        }
        handles.push(tokio::spawn(download_segment(
            client.clone(),
            url.to_string(),
            etag.clone(),
            path.to_path_buf(),
            seg_start,
            seg_end,
        )));
    }

    let mut retries = probe_retries;
    let mut errors = Vec::new();
    for h in handles {
        match h.await {
            Ok(Ok(r)) => retries += r,
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(format!("segment task: {e}")),
        }
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    Outcome { retries, error }
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK as usize];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn expected_hash(size: u64) -> String {
    let mut h = Sha256::new();
    let mut off = 0u64;
    while off < size {
        let len = HASH_CHUNK.min(size - off);
        h.update(expected_bytes(size, off, len));
        off += len;
    }
    hex::encode(h.finalize())
}

async fn run_once(run: usize, scenario: Scenario, size: u64, connections: usize) -> Measured {
    let mut m = Measured::default();
    let lab = match start(LabConfig {
        size_bytes: size,
        scenario,
        port: 0,
        path: BENCH_PATH.to_string(),
    })
    .await
    {
        Ok(lab) => lab,
        Err(e) => {
            m.error = Some(format!("start lab: {e}"));
            return m;
        }
    };

    let path = std::env::temp_dir().join(format!("tdm-bench-{}-{run}.bin", std::process::id()));
    let client = Client::new();
    let t0 = Instant::now();
    let outcome = download(&client, &lab.url, size, connections, &path).await;
    m.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    m.retries = outcome.retries;
    m.accepted = lab.accepted.load(Ordering::SeqCst);
    drop(client);

    m.expected_sha256 = tokio::task::spawn_blocking(move || expected_hash(size))
        .await
        .unwrap_or_default();
    match outcome.error {
        Some(e) => m.error = Some(e),
        None => {
            let p = path.clone();
            match tokio::task::spawn_blocking(move || hash_file(&p)).await {
                Ok(Ok(sha)) => {
                    m.verified = sha == m.expected_sha256;
                    if !m.verified {
                        m.error = Some("sha256 mismatch".to_string());
                    }
                    m.sha256 = sha;
                }
                Ok(Err(e)) => m.error = Some(format!("hash temp file: {e}")),
                Err(e) => m.error = Some(format!("hash task: {e}")),
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    if let Err(e) = lab.shutdown().await {
        m.error.get_or_insert(format!("lab shutdown: {e}"));
    }
    m
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode, Box<dyn Error>> {
    let cli = Cli::parse();
    let scenario: Scenario = cli.scenario.parse()?;
    let size = parse_size(&cli.size)?;
    if size == 0 {
        return Err("--size must be greater than zero".into());
    }
    if cli.connections == 0 {
        return Err("--connections must be at least 1".into());
    }
    if cli.runs == 0 {
        return Err("--runs must be at least 1".into());
    }

    let mut elapsed_ok = Vec::new();
    let mut failed = 0usize;
    for run in 1..=cli.runs {
        let m = run_once(run, scenario, size, cli.connections).await;
        let ok = m.verified && m.error.is_none();
        if ok {
            elapsed_ok.push(m.elapsed_ms);
        } else {
            failed += 1;
        }
        let throughput = if m.elapsed_ms > 0.0 {
            size as f64 / (1024.0 * 1024.0) / (m.elapsed_ms / 1000.0)
        } else {
            0.0
        };
        let line = json!({
            "type": "run",
            "run": run,
            "scenario": scenario.to_string(),
            "size_bytes": size,
            "connections": cli.connections,
            "elapsed_ms": round3(m.elapsed_ms),
            "throughput_mib_s": round3(throughput),
            "retries": m.retries,
            "accepted_connections": m.accepted,
            "sha256": m.sha256,
            "expected_sha256": m.expected_sha256,
            "verified": m.verified,
            "ok": ok,
            "error": m.error,
        });
        println!("{line}");
    }

    elapsed_ok.sort_by(|a, b| a.total_cmp(b));
    let stats = if elapsed_ok.is_empty() {
        json!({ "median": null, "min": null, "max": null })
    } else {
        json!({
            "median": round3(median(&elapsed_ok)),
            "min": round3(elapsed_ok[0]),
            "max": round3(*elapsed_ok.last().expect("non-empty")),
        })
    };
    let summary = json!({
        "type": "summary",
        "scenario": scenario.to_string(),
        "size_bytes": size,
        "connections": cli.connections,
        "runs": cli.runs,
        "ok_runs": cli.runs - failed,
        "failed_runs": failed,
        "elapsed_ms": stats,
    });
    println!("{summary}");

    Ok(if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
