//! HLS download against a local server. No network access required.
//!
//! The parser has unit tests; what is untested without a server is the part
//! that matters to a user — that a `.m3u8` URL ends up as one playable file
//! holding every segment, in order.

use std::collections::HashMap;
use std::sync::Arc;

use tdm::{Download, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve a fixed set of paths, and return the base URL.
async fn serve(routes: HashMap<String, (&'static str, Vec<u8>)>) -> String {
    let routes = Arc::new(routes);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let routes = Arc::clone(&routes);
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
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let response = match routes.get(&path) {
                    Some((content_type, body)) => {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        out.extend_from_slice(body);
                        out
                    }
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                               Connection: close\r\n\r\n"
                        .to_vec(),
                };
                let _ = sock.write_all(&response).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tdm-media-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[tokio::test]
async fn a_master_playlist_downloads_every_segment_of_its_best_variant() {
    // Two variants; only the higher-bandwidth one should be fetched.
    let master = "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=640x360\n\
         low.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1920x1080\n\
         high.m3u8\n";
    let media = "#EXTM3U\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n\
         #EXTINF:4.0,\n\
         seg0.ts\n\
         #EXTINF:4.0,\n\
         seg1.ts\n\
         #EXTINF:2.0,\n\
         seg2.ts\n\
         #EXT-X-ENDLIST\n";
    let low = "#EXTM3U\n#EXTINF:4.0,\nwrong.ts\n#EXT-X-ENDLIST\n";

    let mut routes = HashMap::new();
    routes.insert("/v/master.m3u8".to_owned(), ("application/x-mpegurl", master.into()));
    routes.insert("/v/high.m3u8".to_owned(), ("application/x-mpegurl", media.into()));
    routes.insert("/v/low.m3u8".to_owned(), ("application/x-mpegurl", low.into()));
    routes.insert("/v/init.mp4".to_owned(), ("video/mp4", b"INIT".to_vec()));
    routes.insert("/v/seg0.ts".to_owned(), ("video/mp2t", vec![b'a'; 1000]));
    routes.insert("/v/seg1.ts".to_owned(), ("video/mp2t", vec![b'b'; 1000]));
    routes.insert("/v/seg2.ts".to_owned(), ("video/mp2t", vec![b'c'; 500]));
    routes.insert("/v/wrong.ts".to_owned(), ("video/mp2t", vec![b'!'; 10]));
    let base = serve(routes).await;

    let dir = temp_dir("hls");
    let dest = dir.join("stream.ts");
    let download = Download::new(format!("{base}/v/master.m3u8"), dest.clone(), 8);
    download.run().await.expect("the manifest downloads");

    // Init segment first, then the segments in playback order.
    let mut expected = b"INIT".to_vec();
    expected.extend(std::iter::repeat_n(b'a', 1000));
    expected.extend(std::iter::repeat_n(b'b', 1000));
    expected.extend(std::iter::repeat_n(b'c', 500));
    assert_eq!(std::fs::read(&dest).expect("output"), expected);

    let snapshot = download.snapshot();
    assert_eq!(snapshot.status, Status::Finished);
    assert_eq!(snapshot.done, expected.len() as u64);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_encrypted_playlist_is_refused_rather_than_saved_as_noise() {
    // Without the key the bytes are unplayable; writing them anyway would look
    // like a successful download of a corrupt file.
    let media = "#EXTM3U\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\"\n\
         #EXTINF:4.0,\n\
         seg0.ts\n\
         #EXT-X-ENDLIST\n";
    let mut routes = HashMap::new();
    routes.insert("/e/index.m3u8".to_owned(), ("application/x-mpegurl", media.into()));
    routes.insert("/e/seg0.ts".to_owned(), ("video/mp2t", vec![0u8; 16]));
    let base = serve(routes).await;

    let dir = temp_dir("enc");
    let dest = dir.join("stream.ts");
    let download = Download::new(format!("{base}/e/index.m3u8"), dest.clone(), 8);
    let err = download.run().await.expect_err("an encrypted stream must fail");
    assert!(err.to_string().contains("encrypted"), "got {err}");
    assert!(!dest.exists(), "no half-written output is left behind");
    assert_eq!(download.snapshot().status, Status::Failed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_manifest_url_is_saved_under_a_container_name() {
    assert_eq!(tdm::media::container_name("playlist.m3u8"), "playlist.ts");
    assert_eq!(tdm::media::container_name("manifest.mpd"), "manifest.mp4");
    // No extension to replace: left alone.
    assert_eq!(tdm::media::container_name("movie.mp4"), "movie.mp4");
    // Nothing but the extension still needs a name.
    assert_eq!(tdm::media::container_name(".m3u8"), "stream.ts");
}
