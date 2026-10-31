//! Integration tests against a local HTTP server. No network access required.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tdm::{Download, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Options {
    /// Answer Range requests with 206. When false the server ignores Range and answers 200.
    honour_range: bool,
    /// Pause between 16 KiB chunks, to make transfers slow enough to interrupt.
    chunk_delay: Option<Duration>,
    /// On the first request starting at each of these offsets, send half the bytes and close.
    drop_starts: Vec<usize>,
    /// Cut the first N non-probe responses short, whatever offset they ask
    /// for. Offsets cannot be predicted now that splitting happens on demand,
    /// so counting requests is the only way to reliably exercise the retry.
    drop_first_n: Arc<Mutex<usize>>,
    /// When set, every request head received is appended here.
    heads: Option<Arc<Mutex<Vec<String>>>>,
    /// Answer 206 but report a Content-Range starting this many bytes away
    /// from what was asked for, while sending the bytes for the real range.
    /// Models a rewriting proxy or a broken cache.
    lie_about_range_by: u64,
}

/// Serve `body` on a random local port and return its URL.
async fn serve(body: Arc<Vec<u8>>, options: Options) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen_ranges = Arc::new(Mutex::new(HashSet::<usize>::new()));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let body = Arc::clone(&body);
            let options = options.clone();
            let seen_ranges = Arc::clone(&seen_ranges);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                if let Some(heads) = &options.heads {
                    heads.lock().expect("heads").push(head.clone());
                }
                // Header names are case-insensitive, and hyper sends them lowercase.
                let range = head.lines().find_map(|l| {
                    let l = l.to_ascii_lowercase();
                    let r = l.strip_prefix("range: bytes=")?.trim().to_owned();
                    let (a, b) = r.split_once('-')?;
                    Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                });
                let total = body.len();

                let (status_line, start, end) = match (options.honour_range, range) {
                    (true, Some((start, end))) => {
                        ("206 Partial Content", start, end.min(total - 1))
                    }
                    _ => ("200 OK", 0, total - 1),
                };
                let is_probe = range == Some((0, 0));
                let slice = &body[start..=end];

                let mut headers = format!("HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n", slice.len());
                if status_line.starts_with("206") {
                    let (rs, re) = if options.lie_about_range_by > 0 && !is_probe {
                        let d = options.lie_about_range_by as usize;
                        (start + d, end + d)
                    } else {
                        (start, end)
                    };
                    headers.push_str(&format!("Content-Range: bytes {rs}-{re}/{total}\r\n"));
                }
                headers.push_str("\r\n");
                if sock.write_all(headers.as_bytes()).await.is_err() {
                    return;
                }

                let drop_by_count = !is_probe && {
                    let mut left = options.drop_first_n.lock().expect("drop count");
                    if *left > 0 {
                        *left -= 1;
                        true
                    } else {
                        false
                    }
                };
                let drop_now = drop_by_count
                    || (!is_probe
                        && options.drop_starts.contains(&start)
                        && seen_ranges.lock().expect("seen").insert(start));
                let send = if drop_now { &slice[..slice.len() / 2] } else { slice };
                for chunk in send.chunks(16 * 1024) {
                    if let Some(delay) = options.chunk_delay {
                        tokio::time::sleep(delay).await;
                    }
                    if sock.write_all(chunk).await.is_err() {
                        return;
                    }
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}/file.bin")
}

fn sample_body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
}

fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tdm-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_file_name(format!("{name}.tdmstate")));
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn segmented_download_matches_source_bytes() {
    let body = Arc::new(sample_body(3 * 1024 * 1024 + 123));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            // Without a delay the transfer finishes before the ramp timer
            // fires, so this test used to pass as a single stream — it was
            // named for segmentation but never segmented.
            chunk_delay: Some(Duration::from_millis(10)),
            heads: Some(Arc::clone(&heads)),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("segmented.bin");

    let download = Download::new(url, &dest, 4);
    download.run().await.expect("download");

    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ");
    let snap = download.snapshot();
    assert_eq!(snap.status, Status::Finished);
    assert_eq!(snap.done, body.len() as u64);
    assert!(!dest.with_file_name("segmented.bin.tdmstate").exists());

    assert!(
        distinct_ranges(&heads).len() >= 2,
        "this test claims to cover segmentation but only one range was requested"
    );
}

