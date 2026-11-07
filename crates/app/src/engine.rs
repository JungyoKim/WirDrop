//! WirDrop의 관리자 권한 쪽: AWDL 링크, AirDrop 받는 쪽과 보내는 쪽.
//!
//! 트레이 앱이 띄우고 (WirDropEngine 서비스로, 서비스가 없으면 UAC 창으로), 엔진은 트레이의
//! 파이프로 다시 접속합니다. 들어오는 요청마다 트레이에 묻고, 트레이가 시키는 보내기를 합니다.
//! 파이프가 닫히면 링크를 내리고 끝납니다.
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::sync::mpsc;
use wirdrop::airdrop::{self, Media, Offer, Outcome, Payload, ReceiverEvent, Request, SendEvent};
use wirdrop::owly_airdrop::proto::apple_id::{self, Relation};
use wirdrop::owly_awdl::MacAddr;
use wirdrop::{Account, Discovered, Discovery, FirewallRules, Identity, Link, LinkConfig, Profile, Receiving, Seat, State, Target};

use crate::ipc::{Conn, EngineConfig, FileInfo, IncomingInfo, Msg, Phase, Role, SendState, TargetInfo, TargetRef};

type Pipe = Conn<NamedPipeClient>;

/// 보내기가 받는 기기가 나타나길 기다리는 시간.
const FIND_FOR: Duration = Duration::from_secs(45);
/// 마지막 요청 뒤 BLE 깨우기가 이어지는 시간. 받는 기기 찾기는 링크가 올라와 있는 동안 계속하므로,
/// 찾은 기기는 떠날 때까지 목록에 남습니다.
const BROWSE_FOR: Duration = Duration::from_secs(90);
/// 받는 기기의 AWDL 상대가 조용해도 되는 시간. 넘으면 목록에서 뺍니다. 쉬는 iPhone은 10~30초마다 몇
/// 초씩 AWDL을 켜므로 더 짧으면 깜빡이고, mDNS 기록만으로는 떠난 뒤 2분 동안 목록에 남습니다.
const QUIET_FOR: Duration = Duration::from_secs(30);

/// 받기 요청들: 답을 기다리는 것과, 수락해서 받고 있는 것 (수락한 때부터 속도를 잽니다).
#[derive(Default)]
struct Inbox {
    asked: HashMap<u64, Request>,
    taking: HashMap<u64, Instant>,
}

pub fn main() -> io::Result<()> {
    crate::paths::init_log("engine");
    wirdrop::init_tls();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let result = rt.block_on(run());
    if let Err(e) = &result {
        tracing::error!("engine stopped: {e}");
    }
    result
}

async fn run() -> io::Result<()> {
    let mut pipe = Conn::connect(Role::Engine, Duration::from_secs(20)).await?;
    let mut cfg = loop {
        match pipe.recv().await? {
            Some(Msg::Config { cfg }) => break cfg,
            Some(Msg::Shutdown) | None => return Ok(()),
            Some(_) => {}
        }
    };
    // 받는 쪽 포트들의 방화벽 규칙. 링크가 바뀌어도 이어서 씁니다.
    let mut firewall: Option<(Vec<u16>, FirewallRules)> = None;
    loop {
        match session(&mut pipe, &mut cfg, &mut firewall).await {
            Ok(End::Shutdown) => return Ok(()),
            Ok(End::Restart) => {}
            Err(e) => {
                tracing::error!("link failed: {e}");
                status(&mut pipe, Phase::Failed, &e.to_string()).await?;
                // 트레이가 다시 시킬 때까지 기다립니다 (어댑터를 꽂았을 때 등).
                loop {
                    match pipe.recv().await? {
                        Some(Msg::Reconnect) => break,
                        Some(Msg::Config { cfg: c }) => cfg = c,
                        Some(Msg::Shutdown) | None => return Ok(()),
                        Some(_) => {}
                    }
                }
            }
        }
    }
}

enum End {
    Shutdown,
    Restart,
}

async fn status(pipe: &mut Pipe, phase: Phase, detail: &str) -> io::Result<()> {
    pipe.send(&Msg::Status {
        phase,
        detail: detail.to_string(),
    })
    .await
}

/// 받는 기기를 기다리는 보내기.
struct Pending {
    job: u64,
    target: TargetRef,
    files: Vec<PathBuf>,
    deadline: Instant,
}

