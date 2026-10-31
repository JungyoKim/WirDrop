//! The conformance vectors that need no network: archives, DVZip, and the
//! sender's encodings.

mod harness;

use std::io::Read;

use flate2::Compression;
use harness::{b64, vectors};
use owly_airdrop::driver::sender::prepare;
use owly_airdrop::proto::archive::compress::{Compressor, Decoder, Encoder};
use owly_airdrop::proto::archive::cpio::{self, Entry, Event};
use owly_airdrop::proto::plist;
use sha2::{Digest, Sha256};

fn decompress(kind: Compressor, mut input: &[u8]) -> Vec<u8> {
    let mut decoder = Decoder::new(kind, 1 << 20);
    let mut out = Vec::new();
    let mut buf = vec![0; 4096];
    loop {
        let p = decoder.decode(input, &mut buf).unwrap();
        out.extend_from_slice(&buf[..p.produced]);
        input = &input[p.consumed..];
        if p.consumed == 0 && p.produced == 0 {
            assert!(decoder.at_boundary(), "stream truncated");
            return out;
        }
    }
}

fn entries(mut archive: &[u8]) -> Vec<(Entry, Vec<u8>)> {
    let mut reader = cpio::Reader::new();
    let mut out: Vec<(Entry, Vec<u8>)> = Vec::new();
    loop {
        let (n, event) = reader.feed(archive).unwrap();
        archive = &archive[n..];
        match event {
            Some(Event::Entry(e)) => out.push((e, Vec::new())),
            Some(Event::Data(d)) => out.last_mut().unwrap().1.extend_from_slice(d),
            Some(Event::EntryEnd) => {}
            Some(Event::Trailer) => return out,
            None => assert!(n > 0, "archive truncated"),
        }
    }
}

fn dvzip_chunks(mut stream: &[u8]) -> usize {
    let mut n = 0;
    while !stream.is_empty() {
        // DVZ-1: the top bit marks a stored chunk.
        let len = (u32::from_be_bytes(stream[..4].try_into().unwrap()) & 0x7fff_ffff) as usize;
        stream = &stream[4 + len..];
        n += 1;
    }
    n
}

