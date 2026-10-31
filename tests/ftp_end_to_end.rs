//! `ftp://` through the real `Download` engine, against an in-process server.
//!
//! `ftp.rs` has its own unit tests, but those exercise the FTP client directly.
//! This checks the part that is easy to get wrong and impossible to see from a
//! unit test: that `Download::run()` recognises the scheme, probes with FTP,
//! plans segments from `REST` support, and reassembles a byte-identical file.

use std::sync::Arc;
use std::time::Duration;

use tdm::{Download, Status};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Minimal FTP server: anonymous login, binary type, SIZE, PASV, REST, RETR.
///
/// `support_rest` toggles the 350/502 answer, which is what decides whether the
/// engine treats the transfer as segmented or as a single stream.
async fn serve_ftp(body: Arc<Vec<u8>>, support_rest: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let body = Arc::clone(&body);
            tokio::spawn(async move {
                let _ = session(sock, body, support_rest).await;
            });
        }
    });
    format!("ftp://127.0.0.1:{}/file.bin", addr.port())
}

async fn session(
    sock: TcpStream,
    body: Arc<Vec<u8>>,
    support_rest: bool,
) -> std::io::Result<()> {
    let (read, mut write) = sock.into_split();
    let mut lines = BufReader::new(read).lines();
    write.write_all(b"220 test ready\r\n").await?;

    // Set by PASV, consumed by RETR.
    let mut data: Option<TcpListener> = None;
    let mut rest: u64 = 0;

    while let Some(line) = lines.next_line().await? {
        let line = line.trim_end().to_owned();
        let (verb, arg) = match line.split_once(' ') {
            Some((v, a)) => (v.to_ascii_uppercase(), a.trim().to_owned()),
            None => (line.to_ascii_uppercase(), String::new()),
        };

        match verb.as_str() {
            "USER" => write.write_all(b"331 need password\r\n").await?,
            "PASS" => write.write_all(b"230 logged in\r\n").await?,
            "TYPE" => write.write_all(b"200 type set\r\n").await?,
            "SIZE" => {
                write
                    .write_all(format!("213 {}\r\n", body.len()).as_bytes())
                    .await?
            }
            // Not every server answers MDTM; make sure the client copes.
            "MDTM" => write.write_all(b"550 not supported\r\n").await?,
            "REST" => {
                if support_rest {
                    rest = arg.parse().unwrap_or(0);
                    write.write_all(b"350 restarting\r\n").await?;
                } else {
                    write.write_all(b"502 not implemented\r\n").await?;
                }
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").await?;
                let port = l.local_addr()?.port();
                data = Some(l);
                // Deliberately the bare form, without parentheses — real
                // servers send it and our parser must not require them.
                write
                    .write_all(
                        format!(
                            "227 Entering Passive Mode 127,0,0,1,{},{}\r\n",
                            port / 256,
                            port % 256
                        )
                        .as_bytes(),
                    )
                    .await?;
            }
            "RETR" => {
                let Some(l) = data.take() else {
                    write.write_all(b"425 no data connection\r\n").await?;
                    continue;
                };
                write.write_all(b"150 opening data connection\r\n").await?;
                let (mut conn, _) = l.accept().await?;
                let from = (rest as usize).min(body.len());
                conn.write_all(&body[from..]).await?;
                conn.shutdown().await?;
                drop(conn);
                rest = 0;
                write.write_all(b"226 transfer complete\r\n").await?;
            }
            "QUIT" => {
                write.write_all(b"221 bye\r\n").await?;
                return Ok(());
            }
            _ => write.write_all(b"500 unknown\r\n").await?,
        }
    }
    Ok(())
}

fn sample_body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(71) % 251) as u8).collect()
}

fn temp_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tdm-ftp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join(format!("{name}.tdmstate")));
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ftp_url_downloads_through_the_engine_byte_identically() {
    let body = Arc::new(sample_body(3 * 1024 * 1024 + 17));
    let url = serve_ftp(Arc::clone(&body), true).await;
    let dest = temp_file("ftp-segmented.bin");

    let download = Download::new(url, &dest, 4);
    download.run().await.expect("ftp download");

    let got = std::fs::read(&dest).expect("read output");
    assert_eq!(got.len(), body.len(), "wrong length");
    assert!(got == *body, "bytes differ after an FTP download");
    assert_eq!(download.snapshot().status, Status::Finished);
    assert_eq!(download.snapshot().done, body.len() as u64);
}

/// A server that cannot restart must be fetched as one stream rather than
/// segmented — otherwise every connection would start at byte 0 and the file
/// would be a pile of overlapping copies.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_rest_is_fetched_as_a_single_stream() {
    let body = Arc::new(sample_body(400_000));
    let url = serve_ftp(Arc::clone(&body), false).await;
    let dest = temp_file("ftp-single.bin");

    let download = Download::new(url, &dest, 8);
    download.run().await.expect("ftp download without REST");

    assert!(std::fs::read(&dest).expect("read output") == *body, "bytes differ");
    assert_eq!(download.snapshot().status, Status::Finished);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_ftp_server_fails_without_hanging() {
    // Port 1 on loopback refuses immediately.
    let dest = temp_file("ftp-refused.bin");
    let download = Download::new("ftp://127.0.0.1:1/nothing.bin", &dest, 2);

    let result = tokio::time::timeout(Duration::from_secs(40), download.run()).await;
    let inner = result.expect("the engine hung instead of failing");
    assert!(inner.is_err(), "a refused connection should be an error");
    assert_eq!(download.snapshot().status, Status::Failed);
}