/// 찾은 받는 기기들, mDNS 인스턴스별로 찾은 때와 함께. 보내기도 함께 씁니다: 찾은 뒤에 잠든 기기는
/// 다시 찾아야 하는데, iPhone은 AirDrop이 깰 때마다 새 인스턴스(와 흔히 새 주소)로 돌아옵니다.
type Targets = Arc<Mutex<HashMap<String, (Instant, Target)>>>;

/// 세션 안의 Apple ID 하나 (없으면 자체 서명 신원): 자기 받는 쪽과 보내는 쪽, 그리고 세션이 도는
/// 동안 appleid가 그 폴더에서 갱신하는 것.
struct Member {
    identity: Identity,
    profile: Profile,
    /// 신원 폴더. 자체 서명 신원이면 None.
    dir: Option<PathBuf>,
    contacts_seen: Option<SystemTime>,
    /// 확인 기록과 인증서가 마지막으로 바뀐 때.
    files_seen: (Option<SystemTime>, Option<SystemTime>),
}

impl Member {
    fn account(&self) -> Option<&Account> {
        self.identity.account()
    }
}

/// cfg가 가리키는 모든 Apple ID, 하나마다 자기 Account와 함께. 읽을 수 있는 것이 없으면 자체 서명
/// 신원. 읽지 못하는 것은 경고를 남기고 뺍니다.
fn members(cfg: &EngineConfig, state: &State) -> io::Result<Vec<Member>> {
    let mut members = Vec::new();
    for dir in &cfg.apple_ids {
        let identity = match Identity::apple_id_as(&dir.identity, Account::new()) {
            Ok(identity) => identity,
            // 엔진은 LocalSystem 서비스입니다: 그 사용자만 읽을 수 있는 폴더는 비어 보입니다.
            Err(e) => {
                tracing::warn!(
                    "can't read the Apple ID identity in {} ({e}), so going on without it",
                    dir.identity.display()
                );
                continue;
            }
        };
        let service_id = if dir.key.is_empty() {
            state.service_id()?
        } else {
            state.service_id_for(&dir.key)?
        };
        let d = &dir.identity;
        members.push(Member {
            identity,
            profile: Profile::new(&cfg.name, service_id),
            dir: Some(d.clone()),
            contacts_seen: wirdrop::identity::contacts_changed(d),
            files_seen: (
                wirdrop::identity::changed(d, "validation_record.cms"),
                wirdrop::identity::changed(d, "certificate.der"),
            ),
        });
    }
    if members.is_empty() {
        members.push(Member {
            identity: Identity::anonymous(state.dir())?,
            profile: Profile::new(&cfg.name, state.service_id()?),
            dir: None,
            contacts_seen: None,
            files_seen: (None, None),
        });
    }
    for m in &mut members {
        m.profile.media = Media {
            hevc: cfg.hevc,
            heif: cfg.heif,
        };
        m.profile.model = model(cfg);
    }
    Ok(members)
}

/// 이 PC가 iPhone에 알리는 모델 이름: 고른 Mac 모델, 없으면 win::auto_model이 고른 것. iPhone은
/// 모델로 기기 그림을 그리는데 (나의 기기에서, 그리고 보낸 사람으로) "Windows"에는 그림이 없어
/// 빈칸이었습니다. 열 수 있는 형식은 따로(미디어 능력) 알리므로 받는 파일은 달라지지 않습니다.
fn model(cfg: &EngineConfig) -> String {
    if cfg.model.is_empty() {
        crate::win::auto_model().into()
    } else {
        cfg.model.clone()
    }
}

/// 받는 쪽들의 이벤트를 합친 것. 각 이벤트에 받은 멤버의 번호가 붙습니다.
struct Receivers {
    running: Vec<Receiving>,
    events: mpsc::Receiver<(usize, ReceiverEvent)>,
}