// spec: T-DVZ-1
#[test]
fn dvzip_vectors_decode_and_reencode() {
    let vectors = vectors();
    for v in vectors["dvzip"].as_array().unwrap() {
        let expect = &v["expect"];
        let input = b64(&v["input_b64"]);
        let output = decompress(Compressor::DvZip, &input);
        assert_eq!(
            output.len() as u64,
            expect["output_len"].as_u64().unwrap(),
            "{}",
            v["name"]
        );
        assert_eq!(
            hex(&Sha256::digest(&output)),
            expect["output_sha256"].as_str().unwrap()
        );
        assert_eq!(
            dvzip_chunks(&input) as u64,
            expect["chunks"].as_u64().unwrap()
        );

        let mut encoder = Encoder::new(Compressor::DvZip, Compression::new(6));
        let mut encoded = encoder.write(&output);
        encoded.extend(encoder.finish());
        assert_eq!(
            dvzip_chunks(&encoded) as u64,
            expect["chunks"].as_u64().unwrap()
        );
        assert_eq!(decompress(Compressor::DvZip, &encoded), output);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// spec: UP-2, UP-2a
#[test]
fn cpio_vectors_decode() {
    let vectors = vectors();
    for v in vectors["cpio"].as_array().unwrap() {
        let decoded = entries(&b64(&v["input_b64"]));
        let files: Vec<_> = decoded.iter().filter(|(e, _)| e.is_file()).collect();
        let expect = v["expect"]["files"].as_array().unwrap();
        assert_eq!(files.len(), expect.len(), "{}", v["name"]);
        for ((entry, data), want) in files.iter().zip(expect) {
            assert_eq!(entry.name, want["path"].as_str().unwrap());
            assert_eq!(entry.mode as u64, want["mode"].as_u64().unwrap());
            assert_eq!(*data, b64(&want["data_b64"]));
        }
    }
}

// spec: T-UP-6
#[test]
fn sender_archive_matches_odc_vectors() {
    let vectors = vectors();
    for v in vectors["cpio"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["format"] == "odc")
    {
        let want = entries(&b64(&v["input_b64"]));
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for (entry, data) in want.iter().filter(|(e, _)| e.is_file()) {
            let path = dir.path().join(entry.name.trim_start_matches("./"));
            std::fs::write(&path, data).unwrap();
            paths.push(path);
        }
        let (_, mut file, _) = prepare(&paths, Compressor::Gzip).unwrap();
        let mut body = Vec::new();
        file.read_to_end(&mut body).unwrap();
        let got = entries(&decompress(Compressor::Gzip, &body));

        // UP-2a: the root entry first, then the files, as in Finder's layout.
        assert_eq!(got[0].0.name, ".");
        assert!(got[0].0.is_dir());
        let files = |list: &[(Entry, Vec<u8>)]| -> Vec<(String, u32, Vec<u8>)> {
            list.iter()
                .filter(|(e, _)| e.is_file())
                .map(|(e, d)| (e.name.clone(), e.mode, d.clone()))
                .collect()
        };
        assert_eq!(files(&got), files(&want), "{}", v["name"]);
        assert_eq!(got.len(), files(&got).len() + 1);
    }
}

// spec: T-ASK-5
#[test]
fn sender_ask_has_the_ios18_shape() {
    use owly_airdrop::proto::http;
    use owly_airdrop::proto::sender::{Config, ask_request};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("IMG_0049.HEIC");
    std::fs::write(&path, vec![1; 1234]).unwrap();
    let (offer, _, _) = prepare(&[path], Compressor::Gzip).unwrap();
    let request = ask_request(
        &Config::new("Test", "486ac2a94ac6"),
        "A3F34E01-0000-4000-8000-000000000001",
        &offer,
    );
    let (head, n) = http::parse_request(&request, &http::Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(head.target, "/Ask");
    harness::check_ios18_ask(&plist::from_bytes(&request[n..], &plist::Limits::default()).unwrap());
}

// spec: T-ASK-1, answered the way iOS declines: 401 on a connection kept open. An iPhone sending
// to us shows "Declined" only for that.
#[test]
fn declines_like_ios_and_still_refuses_the_upload() {
    use owly_airdrop::proto::receiver::{Config, Connection, Event as Rx};
    use std::sync::Arc;
    fn drain(conn: &mut Connection) -> Vec<Rx> {
        std::iter::from_fn(|| conn.poll()).collect()
    }
    fn reply(events: &[Rx]) -> String {
        events
            .iter()
            .find_map(|e| match e {
                Rx::Transmit(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
                _ => None,
            })
            .expect("a response")
    }
    let vectors = vectors();
    let upload = &vectors["upload"].as_array().unwrap()[0];
    let asks = vectors["ask"].as_array().unwrap();
    let ask = asks.iter().find(|a| a["name"] == upload["after_ask"]).unwrap();

    let mut conn = Connection::new(Arc::new(Config::new("Test")));
    conn.receive(&b64(&ask["request_b64"]));
    assert!(drain(&mut conn).iter().any(|e| matches!(e, Rx::Ask(_))));
    conn.decline();
    let events = drain(&mut conn);
    let answer = reply(&events);
    assert!(answer.starts_with("HTTP/1.1 401 "), "{answer}");
    assert!(answer.contains("Connection: keep-alive"), "{answer}");
    assert!(!events.iter().any(|e| matches!(e, Rx::Close)));

    // UP-6: still no upload without an accepted /Ask.
    conn.receive(&b64(&upload["request_b64"]));
    let events = drain(&mut conn);
    assert!(!events.iter().any(|e| matches!(e, Rx::CreateFile(_) | Rx::Data(_))));
    let answer = reply(&events);
    assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
}

// After a decline the connection takes a new /Ask (the user taps again), and after a link share
// too: an iPad sharing from Freeform sends another /Ask on the same connection seconds later.
#[test]
fn takes_another_ask_after_a_decline_or_a_link() {
    use owly_airdrop::proto::messages::Offer;
    use owly_airdrop::proto::receiver::{Config, Connection, Event as Rx};
    use owly_airdrop::proto::sender;
    use std::sync::Arc;
    fn drain(conn: &mut Connection) -> Vec<Rx> {
        std::iter::from_fn(|| conn.poll()).collect()
    }
    let asked = |conn: &mut Connection| drain(conn).iter().any(|e| matches!(e, Rx::Ask(_)));
    let vectors = vectors();
    let ask = b64(&vectors["ask"].as_array().unwrap()[0]["request_b64"]);
    let offer = Offer::Urls(vec!["https://www.apple.com/icloud/".into()]);
    let link = sender::ask_request(&sender::Config::new("iPad", "0123456789ab"), "A3F34E01-0000-4000-8000-0000000000aa", &offer);

    let mut conn = Connection::new(Arc::new(Config::new("Test")));
    conn.receive(&ask);
    assert!(asked(&mut conn));
    conn.decline();
    drain(&mut conn);
    conn.receive(&ask);
    assert!(asked(&mut conn), "a new /Ask after a decline");

    let mut conn = Connection::new(Arc::new(Config::new("Test")));
    conn.receive(&link);
    assert!(asked(&mut conn));
    conn.accept();
    let events = drain(&mut conn);
    assert!(events.iter().any(|e| matches!(e, Rx::Urls(_))));
    assert!(!events.iter().any(|e| matches!(e, Rx::Close)));
    conn.receive(&link);
    assert!(asked(&mut conn), "a new /Ask after a link share");
}

// Two Apple IDs on one host, a receiver each: a stranger gets the name from the receiver that
// answers strangers, and none from the other (DISC-1), so it lists the host once.
#[test]
fn only_one_receiver_of_a_household_names_itself_to_a_stranger() {
    use owly_airdrop::proto::apple_id::Account;
    use owly_airdrop::proto::receiver::{Config, Connection, Event as Rx};
    use owly_airdrop::proto::sender;
    use std::sync::Arc;
    let mut phone = sender::Config::new("Phone", "0123456789ab");
    phone.account = Account::new();
    let discover = sender::discover_request(&phone);
    let (first, second) = (Account::new(), Account::new());
    let answer = |me: &Account, strangers: bool| {
        let mut config = Config::new("PC");
        config.account = me.clone();
        config.household = vec![first.clone(), second.clone()];
        config.answers_strangers = strangers;
        let mut conn = Connection::new(Arc::new(config));
        conn.receive(&discover);
        let out: Vec<u8> = std::iter::from_fn(|| conn.poll())
            .filter_map(|e| match e {
                Rx::Transmit(bytes) => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect();
        String::from_utf8_lossy(&out).into_owned()
    };
    let shown = answer(&first, true);
    let hidden = answer(&second, false);
    assert!(shown.starts_with("HTTP/1.1 200 ") && shown.contains("ReceiverComputerName"), "{shown}");
    assert!(hidden.starts_with("HTTP/1.1 200 ") && !hidden.contains("ReceiverComputerName"), "{hidden}");
}


// spec: T-UP-1
#[test]
fn uploads_complete_however_the_body_arrives() {
    use owly_airdrop::proto::receiver::{Config, Connection, Event as Rx};
    use std::sync::Arc;
    let vectors = vectors();
    let asks = vectors["ask"].as_array().unwrap();
    for v in vectors["upload"].as_array().unwrap() {
        let ask = asks.iter().find(|a| a["name"] == v["after_ask"]).unwrap();
        let request = [b64(&ask["request_b64"]), b64(&v["request_b64"])].concat();
        for size in [1, 7, 100, 1378, 4096, 16384, 65536] {
            let mut conn = Connection::new(Arc::new(Config::new("Test")));
            let mut sent = Vec::new();
            let mut complete = false;
            for piece in request.chunks(size) {
                conn.receive(piece);
                while let Some(event) = conn.poll() {
                    match event {
                        Rx::Ask(_) => conn.accept(),
                        Rx::UploadComplete => {
                            complete = true;
                            conn.finish_upload(Ok(()));
                        }
                        Rx::Transmit(bytes) => sent.push(
                            String::from_utf8_lossy(&bytes[..12.min(bytes.len())]).into_owned(),
                        ),
                        _ => {}
                    }
                }
            }
            assert!(
                complete,
                "{}: in reads of {size}, the upload never completed; sent {sent:?}",
                v["name"]
            );
        }
    }
}
