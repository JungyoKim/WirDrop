//! iOS sends uploads as chunked DVZip, and may keep the connection open after the archive
//! without the last chunk, waiting for the answer. With allow_dvzip_archive_boundary the
//! receiver driver ends the upload at the archive's end; without it the upload hangs.
use std::io::Read;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::path::PathBuf;
use std::time::Duration;

use owly_airdrop::driver::receiver::{Options, Receiver, ReceiverEvent};
use owly_airdrop::driver::{sender::prepare, tls};
use owly_airdrop::proto::archive::compress::Compressor;
use owly_airdrop::proto::receiver::Config;
use owly_airdrop::proto::sender::{self, Action, Outcome, Transfer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{Instant, sleep_until};

/// The sender's upload head, chunked instead of with a length.
fn chunked(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8(head.to_vec()).unwrap();
    let head = text.strip_suffix("\r\n\r\n").unwrap();
    let mut lines: Vec<&str> = head
        .split("\r\n")
        .filter(|l| !l.to_ascii_lowercase().starts_with("content-length:"))
        .collect();
    lines.push("Transfer-Encoding: chunked");
    format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes()
}

/// Uploads a DVZip archive in one chunk and leaves the connection open. Returns what the
/// receiver saved and the sender's outcome, if they came within wait.
async fn open_ended_upload(allow: bool, wait: Duration) -> (Option<Vec<PathBuf>>, Option<Outcome>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clip.bin");
    std::fs::write(&src, (0..400_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
    let identity = tls::Identity::generate().unwrap();
    let mut config = Config::new("PC");
    config.allow_dvzip_archive_boundary = allow;
    let any = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0);
    let (receiver, mut events) = Receiver::bind(any, &identity, Options::new(config, dir.path().join("in")))
        .await
        .unwrap();
    let addr = receiver.local_addr().unwrap();
    tokio::spawn(receiver.run());

    let (offer, mut file, len) = prepare(&[src], Compressor::DvZip).unwrap();
    let mut archive = Vec::new();
    file.read_to_end(&mut archive).unwrap();
    let connector = tokio_rustls::TlsConnector::from(tls::client_config(None).unwrap());
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut stream = connector.connect(tls::server_name(), tcp).await.unwrap();
    let cfg = sender::Config::new("Phone", "0123456789ab");
    let id = "6B1C4F0E-8E1A-4C3B-9D2A-1F2E3D4C5B6A";
    let mut transfer = Transfer::new(&cfg, id, &offer, Some((Compressor::DvZip, Some(len))));

    let (mut saved, mut outcome) = (None, None);
    let mut deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = vec![0; 16 << 10];
    loop {
        while let Some(action) = transfer.poll() {
            match action {
                Action::Transmit(bytes) => {
                    stream.write_all(&bytes).await.unwrap();
                    stream.flush().await.unwrap();
                }
                Action::SendUpload(head) => {
                    stream.write_all(&chunked(&head)).await.unwrap();
                    stream.write_all(format!("{:x}\r\n", archive.len()).as_bytes()).await.unwrap();
                    stream.write_all(&archive).await.unwrap();
                    stream.write_all(b"\r\n").await.unwrap();
                    stream.flush().await.unwrap();
                    // No last chunk, no end of stream.
                    transfer.upload_sent();
                    deadline = Instant::now() + wait;
                }
                Action::Done(o) => outcome = Some(o),
                Action::Accepted | Action::SetTimer(_) | Action::Reconnect => {}
            }
        }
        if saved.is_some() && outcome.is_some() {
            break;
        }
        tokio::select! {
            read = stream.read(&mut buf) => match read {
                Ok(0) | Err(_) => transfer.receive_eof(),
                Ok(n) => transfer.receive(&buf[..n]),
            },
            event = events.recv() => match event.unwrap() {
                ReceiverEvent::Request(request) => request.accept(),
                ReceiverEvent::Received { paths, .. } => saved = Some(paths),
                ReceiverEvent::Failed { reason, .. } => panic!("upload failed: {reason}"),
                _ => {}
            },
            () = sleep_until(deadline) => break,
        }
    }
    (saved, outcome)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_ended_dvzip_upload_finishes_at_the_archive_boundary() {
    let (saved, outcome) = open_ended_upload(true, Duration::from_secs(3)).await;
    let saved = saved.expect("the receiver saved nothing");
    assert_eq!(saved.len(), 1);
    assert_eq!(outcome, Some(Outcome::Delivered));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_boundary_it_waits_for_the_last_chunk() {
    let (saved, outcome) = open_ended_upload(false, Duration::from_secs(1)).await;
    assert!(saved.is_none() && outcome.is_none());
}
