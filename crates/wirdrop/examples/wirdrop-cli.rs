//! wirdrop-cli: wirdrop 라이브러리로 보내고 받습니다. 관리자 권한으로 실행하세요 (Wintun, 방화벽
//! 규칙).
//!
//!     wirdrop-cli receive [--save DIR] [--accept-all]
//!     wirdrop-cli send FILE... [--to NAME] [--wait SECS]
//!
//! 옵션: --name NAME (기본: 컴퓨터 이름), --usbfwd PATH, --wintun PATH, --state DIR, --apple-id
//! DIR|none, --channel N, --no-tx-ampdu (상대에게 Block Ack 세션과 A-MPDU 안 씀), --rx-stbc (STBC
//! 수신 알림), --beamformee (상대가 우리 쪽으로 빔포밍), --data-vht NSS/MCS (유니캐스트 데이터
//! 속도, 기본 2/5), --stop-file PATH (이 파일이 생기면 멈춤).
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wirdrop::airdrop::{self, Offer, Payload, ReceiverEvent, SendEvent};
use wirdrop::{Discovered, Discovery, FirewallRules, Identity, Link, LinkConfig, Profile, State};

struct Args {
    cmd: String,
    files: Vec<PathBuf>,
    name: String,
    usbfwd: PathBuf,
    wintun: PathBuf,
    state: PathBuf,
    apple_id: Option<PathBuf>,
    channel: u8,
    save: PathBuf,
    accept_all: bool,
    to: Option<String>,
    wait: u64,
    stop_file: Option<PathBuf>,
    tx_ampdu: bool,
    rx_stbc: bool,
    beamformee: bool,
    data_vht: Option<String>,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

/// 있는 첫 후보, 없으면 첫 후보.
fn pick(candidates: &[Option<PathBuf>]) -> PathBuf {
    let c: Vec<&PathBuf> = candidates.iter().flatten().collect();
    c.iter().find(|p| p.is_file()).or(c.first()).map(|p| p.to_path_buf()).unwrap_or_default()
}

fn parse() -> io::Result<Args> {
    let beside = |f: &str| std::env::current_exe().ok().map(|e| e.with_file_name(f));
    let mut a = Args {
        cmd: String::new(),
        files: Vec::new(),
        name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into()),
        usbfwd: pick(&[beside("usbfwd.exe")]),
        wintun: pick(&[beside("wintun.dll")]),
        state: State::default_dir().ok_or_else(|| bad("no LOCALAPPDATA"))?,
        apple_id: wirdrop::default_apple_id_dir(),
        channel: 149,
        save: std::env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join(r"Downloads\AirDrop"))
            .unwrap_or_default(),
        accept_all: false,
        to: None,
        wait: 60,
        stop_file: None,
        tx_ampdu: true,
        rx_stbc: false,
        beamformee: false,
        data_vht: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| bad(format!("{arg} needs a value")));
        match arg.as_str() {
            "--name" => a.name = value()?,
            "--usbfwd" => a.usbfwd = value()?.into(),
            "--wintun" => a.wintun = value()?.into(),
            "--state" => a.state = value()?.into(),
            "--apple-id" => {
                let v = value()?;
                a.apple_id = (v != "none").then(|| v.into());
            }
            "--channel" => a.channel = value()?.parse().map_err(|_| bad("--channel N"))?,
            "--save" => a.save = value()?.into(),
            "--accept-all" => a.accept_all = true,
            "--to" => a.to = Some(value()?),
            "--wait" => a.wait = value()?.parse().map_err(|_| bad("--wait SECS"))?,
            "--stop-file" => a.stop_file = Some(value()?.into()),
            "--tx-ampdu" => a.tx_ampdu = true,
            "--no-tx-ampdu" => a.tx_ampdu = false,
            "--rx-stbc" => a.rx_stbc = true,
            "--beamformee" => a.beamformee = true,
            "--data-vht" => a.data_vht = Some(value()?),
            s if s.starts_with("--") => return Err(bad(format!("unknown option {s}"))),
            s if a.cmd.is_empty() => a.cmd = s.to_string(),
            s => a.files.push(s.into()),
        }
    }
    match a.cmd.as_str() {
        "receive" => {}
        "send" if !a.files.is_empty() => {}
        _ => {
            return Err(bad(
                "usage: wirdrop-cli receive [--save DIR] [--accept-all] | wirdrop-cli send FILE... [--to NAME]",
            ));
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let args = parse()?;
    let state = State::open(&args.state)?;
    let identity = Identity::load(args.apple_id.as_deref(), state.dir())?;
    let profile = Profile::new(&args.name, state.service_id()?);
    let _firewall = FirewallRules::allow_current_exe(airdrop::PORT)?;
    let mut config = LinkConfig::usb(&args.name, state.awdl_mac()?, &args.usbfwd, &args.wintun);
    config.channel = args.channel;
    config.tuning.tx_ampdu = args.tx_ampdu;
    config.tuning.rx_stbc = args.rx_stbc;
    config.tuning.beamformee = args.beamformee;
    config.usbfwd.data_vht = args.data_vht.clone();
    let started = Instant::now();
    let mut link = tokio::task::spawn_blocking(move || Link::start(config))
        .await
        .map_err(io::Error::other)??;
    println!(
        "link up in {:.0} s: {} (Apple ID identity: {})",
        started.elapsed().as_secs_f64(),
        args.name,
        identity.is_apple_id()
    );
    let result = if args.cmd == "receive" {
        receive(&args, &mut link, &identity, &profile).await
    } else {
        send(&args, &mut link, &identity, &profile).await
    };
    if let Err(e) = &result {
        println!("error: {e}");
    }
    tokio::task::spawn_blocking(move || link.stop()).await.map_err(io::Error::other)?;
    result
}

fn stop_requested(args: &Args) -> bool {
    args.stop_file.as_deref().is_some_and(|p| p.exists())
}

fn offer_summary(offer: &Offer) -> (String, u64) {
    match offer {
        Offer::Files(files) => {
            let total = files.iter().filter_map(|f| f.size).sum();
            let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
            (names.join(", "), total)
        }
        Offer::Urls(urls) => (urls.join(", "), 0),
    }
}

async fn receive(args: &Args, link: &mut Link, identity: &Identity, profile: &Profile) -> io::Result<()> {
    let (receiving, mut events) = airdrop::receive(link.interface(), identity, profile, &args.save).await?;
    println!(
        "ready: receiving as {} on {} into {}",
        profile.name,
        receiving.addr(),
        args.save.display()
    );
    let mut peers = link.peers();
    let mut transfers = std::collections::HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            event = events.recv() => match event {
                None => break,
                Some(ReceiverEvent::Request(request)) => {
                    let (names, total) = offer_summary(&request.ask.offer);
                    println!(
                        "request {}: {} ({}) offers {names}, {:.2} MB",
                        request.id,
                        request.ask.sender_computer_name,
                        request.ask.sender_model_name,
                        total as f64 / 1e6
                    );
                    let accept = args.accept_all || ask_yes_no().await;
                    transfers.insert(request.id, (Instant::now(), total));
                    if accept { request.accept() } else { request.decline() }
                }
                Some(ReceiverEvent::Received { id, paths }) => {
                    let (at, total) = transfers.remove(&id).unwrap_or((Instant::now(), 0));
                    let secs = at.elapsed().as_secs_f64();
                    println!(
                        "received {id}: {} file(s), {:.2} MB in {secs:.1} s ({:.1} Mbit/s)",
                        paths.len(),
                        total as f64 / 1e6,
                        total as f64 * 8.0 / 1e6 / secs.max(0.001)
                    );
                    for p in paths {
                        println!("  {}", p.display());
                    }
                }
                Some(ReceiverEvent::Failed { id, reason }) => println!("failed {id}: {reason}"),
                Some(ReceiverEvent::Progress { .. }) => {}
                Some(ReceiverEvent::Withdrawn { id }) => println!("withdrawn {id}"),
                Some(ReceiverEvent::Urls { id, urls }) => println!("urls {id}: {urls:?}"),
                Some(ReceiverEvent::Discovered { peer, .. }) => println!("discovered by {peer}"),
            },
            changed = peers.changed() => {
                if changed.is_err() { break; }
                let list: Vec<String> = peers.borrow().iter().map(|p| format!("{} ({:?} dBm)", p.name, p.rssi)).collect();
                println!("AWDL peers: [{}]", list.join(", "));
            }
            _ = tick.tick() => {
                if stop_requested(args) { break; }
                if let Some(why) = link.check() { return Err(io::Error::other(why)); }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

async fn ask_yes_no() -> bool {
    println!("accept? [y/N]");
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
    })
    .await
    .unwrap_or(false)
}

async fn send(args: &Args, link: &mut Link, identity: &Identity, profile: &Profile) -> io::Result<()> {
    let mut discovery = Discovery::start(link.interface(), identity, profile, true).await?;
    println!("looking for receivers ({} s)", args.wait);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.wait);
    let target = loop {
        tokio::select! {
            event = discovery.next() => match event {
                Some(Discovered::Found(t)) => {
                    println!("found {} ({:?}, verified: {}) at {}", t.name, t.model, t.verified, t.addr);
                    if args.to.as_ref().is_none_or(|n| t.name.eq_ignore_ascii_case(n)) {
                        break t;
                    }
                }
                Some(Discovered::Lost(instance)) => println!("lost {instance}"),
                None => return Err(io::Error::other("browsing stopped")),
            },
            _ = tokio::time::sleep_until(deadline) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no receiver found"));
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if stop_requested(args) { return Ok(()); }
                if let Some(why) = link.check() { return Err(io::Error::other(why)); }
            }
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let t0 = Instant::now();
    let printer = tokio::spawn(async move {
        let mut next = 0.0;
        while let Some(event) = rx.recv().await {
            match event {
                SendEvent::Asking => {}
                SendEvent::Accepted => println!("accepted after {:.1} s", t0.elapsed().as_secs_f64()),
                SendEvent::Progress(p) => {
                    let done = p.sent as f64 / p.total.max(1) as f64;
                    if done >= next {
                        println!(
                            "sent {:.0}% ({:.2} MB) at {:.1} s",
                            done * 100.0,
                            p.sent as f64 / 1e6,
                            t0.elapsed().as_secs_f64()
                        );
                        next = (done * 10.0).floor() / 10.0 + 0.1;
                    }
                }
            }
        }
    });
    let payload = Payload::Files(args.files.clone());
    let send = discovery.send(&target, &payload, Some(tx));
    tokio::pin!(send);
    // 멈춤 파일과 끊긴 링크도 올리기를 끝냅니다.
    let outcome = loop {
        tokio::select! {
            r = &mut send => break r?,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if stop_requested(args) {
                    println!("stopped during the upload");
                    return Ok(());
                }
                if let Some(why) = link.check() {
                    return Err(io::Error::other(why));
                }
            }
        }
    };
    let _ = printer.await;
    println!("outcome: {outcome:?} after {:.1} s", t0.elapsed().as_secs_f64());
    Ok(())
}