/// Distinct non-probe byte ranges the server was asked for.
fn distinct_ranges(heads: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    let mut ranges: Vec<String> = heads
        .lock()
        .expect("heads")
        .iter()
        .filter_map(|h| {
            h.lines()
                .map(|l| l.to_ascii_lowercase())
                .find_map(|l| l.strip_prefix("range: bytes=").map(str::to_owned))
        })
        .filter(|r| r != "0-0")
        .collect();
    ranges.sort();
    ranges.dedup();
    ranges
}

#[tokio::test(flavor = "multi_thread")]
async fn server_without_range_support_falls_back_to_one_stream() {
    let body = Arc::new(sample_body(200_000));
    let url = serve(Arc::clone(&body), Options::default()).await;
    let dest = temp_file("single.bin");

    let download = Download::new(url, &dest, 8);
    download.run().await.expect("download");

    assert_eq!(std::fs::read(&dest).expect("read output"), *body);
    assert_eq!(download.snapshot().status, Status::Finished);
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_saves_progress_and_resume_completes_the_file() {
    let body = Arc::new(sample_body(2 * 1024 * 1024));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            chunk_delay: Some(Duration::from_millis(40)),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("paused.bin");

    let first = Download::new(url.clone(), &dest, 2);
    let runner = first.clone();
    let task = tokio::spawn(async move { runner.run().await });

    // Wait for the first progress checkpoint, then pause.
    for _ in 0..200 {
        if first.snapshot().done > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    first.pause();
    task.await.expect("join").expect("paused run returns Ok");

    let paused = first.snapshot();
    assert_eq!(paused.status, Status::Paused);
    assert!(paused.done > 0 && paused.done < body.len() as u64, "done={}", paused.done);
    assert!(dest.with_file_name("paused.bin.tdmstate").exists());

    let second = Download::new(url, &dest, 2);
    second.run().await.expect("resumed run");
    assert_eq!(second.snapshot().status, Status::Finished);
    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ after resume");
}

/// Redirect the first request to `target`, recording what headers each hop saw.
async fn serve_redirect(target: String, heads: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let target = target.clone();
            let heads = Arc::clone(&heads);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                heads.lock().expect("heads").push(String::from_utf8_lossy(&head).to_string());
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}/start.bin")
}

/// A cookie set for one host must not follow a redirect to an unrelated host.
/// `127.0.0.1` and `localhost` are different hosts, so the hop is cross-site
/// even though both resolve to the loopback interface.
#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_dropped_when_a_redirect_crosses_to_another_host() {
    let body = Arc::new(sample_body(64 * 1024));
    let dest_heads = Arc::new(Mutex::new(Vec::new()));
    let final_url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            heads: Some(Arc::clone(&dest_heads)),
            ..Default::default()
        },
    )
    .await;
    // Point the redirect at the same server under a different hostname.
    let cross = final_url.replace("127.0.0.1", "localhost");
    let hop_heads = Arc::new(Mutex::new(Vec::new()));
    let start = serve_redirect(cross, Arc::clone(&hop_heads)).await;
    let dest = temp_file("redirect.bin");

    let download = Download::new(start, &dest, 1);
    download.set_header("Cookie", "session=secret");
    download.set_header("Referer", "https://origin.test/page");
    download.set_header("User-Agent", "tdm-test/1.0");
    download.run().await.expect("download should follow the redirect");

    let first = hop_heads.lock().expect("heads");
    assert!(!first.is_empty(), "the first host was never contacted");
    assert!(
        first[0].to_ascii_lowercase().contains("cookie: session=secret"),
        "the cookie should reach the host it was set for"
    );

    let after = dest_heads.lock().expect("heads");
    assert!(!after.is_empty(), "the redirect target was never contacted");
    for head in after.iter() {
        let lower = head.to_ascii_lowercase();
        assert!(!lower.contains("cookie:"), "cookie leaked across hosts:\n{head}");
        assert!(!lower.contains("referer:"), "referer leaked across hosts:\n{head}");
        // Non-sensitive headers must still survive the hop.
        assert!(lower.contains("user-agent: tdm-test/1.0"), "user-agent was lost");
    }
    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ");
}