/// 링크 한 번의 실행: 실패하거나, 트레이가 다시 시작하라고 하거나, 파이프가 닫힐 때까지.
/// firewall에는 받는 쪽 포트의 규칙이 있습니다. 처음에 (어댑터가 올라오는 동안) 만들고 다음
/// 실행에도 씁니다.
async fn session(pipe: &mut Pipe, cfg: &mut EngineConfig, firewall: &mut Option<(Vec<u16>, FirewallRules)>) -> io::Result<End> {
    tracing::info!("starting the link: {cfg:?}");
    status(pipe, Phase::Starting, "turning on the USB adapter").await?;
    let usbfwd = crate::paths::beside("usbfwd.exe");
    let stale = crate::win::kill_running(&usbfwd);
    if stale > 0 {
        tracing::warn!("ended {stale} usbfwd left over from an earlier run");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let state = State::open(crate::paths::data_dir())?;
    let mac = state.awdl_mac()?;
    let mac_s = mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
    tracing::info!("state in {}, AWDL address {mac_s}", state.dir().display());
    let mut members = members(cfg, &state)?;
    // 이 PC의 모든 Apple ID: 그중 어느 것의 기기든 "내 기기"로 보고 묻지 않고 받습니다.
    let household: Vec<Account> = members.iter().filter_map(Member::account).cloned().collect();
    let mut link_cfg = LinkConfig::usb(&cfg.name, state.awdl_mac()?, usbfwd, crate::paths::beside("wintun.dll"));
    if let Some(country) = &cfg.country {
        link_cfg.usbfwd.env.push(("USBFWD_COUNTRY".into(), country.clone()));
    }
    // BE6500UA(RTL8912AU)는 Wi-Fi 6(HE)을 받으므로 iPhone에 알립니다 (usbfwd도 이것을 먼저 고릅니다).
    // 보내는 패킷은 usbfwd가 순서대로 7935바이트까지 A-MSDU로 묶으므로 owly는 묶지 않습니다: owly가 둘씩
    // 묶은 프레임은 usbfwd가 더 합치지 못합니다.
    if crate::win::find_adapter().is_some_and(|a| a.contains("RTL8912AU")) {
        link_cfg.tuning.awdl_ext_flags |= wirdrop::owly_awdl::tlv::ext_flag::HE;
        link_cfg.tuning.amsdu_max_frame = None;
    }
    tracing::info!("AWDL extension flags 0x{:04x}", link_cfg.tuning.awdl_ext_flags);
    let ports: Vec<u16> = (0..members.len() as u16).map(|i| airdrop::PORT + i).collect();
    // netsh는 1초 가까이 걸리니, 더 오래 걸리는 어댑터와 나란히 합니다.
    let rules = if firewall.as_ref().is_some_and(|(p, _)| *p == ports) {
        None
    } else {
        if let Some(old) = firewall.take() {
            tokio::task::spawn_blocking(move || drop(old));
        }
        let p = ports.clone();
        Some(tokio::task::spawn_blocking(move || FirewallRules::allow_current_exe_on(&p)))
    };
    let start = tokio::task::spawn_blocking(move || Link::start(link_cfg));
    tokio::pin!(start);
    let mut link = loop {
        tokio::select! {
            r = &mut start => break r.map_err(io::Error::other)??,
            m = pipe.recv() => match m? {
                None | Some(Msg::Shutdown) => return Ok(End::Shutdown),
                Some(Msg::Config { cfg: c }) => *cfg = c,
                Some(_) => {}
            },
        }
    };
    if let Some(rules) = rules {
        *firewall = Some((ports, rules.await.map_err(io::Error::other)??));
    }
    let mut receiving = start_receiving(&link, &members, &household, cfg).await?;
    status(pipe, Phase::Ready, "").await?;
    tracing::info!(
        "ready as {} (Apple IDs: {}, media taken as is: HEVC {}, HEIF {})",
        cfg.name,
        household.len(),
        cfg.hevc,
        cfg.heif
    );

    let mut peers = link.peers();
    let mut inbox = Inbox::default();
    // 처음부터 듣습니다: AirDrop이 켜진 폰(최근에 깨웠거나 보내는 중)은 탐색기 메뉴를 여는 순간
    // 목록에 있습니다.
    let mut discovery: Option<Discovery> = match start_discovery(&link, &members, false).await {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::warn!("not listening for receivers until asked: {e}");
            None
        }
    };
    let mut browse_until = Instant::now();
    let targets: Targets = Arc::default();
    let mut pending: Vec<Pending> = Vec::new();
    let mut running: HashMap<u64, tokio::task::AbortHandle> = HashMap::new();
    let (updates_tx, mut updates) = mpsc::unbounded_channel::<(u64, SendState)>();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut contacts_checked = Instant::now();
    // AWDL 상대마다 마지막으로 들린 때, 그리고 상대가 조용해서 뺀 받는 기기들: 다시 들리는 순간 새
    // mDNS 알림 없이 다시 띄웁니다.
    let mut heard: HashMap<MacAddr, Instant> = HashMap::new();
    let mut hidden: HashMap<String, Target> = HashMap::new();

    loop {
        tokio::select! {
            m = pipe.recv() => {
                let Some(m) = m? else { return Ok(End::Shutdown) };
                match m {
                    Msg::Shutdown => return Ok(End::Shutdown),
                    Msg::Reconnect => return Ok(End::Restart),
                    Msg::Config { cfg: c } => {
                        if c.apple_ids != cfg.apple_ids {
                            // 로그인이나 로그아웃: 지금 있는 Apple ID로 다시 시작합니다.
                            *cfg = c;
                            return Ok(End::Restart);
                        }
                        let restart = c.name != cfg.name
                            || c.save_dir != cfg.save_dir
                            || c.receive != cfg.receive
                            || c.everyone != cfg.everyone
                            || c.model != cfg.model
                            || (c.hevc, c.heif) != (cfg.hevc, cfg.heif);
                        *cfg = c;
                        if restart {
                            for m in &mut members {
                                m.profile.name = cfg.name.clone();
                                m.profile.media = Media { hevc: cfg.hevc, heif: cfg.heif };
                                m.profile.model = model(cfg);
                            }
                            // 옛 받는 쪽들이 먼저 포트를 놓아야 합니다.
                            drop(receiving.take());
                            receiving = start_receiving(&link, &members, &household, cfg).await?;
                            if let Some(d) = &discovery {
                                let waking = d.waking();
                                // 옛 것이 먼저 BLE 광고를 놓아야 합니다.
                                drop(discovery.take());
                                discovery = Some(start_discovery(&link, &members, waking).await?);
                            }
                        }
                    }
                    Msg::Answer { id, accept } => match inbox.asked.remove(&id) {
                        Some(r) if accept => {
                            inbox.taking.insert(id, Instant::now());
                            r.accept();
                        }
                        Some(r) => r.decline(),
                        None => tracing::info!("answer for {id}, which is gone"),
                    },
                    Msg::Discover { secs } => {
                        browse_until = browse_until.max(Instant::now() + Duration::from_secs(secs));
                        wake(&mut discovery, &link, &members).await?;
                    }
                    Msg::Send { job, target, files } => {
                        browse_until = browse_until.max(Instant::now() + BROWSE_FOR);
                        wake(&mut discovery, &link, &members).await?;
                        let p = Pending { job, target, files, deadline: Instant::now() + FIND_FOR };
                        match find(&targets, &p.target) {
                            Some(t) => start_send(discovery.as_ref(), t, p, &updates_tx, &mut running, &targets),
                            None => {
                                pipe.send(&Msg::SendUpdate { job, state: SendState::Waiting }).await?;
                                pending.push(p);
                            }
                        }
                    }
                    Msg::Cancel { job } => {
                        if let Some(task) = running.remove(&job) {
                            task.abort();
                            pipe.send(&Msg::SendUpdate { job, state: SendState::Cancelled }).await?;
                        } else if let Some(i) = pending.iter().position(|p| p.job == job) {
                            pending.remove(i);
                            pipe.send(&Msg::SendUpdate { job, state: SendState::Cancelled }).await?;
                        }
                    }
                    Msg::CancelReceive { id } => {
                        let stopped = receiving.as_ref().is_some_and(|r| r.running.iter().any(|x| x.cancel(id)));
                        tracing::info!("request {id}: receiving cancelled ({})", if stopped { "stopped" } else { "already over" });
                    }
                    other => tracing::debug!("ignoring {other:?}"),
                }
            }
            event = next_event(&mut receiving) => match event {
                Some((member, event)) => on_receiver_event(pipe, event, member, &household, &mut inbox).await?,
                None => {
                    tracing::warn!("the receivers stopped");
                    receiving = None;
                }
            },
            found = next_found(&mut discovery) => match found {
                Some(Discovered::Found(t)) => {
                    hidden.remove(&t.service.instance);
                    list(pipe, t, &targets, &mut pending, discovery.as_ref(), &updates_tx, &mut running).await?;
                }
                Some(Discovered::Lost(id)) => {
                    lock(&targets).remove(&id);
                    hidden.remove(&id);
                    pipe.send(&Msg::Lost { id }).await?;
                }
                None => {
                    discovery = None;
                    lock(&targets).clear();
                    hidden.clear();
                    pipe.send(&Msg::LostAll).await?;
                }
            },
            Some((job, state)) = updates.recv() => {
                if state.is_final() {
                    running.remove(&job);
                }
                pipe.send(&Msg::SendUpdate { job, state }).await?;
            }
            changed = peers.changed() => {
                if changed.is_ok() {
                    let now = Instant::now();
                    let present: Vec<MacAddr> = peers.borrow().iter().map(|p| p.mac).collect();
                    pipe.send(&Msg::Peers { count: present.len() }).await?;
                    for mac in &present {
                        heard.insert(*mac, now);
                    }
                    let back: Vec<String> = hidden
                        .iter()
                        .filter(|(_, t)| peer_of(t).is_some_and(|m| present.contains(&m)))
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in back {
                        if let Some(t) = hidden.remove(&id) {
                            tracing::info!("{} is heard again: listed", t.name);
                            list(pipe, t, &targets, &mut pending, discovery.as_ref(), &updates_tx, &mut running).await?;
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if let Some(why) = link.check() {
                    return Err(io::Error::other(why));
                }
                let now = Instant::now();
                let present: Vec<MacAddr> = peers.borrow().iter().map(|p| p.mac).collect();
                for mac in &present {
                    heard.insert(*mac, now);
                }
                heard.retain(|_, at| now.duration_since(*at) < Duration::from_secs(600));
                let quiet: Vec<String> = lock(&targets)
                    .iter()
                    .filter(|(_, (found_at, t))| {
                        peer_of(t).is_some_and(|m| {
                            let last = heard.get(&m).map_or(*found_at, |h| (*h).max(*found_at));
                            !present.contains(&m) && now.duration_since(last) > QUIET_FOR
                        })
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in quiet {
                    let unlisted = lock(&targets).remove(&id);
                    if let Some((_, t)) = unlisted {
                        tracing::info!("{} has been quiet for {}s: unlisted until it's heard again", t.name, QUIET_FOR.as_secs());
                        hidden.insert(id.clone(), t);
                        pipe.send(&Msg::Lost { id }).await?;
                    }
                }
                if now.duration_since(contacts_checked) > Duration::from_secs(30) {
                    contacts_checked = now;
                    if renewed(&mut members) {
                        return Ok(End::Restart);
                    }
                }
                let (expired, waiting): (Vec<Pending>, Vec<Pending>) = pending.drain(..).partition(|p| p.deadline <= now);
                pending = waiting;
                for p in expired {
                    let reason = crate::ipc::NOT_FOUND.to_string();
                    pipe.send(&Msg::SendUpdate { job: p.job, state: SendState::Failed { reason } }).await?;
                }
                if let Some(d) = &mut discovery
                    && d.waking()
                    && now > browse_until
                    && pending.is_empty()
                    && running.is_empty()
                {
                    tracing::info!("done waking phones; the {} found stay listed until they leave", lock(&targets).len());
                    d.set_wake(false).await;
                }
            }
        }
    }
}

async fn start_receiving(link: &Link, members: &[Member], household: &[Account], cfg: &EngineConfig) -> io::Result<Option<Receivers>> {
    if !cfg.receive {
        return Ok(None);
    }
    // Apple ID마다 받는 쪽은 자기 기기와 연락처에 대답하고, 첫 받는 쪽은 ("연락처만"이 아니면)
    // 모르는 사람에게도 대답합니다.
    let (tx, events) = mpsc::channel(64);
    let mut running = Vec::new();
    // id 카운터는 모두 하나를 씁니다: 트레이는 요청을 id만으로 구별합니다.
    let ids = Arc::new(std::sync::atomic::AtomicU64::new(1));
    for (i, m) in members.iter().enumerate() {
        let seat = Seat {
            household: household.to_vec(),
            answers_strangers: cfg.everyone && i == 0,
            port: airdrop::PORT + i as u16,
            ids: ids.clone(),
        };
        let (receiving, mut rx) = airdrop::receive_as(link.interface(), &m.identity, &m.profile, &cfg.save_dir, &seat).await?;
        running.push(receiving);
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if tx.send((i, event)).await.is_err() {
                    break;
                }
            }
        });
    }
    Ok(Some(Receivers { running, events }))
}

async fn start_discovery(link: &Link, members: &[Member], wake: bool) -> io::Result<Discovery> {
    let accounts: Vec<(Identity, Profile)> = members.iter().map(|m| (m.identity.clone(), m.profile.clone())).collect();
    let mut discovery = Discovery::start_as(link.interface(), &accounts, wake).await?;
    // BLE 광고에 깼지만 AirDrop mDNS가 들리지 않는 폰들 (owly DISC-5).
    discovery.knock_peers(link.peers());
    Ok(discovery)
}

/// 주변 폰을 깨웁니다 (BLE 광고). 받는 기기 찾기가 멈춰 있었다면 다시 듣습니다.
async fn wake(discovery: &mut Option<Discovery>, link: &Link, members: &[Member]) -> io::Result<()> {
    match discovery {
        Some(d) => d.set_wake(true).await,
        None => *discovery = Some(start_discovery(link, members, true).await?),
    }
    Ok(())
}

/// appleid가 Apple ID 폴더마다 갱신한 것을 반영합니다: iCloud 연락처(다시 읽음), 확인 기록(그
/// 자리에서 설치), 인증서(true: 링크를 새 인증서로 다시 시작).
fn renewed(members: &mut [Member]) -> bool {
    for m in members {
        let (Some(dir), Some(account)) = (m.dir.clone(), m.account().cloned()) else {
            continue;
        };
        let contacts = wirdrop::identity::contacts_changed(&dir);
        if contacts != m.contacts_seen {
            m.contacts_seen = contacts;
            let n = wirdrop::identity::load_contacts(&dir, &account);
            tracing::info!("contacts of {} read again: {} hashes", dir.display(), n.unwrap_or(0));
        }
        let files = (
            wirdrop::identity::changed(&dir, "validation_record.cms"),
            wirdrop::identity::changed(&dir, "certificate.der"),
        );
        if files != m.files_seen {
            let cert_changed = files.1 != m.files_seen.1;
            m.files_seen = files;
            if cert_changed {
                tracing::info!("the Apple ID certificate in {} was renewed: starting over with it", dir.display());
                return true;
            }
            match wirdrop::identity::install_record(&dir, &account) {
                Ok(_) => tracing::info!("the validation record in {} was renewed: now sending the new one", dir.display()),
                Err(e) => tracing::warn!("renewed validation record unreadable: {e}"),
            }
        }
    }
    false
}

async fn next_event(receiving: &mut Option<Receivers>) -> Option<(usize, ReceiverEvent)> {
    match receiving {
        Some(r) => r.events.recv().await,
        None => std::future::pending().await,
    }
}

async fn next_found(discovery: &mut Option<Discovery>) -> Option<Discovered> {
    match discovery {
        Some(d) => d.next().await,
        None => std::future::pending().await,
    }
}

fn matches(t: &Target, r: &TargetRef) -> bool {
    r.id.as_deref() == Some(t.service.instance.as_str()) || t.name.eq_ignore_ascii_case(&r.name)
}

/// 받는 기기에 닿는 AWDL 상대: 링크 로컬 주소가 MAC으로 만들어집니다.
fn peer_of(t: &Target) -> Option<MacAddr> {
    MacAddr::from_link_local(*t.addr.ip())
}

/// 받는 기기를 목록에 올리고 (찾았거나 다시 들림), 그 기기를 기다리던 보내기를 시작합니다.
async fn list(
    pipe: &mut Pipe,
    t: Target,
    targets: &Targets,
    pending: &mut Vec<Pending>,
    discovery: Option<&Discovery>,
    updates: &mpsc::UnboundedSender<(u64, SendState)>,
    running: &mut HashMap<u64, tokio::task::AbortHandle>,
) -> io::Result<()> {
    let info = TargetInfo {
        id: t.service.instance.clone(),
        name: t.name.clone(),
        model: t.model.clone(),
        verified: t.verified,
    };
    lock(targets).insert(info.id.clone(), (Instant::now(), t.clone()));
    pipe.send(&Msg::Found { target: info }).await?;
    let (ready, waiting): (Vec<Pending>, Vec<Pending>) = pending.drain(..).partition(|p| matches(&t, &p.target));
    *pending = waiting;
    for p in ready {
        start_send(discovery, t.clone(), p, updates, running, targets);
    }
    Ok(())
}

fn lock(targets: &Targets) -> std::sync::MutexGuard<'_, HashMap<String, (Instant, Target)>> {
    targets.lock().unwrap_or_else(|e| e.into_inner())
}

/// r이 가리키는 받는 기기: 그 인스턴스나 이름으로 마지막에 찾은 것.
fn find(targets: &Targets, r: &TargetRef) -> Option<Target> {
    lock(targets)
        .values()
        .filter(|(_, t)| matches(t, r))
        .max_by_key(|(at, _)| *at)
        .map(|(_, t)| t.clone())
}

fn start_send(
    discovery: Option<&Discovery>,
    target: Target,
    p: Pending,
    updates: &mpsc::UnboundedSender<(u64, SendState)>,
    running: &mut HashMap<u64, tokio::task::AbortHandle>,
    targets: &Targets,
) {
    let job = p.job;
    let Some(discovery) = discovery else {
        let _ = updates.send((
            job,
            SendState::Failed {
                reason: "browsing stopped".into(),
            },
        ));
        return;
    };
    let sender = discovery.sender();
    let updates = updates.clone();
    let targets = targets.clone();
    tracing::info!(
        "sending {} file(s) to {} ({}) as account {}",
        p.files.len(),
        target.name,
        target.addr,
        target.via
    );
    let task = tokio::spawn(async move {
        let asked = Instant::now();
        let payload = Payload::Files(p.files);
        let wanted = TargetRef {
            id: Some(target.service.instance.clone()),
            name: target.name.clone(),
        };
        let mut target = target;
        let mut attempt = 0u64;
        let (result, accepted, total) = loop {
            attempt += 1;
            let (progress_tx, mut progress) = mpsc::unbounded_channel();
            let forward = {
                let updates = updates.clone();
                tokio::spawn(async move {
                    let mut last: Option<Instant> = None;
                    // 받는 기기가 수락한 때와 올리는 크기.
                    let mut accepted: Option<Instant> = None;
                    let mut total = 0;
                    while let Some(event) = progress.recv().await {
                        let state = match event {
                            SendEvent::Asking => {
                                // 여기까지가 우리 쪽 준비: 나머지는 연결과 받는 기기의 판단.
                                tracing::info!("send {job}: asking after {:.2} s", asked.elapsed().as_secs_f64());
                                SendState::Asking
                            }
                            SendEvent::Accepted => {
                                accepted = Some(Instant::now());
                                SendState::Accepted
                            }
                            SendEvent::Progress(p) => {
                                total = p.total;
                                // 진행 막대에는 초당 몇 번이면 충분합니다.
                                if last.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) && p.sent < p.total {
                                    continue;
                                }
                                last = Some(Instant::now());
                                SendState::Progress {
                                    sent: p.sent,
                                    total: p.total,
                                }
                            }
                        };
                        let _ = updates.send((job, state));
                    }
                    (accepted, total)
                })
            };
            let _ = updates.send((job, SendState::Packing));
            let result = sender.send(&target, &payload, Some(progress_tx)).await;
            let (accepted, total) = forward.await.unwrap_or_default();
            // 받는 기기가 답하기도 전에 연결이 없습니다: 찾은 뒤에 잠들었고, 이 보내기가 시작한
            // 깨우기로 돌아오는데 흔히 새 인스턴스로 옵니다. FIND_FOR가 지날 때까지 마지막에 찾은
            // 것으로 해 봅니다.
            match &result {
                Err(e) if accepted.is_none() && asked.elapsed() + Duration::from_secs(attempt.min(3)) < FIND_FOR => {
                    tracing::info!(
                        "send {job} to {} ({}): {e}; trying again as it wakes (attempt {attempt})",
                        target.name,
                        target.addr
                    );
                    let _ = updates.send((job, SendState::Waiting));
                    tokio::time::sleep(Duration::from_secs(attempt.min(3))).await;
                    if let Some(t) = find(&targets, &TargetRef { id: None, ..wanted.clone() }) {
                        target = t;
                    }
                }
                _ => break (result, accepted, total),
            }
        };
        let state = match result {
            Ok(Outcome::Delivered) => SendState::Delivered,
            Ok(Outcome::Declined) => SendState::Declined,
            Ok(Outcome::Failed(reason)) => SendState::Failed { reason },
            Err(e) => SendState::Failed { reason: e.to_string() },
        };
        tracing::info!("send {job} to {}: {state:?}", target.name);
        if let (SendState::Delivered, Some(at)) = (&state, accepted) {
            let secs = at.elapsed().as_secs_f64();
            tracing::info!(
                "send {job}: accepted after {:.2} s, then {:.1} MB delivered in {secs:.2} s ({:.1} MB/s)",
                (at - asked).as_secs_f64(),
                total as f64 / 1e6,
                total as f64 / 1e6 / secs.max(0.001)
            );
        }
        let _ = updates.send((job, state));
    });
    running.insert(job, task.abort_handle());
}

