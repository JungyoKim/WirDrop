//! 송신기와 수신기를 이 PC 안(::1)에서 맞붙이는 회귀 테스트입니다. 무선 없이 AirDrop 프로토콜
//! 쪽(TLS, /Ask, 압축 묶음, 받은 파일 저장)이 깨지지 않았는지 봅니다. 앱과 같은 설정
//! ([airdrop::receiver_config], [airdrop::sender_config])을 씁니다.
//!
//! 속도 점검은 따로 돌립니다 (scripts/check-transfer.ps1):
//! cargo test -p wirdrop --release --test loopback -- --ignored --nocapture
use std::net::{Ipv6Addr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wirdrop::airdrop::{self, Outcome, Payload, ReceiverEvent};
use wirdrop::owly_airdrop::driver::receiver::{Options, Receiver};
use wirdrop::owly_airdrop::driver::sender::Sender;
use wirdrop::owly_airdrop::proto::mdns::RECEIVER_FLAGS;
use wirdrop::{Identity, Profile, Seat};

/// 테스트마다 따로 쓰는 임시 폴더. 끝나면 지웁니다.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("wirdrop-loopback-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// rel에 내용을 쓰고 그 경로를 돌려줍니다 (폴더도 만듭니다).
    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 사진이나 영상처럼 거의 압축되지 않는 내용 (xorshift).
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// 한 번의 전송 결과.
struct Run {
    outcome: Outcome,
    /// 받은 것들 (수락했을 때).
    received: Option<Vec<PathBuf>>,
    /// 보내기를 시작한 때부터 끝까지 걸린 시간: 수락을 기다리는 동안 압축이 함께 돌므로 수락부터가
    /// 아니라 처음부터 잽니다.
    secs: f64,
}

/// payload를 보내고, 수신 쪽은 accept대로 답합니다.
async fn transfer(scratch: &Scratch, payload: Payload, accept: bool) -> Run {
    wirdrop::init_tls();
    std::fs::create_dir_all(scratch.path("pc")).unwrap();
    std::fs::create_dir_all(scratch.path("phone")).unwrap();
    let identity = Identity::anonymous(&scratch.path("pc")).unwrap();
    let config = airdrop::receiver_config(&identity, &Profile::new("PC", "aaaaaaaaaaaa"), &Seat::default());
    let any = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0);
    let (receiver, mut events) = Receiver::bind(any, identity.tls(), Options::new(config, scratch.path("in")))
        .await
        .unwrap();
    let addr = receiver.local_addr().unwrap();
    tokio::spawn(receiver.run());

    let sender = Sender::new(airdrop::sender_config(&Profile::new("Phone", "bbbbbbbbbbbb"), Default::default())).unwrap();
    let (tx, mut progress) = mpsc::unbounded_channel();
    let started = Instant::now();
    let mut send = tokio::spawn(async move { sender.send(addr, &payload, RECEIVER_FLAGS, Some(tx)).await });

    let run = async {
        let (mut outcome, mut received, mut progress_open) = (None, None, true);
        loop {
            tokio::select! {
                event = events.recv() => match event.expect("the receiver stopped") {
                    ReceiverEvent::Request(r) if accept => r.accept(),
                    ReceiverEvent::Request(r) => r.decline(),
                    ReceiverEvent::Received { paths, .. } => received = Some(paths),
                    ReceiverEvent::Failed { reason, .. } => panic!("receiving failed: {reason}"),
                    _ => {}
                },
                event = progress.recv(), if progress_open => match event {
                    Some(_) => {}
                    None => progress_open = false,
                },
                done = &mut send, if outcome.is_none() => outcome = Some(done.unwrap().expect("sending failed")),
            }
            if let Some(outcome) = &outcome
                && (received.is_some() || !accept)
            {
                return Run {
                    outcome: outcome.clone(),
                    received,
                    secs: started.elapsed().as_secs_f64(),
                };
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("the transfer took over two minutes")
}

/// 받은 것들 가운데 이름이 name인 것.
fn find<'a>(received: &'a [PathBuf], name: &str) -> &'a Path {
    received
        .iter()
        .find(|p| p.file_name().is_some_and(|n| n == name))
        .unwrap_or_else(|| panic!("{name} didn't arrive: {received:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_a_folder_arrive_whole_with_their_korean_names() {
    let s = Scratch::new("whole");
    let video = noise(6 << 20, 7);
    let photo = noise(300_000, 11);
    let memo = "회의 메모\n둘째 줄".as_bytes();
    let files = vec![s.write("out/영상 1.mov", &video), s.write("out/메모.txt", memo)];
    s.write("out/앨범/사진 1.heic", &photo);
    let payload = Payload::Files([files, vec![s.path("out/앨범")]].concat());

    let run = transfer(&s, payload, true).await;
    assert_eq!(run.outcome, Outcome::Delivered);
    let got = run.received.expect("nothing was received");
    assert_eq!(std::fs::read(find(&got, "영상 1.mov")).unwrap(), video);
    assert_eq!(std::fs::read(find(&got, "메모.txt")).unwrap(), memo);
    assert_eq!(std::fs::read(find(&got, "앨범").join("사진 1.heic")).unwrap(), photo);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_ask_writes_nothing() {
    let s = Scratch::new("declined");
    let file = s.write("out/사진.jpg", &noise(200_000, 3));
    let run = transfer(&s, Payload::Files(vec![file]), false).await;
    assert_eq!(run.outcome, Outcome::Declined);
    let left = std::fs::read_dir(s.path("in")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "a declined transfer left files behind");
}

/// 속도 점검: 큰 파일 하나를 보내고 MB/s를 찍습니다. WIRDROP_MIN_MBPS가 있으면 그보다 느릴 때 실패합니다.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "속도 점검: scripts/check-transfer.ps1이 --release로 돌립니다"]
async fn loopback_throughput() {
    let s = Scratch::new("speed");
    let size = 256usize << 20;
    let file = s.write("out/큰 영상.mov", &noise(size, 5));
    let run = transfer(&s, Payload::Files(vec![file]), true).await;
    assert_eq!(run.outcome, Outcome::Delivered);
    let mbps = size as f64 / 1e6 / run.secs.max(0.001);
    println!(
        "LOOPBACK {:.1} MB in {:.2} s from the start ({mbps:.1} MB/s)",
        size as f64 / 1e6,
        run.secs
    );
    if let Some(min) = std::env::var("WIRDROP_MIN_MBPS").ok().and_then(|v| v.parse::<f64>().ok()) {
        assert!(mbps >= min, "loopback {mbps:.1} MB/s is under the floor of {min} MB/s");
    }
}