/// A server that answers 206 with a Content-Range for a *different* offset
/// must be rejected. If it were trusted, its bytes would land at our offset
/// and the finished file would be silently wrong — which is the failure this
/// check exists to prevent.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_lying_about_the_returned_range_fails_instead_of_corrupting() {
    let body = Arc::new(sample_body(4 * 1024 * 1024));
    let url = serve(
        Arc::clone(&body),
        Options { honour_range: true, lie_about_range_by: 64, ..Default::default() },
    )
    .await;
    let dest = temp_file("lying.bin");

    let download = Download::new(url, &dest, 2);
    let err = download.run().await.expect_err("a mismatched range must not succeed");

    assert!(
        err.to_string().contains("wrong byte range"),
        "expected a range mismatch, got: {err}"
    );
    assert_eq!(download.snapshot().status, Status::Failed);
    // The output must not be passed off as a finished download.
    let on_disk = std::fs::read(&dest).unwrap_or_default();
    assert!(on_disk != *body, "corrupt bytes were accepted as correct");
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_headers_are_sent_with_probe_and_segment_requests() {
    let body = Arc::new(sample_body(2 * 1024 * 1024));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            heads: Some(Arc::clone(&heads)),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("headers.bin");

    let download = Download::new(url, &dest, 2);
    download.set_header("Referer", "https://example.test/page");
    download.set_header("Cookie", "session=abc");
    download.set_header("User-Agent", "tdm-test/1.0");
    download.run().await.expect("download");

    let heads = heads.lock().expect("heads");
    // The engine now splits on demand rather than up front, so the number of
    // connections depends on timing. What matters is that the probe and every
    // segment request carry the headers.
    assert!(heads.len() >= 2, "expected a probe and at least one segment, got {}", heads.len());
    for head in heads.iter() {
        let head = head.to_ascii_lowercase();
        assert!(head.contains("referer: https://example.test/page\r\n"), "missing Referer in {head}");
        assert!(head.contains("cookie: session=abc\r\n"), "missing Cookie in {head}");
        assert!(head.contains("user-agent: tdm-test/1.0\r\n"), "missing User-Agent in {head}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn speed_limit_slows_the_transfer_to_the_configured_average() {
    let body = Arc::new(sample_body(1024 * 1024));
    let url = serve(Arc::clone(&body), Options { honour_range: true, ..Default::default() }).await;
    let dest = temp_file("limited.bin");

    let download = Download::new(url, &dest, 1);
    download.set_speed_limit(Some(512 * 1024));
    let started = std::time::Instant::now();
    download.run().await.expect("download");

    // 1 MiB at 512 KiB/s is about 2 s; allow some slack for scheduling.
    assert!(started.elapsed() >= Duration::from_millis(1800), "finished too fast: {:?}", started.elapsed());
    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ");
}

/// The engine starts with one connection and hands the back half of the
/// neediest range to each new one. This proves the split actually happens on
/// the wire — several distinct byte ranges requested — and that the reassembled
/// file is still byte-identical.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_download_splits_across_several_connections() {
    let body = Arc::new(sample_body(6 * 1024 * 1024));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            // Slow enough that the ramp timer gets a chance to fire.
            chunk_delay: Some(Duration::from_millis(12)),
            heads: Some(Arc::clone(&heads)),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("split.bin");

    Download::new(url, &dest, 8).run().await.expect("download");

    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ after splitting");

    // Collect the distinct ranges requested, ignoring the 0-0 probe.
    let seen = heads.lock().expect("heads");
    let mut ranges: Vec<String> = seen
        .iter()
        .filter_map(|h| {
            h.lines()
                .map(|l| l.to_ascii_lowercase())
                .find_map(|l| l.strip_prefix("range: bytes=").map(str::to_owned))
        })
        .filter(|r| r != "0-0")
        .collect();
    ranges.sort();
    ranges.dedup();

    assert!(
        ranges.len() >= 2,
        "expected the range to be split across connections, saw only: {ranges:?}"
    );
    // Every requested range must start where it claims, never at 0 twice.
    let starts: Vec<&str> = ranges.iter().map(|r| r.split('-').next().unwrap()).collect();
    let mut unique = starts.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(starts.len(), unique.len(), "two connections asked for the same start: {ranges:?}");
}

/// A speed limit is a limit on the whole application, not on each download.
/// With a per-download limiter, three concurrent downloads would run at three
/// times the configured rate.
#[tokio::test(flavor = "multi_thread")]
async fn one_shared_limiter_caps_the_combined_rate_of_several_downloads() {
    use std::sync::Arc as StdArc;
    use tdm::engine::Limiter;

    let body = Arc::new(sample_body(512 * 1024));
    let url = serve(Arc::clone(&body), Options { honour_range: true, ..Default::default() }).await;

    // 3 x 512 KiB at a combined 512 KiB/s is about 3 s. Unshared it would be 1 s.
    let limiter = StdArc::new(Limiter::new(Some(512 * 1024)));
    let mut handles = Vec::new();
    for i in 0..3 {
        let dest = temp_file(&format!("shared_{i}.bin"));
        let mut d = Download::new(url.clone(), &dest, 1);
        d.set_limiter(StdArc::clone(&limiter));
        handles.push(d);
    }

    let started = std::time::Instant::now();
    let runs = handles.iter().map(|d| d.run()).collect::<Vec<_>>();
    for r in futures_util::future::join_all(runs).await {
        r.expect("download");
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(2200),
        "three downloads finished in {elapsed:?}; the shared cap was not applied"
    );
    assert_eq!(limiter.get(), Some(512 * 1024));
}

/// Removing an unfinished download must not leave its bytes on disk.
#[tokio::test(flavor = "multi_thread")]
async fn discarding_a_download_removes_the_partial_file_and_its_sidecar() {
    let body = Arc::new(sample_body(2 * 1024 * 1024));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            chunk_delay: Some(Duration::from_millis(40)),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("discarded.bin");
    let sidecar = dest.with_file_name("discarded.bin.tdmstate");

    let download = Download::new(url, &dest, 2);
    let runner = download.clone();
    let task = tokio::spawn(async move { runner.run().await });
    for _ in 0..200 {
        if download.snapshot().done > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    download.pause();
    task.await.expect("join").expect("paused run");

    assert!(dest.exists(), "precondition: a partial file should exist");
    assert!(sidecar.exists(), "precondition: a sidecar should exist");

    download.discard_partial().await.expect("discard");
    assert!(!dest.exists(), "the partial file was left behind");
    assert!(!sidecar.exists(), "the resume sidecar was left behind");

    // Discarding again is not an error.
    download.discard_partial().await.expect("second discard should be a no-op");
}

/// A category folder that does not exist yet must be created, not reported as
/// "cannot find the file specified".
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_destination_directory_is_created() {
    let body = Arc::new(sample_body(32 * 1024));
    let url = serve(Arc::clone(&body), Options { honour_range: true, ..Default::default() }).await;

    let nested = std::env::temp_dir()
        .join(format!("tdm-test-{}", std::process::id()))
        .join("Category")
        .join("Deeper");
    let _ = std::fs::remove_dir_all(&nested);
    let dest = nested.join("in_new_dir.bin");
    assert!(!nested.exists(), "precondition: the directory must not exist");

    Download::new(url, &dest, 1).run().await.expect("download into a new directory");
    assert_eq!(std::fs::read(&dest).expect("read output"), *body);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_connection_is_retried_and_file_is_complete() {
    let size = 2 * 1024 * 1024 + 9;
    let body = Arc::new(sample_body(size));
    // Cut the first two transfers short whatever range they ask for. The
    // previous version guessed the offsets from the old equal-split layout,
    // and those offsets no longer occur — so half the setup was dead and the
    // test proved less than its name claimed.
    let remaining = Arc::new(Mutex::new(2usize));
    let url = serve(
        Arc::clone(&body),
        Options {
            honour_range: true,
            drop_first_n: Arc::clone(&remaining),
            ..Default::default()
        },
    )
    .await;
    let dest = temp_file("retried.bin");

    let download = Download::new(url, &dest, 2);
    download.run().await.expect("download survives dropped connections");

    assert_eq!(*remaining.lock().expect("drop count"), 0, "the server never dropped anything");
    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ after retry");
    assert_eq!(download.snapshot().status, Status::Finished);
    assert_eq!(download.snapshot().done, size as u64);
}

/// A server that sends no length leaves nothing to `set_len` to, so a fresh
/// download over an older, longer file used to keep that file's tail.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_download_of_unknown_length_does_not_keep_an_old_tail() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nnew").await;
                let _ = sock.shutdown().await;
            });
        }
    });
    let dest = temp_file("no_length.bin");
    std::fs::write(&dest, b"an older and much longer file").unwrap();

    Download::new(format!("http://{addr}/f"), &dest, 1).run().await.expect("download");
    assert_eq!(std::fs::read(&dest).expect("read output"), b"new");
}