async fn on_receiver_event(pipe: &mut Pipe, event: ReceiverEvent, member: usize, household: &[Account], inbox: &mut Inbox) -> io::Result<()> {
    match event {
        ReceiverEvent::Request(r) => {
            let ask = &r.ask;
            // 증명된 Apple ID(Apple이 서명한 기록과 같은 계정의 TLS 인증서)만 보낸 사람을 말해
            // 줍니다. 그 밖에는 기록이 없을 때처럼 이름과 모델로만 압니다.
            let record = ask.sender_record.as_deref().filter(|_| r.account.is_some());
            // 이 PC의 어느 Apple ID의 기기든 묻지 않고 받습니다. 사람마다 자기 Mac이 자기 기기의
            // 파일을 받는 것처럼.
            let is_own = household.iter().any(|a| a.own(r.account.as_deref()));
            let files = match &ask.offer {
                Offer::Files(files) => files
                    .iter()
                    .map(|f| FileInfo {
                        name: f.name.clone(),
                        size: f.size,
                        kind: f.file_type.clone(),
                    })
                    .collect(),
                Offer::Urls(urls) => urls
                    .iter()
                    .map(|u| FileInfo {
                        name: u.clone(),
                        size: None,
                        kind: "url".into(),
                    })
                    .collect(),
            };
            let info = IncomingInfo {
                id: r.id,
                sender: ask.sender_computer_name.clone(),
                model: ask.sender_model_name.clone(),
                files,
                own: is_own,
                apple_id: record.is_some(),
                person: crate::record::person_key(record, &ask.sender_computer_name, &ask.sender_model_name),
                icon: ask.file_icon.as_deref().and_then(|bytes| save_icon(r.id, bytes)),
            };
            tracing::info!(
                "request {} from {} ({}) to account {member}: {} item(s), own device: {is_own}",
                r.id,
                info.sender,
                info.model,
                info.files.len()
            );
            inbox.asked.insert(r.id, r);
            pipe.send(&Msg::Incoming { req: info }).await
        }
        ReceiverEvent::Withdrawn { id } => {
            inbox.asked.remove(&id);
            pipe.send(&Msg::Withdrawn { id }).await
        }
        ReceiverEvent::Received { id, paths } => {
            // 보내기와 같은 꼴의 한 줄: scripts/check-transfer.ps1이 기준선과 견줍니다.
            if let Some(at) = inbox.taking.remove(&id) {
                let (mb, secs) = (crate::paths::total_size(&paths) as f64 / 1e6, at.elapsed().as_secs_f64());
                tracing::info!("receive {id}: {mb:.1} MB received in {secs:.2} s ({:.1} MB/s)", mb / secs.max(0.001));
            }
            pipe.send(&Msg::Received { id, paths }).await
        }
        ReceiverEvent::Progress { id, written } => pipe.send(&Msg::ReceiveProgress { id, done: written }).await,
        ReceiverEvent::Failed { id, reason } => {
            inbox.taking.remove(&id);
            pipe.send(&Msg::ReceiveFailed { id, reason }).await
        }
        ReceiverEvent::Urls { id, urls } => pipe.send(&Msg::Urls { id, urls }).await,
        ReceiverEvent::Discovered { peer, request, account } => {
            let record = request.sender_record.as_deref();
            // owly의 받는 쪽이 정하는 대로 (apple_id::answering, DISC-4): 보낸 사람의 Apple ID인
            // 받는 쪽이 대답하고, 모르는 사람에게는 첫 받는 쪽이, 나머지는 이름을 주지 않습니다.
            let proven = account.as_deref();
            let answer: std::borrow::Cow<str> = match apple_id::answering(household, record, proven) {
                Some((i, Some(Relation::Own))) if i == member => "our name and Apple ID record (own device)".into(),
                Some((i, Some(Relation::Contact))) if i == member => "our name and Apple ID record (a contact)".into(),
                Some((i, None)) if i == member => "our name only (its own device, not proven here)".into(),
                Some((i, _)) => format!("no name (it's account {i}'s)").into(),
                None if member == 0 => "our name only".into(),
                None => "no name (a stranger, for account 0)".into(),
            };
            tracing::info!(
                "/Discover from {peer} to account {member} ({:?}, {:?}, sender record: {} bytes, {}, {}): answered with {}",
                request.sender_computer_name.as_deref().unwrap_or("-"),
                request.sender_model_name.as_deref().unwrap_or("-"),
                record.map_or(0, <[u8]>::len),
                crate::record::person_key(record, "-", "-"),
                if proven.is_some() { "proven" } else { "not proven" },
                answer
            );
            Ok(())
        }
    }
}

/// /Ask에 실려 온 미리보기를 카드에 쓰려고 저장합니다. JPEG나 PNG만.
fn save_icon(id: u64, bytes: &[u8]) -> Option<PathBuf> {
    let ext = if bytes.starts_with(&[0xff, 0xd8]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG") {
        "png"
    } else {
        return None;
    };
    let dir = crate::paths::cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("ask-{id}-{}.{ext}", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}
