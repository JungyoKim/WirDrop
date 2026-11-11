//! 트레이 앱의 상태와 판단. 태스크 하나가 갖고 이벤트를 하나씩 처리합니다: 엔진과 탐색기 메뉴에서
//! 온 파이프 메시지, 트레이 메뉴 명령, 아일랜드 카드의 버튼, USB 변화.
mod account;
mod lifecycle;
mod receive;
mod send;
mod updates;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::UiHandle;
use super::cards::Cards;
use super::menu;
use super::rate::{self, Rate};
use super::text;
use super::view::{AppleAccount, AppleId, AskLine, Change, Device, JobLine, Snapshot};
use crate::elevate;
use crate::ipc::{self, Conn, EngineConfig, IncomingInfo, Msg, Phase, Role, SendState, TargetInfo, TargetRef};
use crate::settings::{Rule, Settings};
use receive::open_files;

pub enum HubEvent {
    Connected(u64, UnboundedSender<Msg>),
    Message(u64, Msg),
    Disconnected(u64),
    Menu(MenuCmd),
    /// 아일랜드 카드의 버튼이나 카드 자체를 눌렀다: 그 인자 ("accept:3" 같은).
    Pressed(String),
    DevicesChanged,
    /// DevicesChanged 조금 뒤: 어댑터가 꽂혔는지 빠졌는지 봅니다.
    CheckDevices,
    /// 엔진의 UAC 창에 답했습니다.
    Launched(io::Result<()>),
    /// 탐색기 메뉴를 켜거나 껐습니다.
    MenuDone(Result<(), String>),
    /// 이 폴더의 Apple ID(이름은 이렇게)에 대한 appleid --refresh가 이 종료 코드로 끝났습니다 (2:
    /// 저장된 로그인이 사라짐).
    Renewed(i32, PathBuf, String),
    /// 메뉴나 카드에서 시작한 로그인의 진행.
    SignIn(crate::signin::Progress),
    /// 새 릴리스가 있는지 (update.rs).
    UpdateChecked(io::Result<Option<crate::update::Manifest>>),
    /// 서비스의 업데이트 실행이 끝난 방식 (service::update).
    Updated(io::Result<u32>),
}

pub enum MenuCmd {
    /// 보낼 파일. 기기는 마우스 자리의 기기 메뉴에서 고릅니다.
    SendHere(Vec<PathBuf>),
    /// 메뉴에서 고른 기기로 보낼 파일.
    SendTo(TargetRef, Vec<PathBuf>),
    Change(Change),
    /// 이 폴더의 Apple ID에 다시, 또는 None이면 하나 더 로그인합니다.
    SignIn(Option<PathBuf>),
    /// 이 폴더의 Apple ID(이름은 이렇게)에서 로그아웃하기 전에 묻습니다.
    AskSignOut(PathBuf, String),
    OpenSaveDir,
    ToggleReceive,
    ToggleAutostart,
    Reconnect,
    /// 찾아 둔 새 릴리스를 지금 들입니다.
    UpdateNow,
    Exit,
    /// 카드 버튼의 인자를 메뉴에서 고름: 아일랜드가 없을 때 트레이 아이콘의 메뉴가 대신 묻고 알립니다.
    Press(String),
}

struct Client {
    tx: UnboundedSender<Msg>,
    role: Option<Role>,
    pid: u32,
}

struct Job {
    target: TargetRef,
    files: Vec<PathBuf>,
    state: SendState,
    rate: Rate,
    /// 받는 기기가 수락한 때와 올리는 크기.
    accepted: Option<Instant>,
    bytes: u64,
}

struct Ask {
    info: IncomingInfo,
    asked: Option<Instant>,
    /// 전송을 수락한 때 (속도는 여기서부터 잽니다).
    card: Option<Instant>,
    rate: Rate,
    /// 카드를 마지막으로 움직인 때.
    drawn: Option<Instant>,
}

impl Ask {
    fn new(info: IncomingInfo, asked: Option<Instant>) -> Self {
        Ask {
            info,
            asked,
            card: None,
            rate: Rate::default(),
            drawn: None,
        }
    }

    fn total(&self) -> u64 {
        self.info.files.iter().filter_map(|f| f.size).sum()
    }
}

/// 답하지 않은 요청은 이 시간이 지나면 거절합니다: 받는 쪽이 포기하기 직전이라
/// (wirdrop::airdrop::ASK_TIMEOUT) 카드도 함께 사라집니다.
const ASK_FOR: Duration = Duration::from_secs(55);
/// 엔진을 띄운 뒤 접속을 기다리는 시간.
const LAUNCH_FOR: Duration = Duration::from_secs(30);
/// 어댑터를 찾기 전에 USB 변화가 멎어 있어야 하는 시간: 장치가 오갈 때 Windows는 여러 번 알립니다.
const DEVICES_SETTLE: Duration = Duration::from_millis(300);
/// 진행 카드를 가장 자주 움직이는 간격.
const DRAW_EVERY: Duration = Duration::from_millis(500);
/// iCloud 연락처를 다시 읽는 간격 (appleid --contacts).
const CONTACTS_EVERY: Duration = Duration::from_secs(6 * 3600);
/// 로그인을 갱신할지 보는 간격 (appleid --refresh는 때가 된 것만 갱신합니다).
const RENEW_EVERY: Duration = Duration::from_secs(6 * 3600);
/// 로그인이 AirDrop 인증서를 기다리는 동안 (Apple은 암호 뒤 몇 분 동안 주지 않습니다): 갱신이 마저
/// 받으려 하는 간격과 횟수.
const PENDING_EVERY: Duration = Duration::from_secs(3 * 60);
const PENDING_TRIES: u32 = 10;
/// 아이콘 메뉴의 알림([Hub::notice])이 떠 있는 시간: 받기 요청의 질문은 만료될 때까지, 나머지는
/// 이만큼.
const NOTICE_FOR: Duration = Duration::from_secs(15);
/// 찾기(엔진의 BLE 깨우기)가 마지막 요청 뒤 이어지는 시간.
const SEARCH_FOR: Duration = Duration::from_secs(90);
/// 업데이트: 켜고 바로 한 번 보고 (다시 켜면 기다리지 않고 올라가도록), 그다음 몇 시간마다. 켠 뒤로
/// 주고받은 것이 없으면 바로, 있었으면 끝나고 1분 뒤에 들입니다.
const UPDATE_FIRST: Duration = Duration::from_secs(5);
const UPDATE_EVERY: Duration = Duration::from_secs(3600);
const UPDATE_QUIET: Duration = Duration::from_secs(60);
/// 업데이트가 실패한 뒤 다시 볼 때까지. 대개 한 번 더 하면 됩니다.
const UPDATE_RETRY: Duration = Duration::from_secs(30 * 60);
/// 새 릴리스를 알아보지 못했을 때 다시 알아볼 때까지. 노트북은 절전에서 깨어나 Wi-Fi가 붙기 전에
/// 알아보곤 해서, 한 시간을 기다리면 그사이 다시 잠들어 새 버전을 며칠씩 놓쳤습니다.
const UPDATE_CHECK_RETRY: Duration = Duration::from_secs(2 * 60);

pub struct Hub {
    events: UnboundedSender<HubEvent>,
    ui: UiHandle,
    settings: Settings,
    /// 아일랜드 카드. 아일랜드를 띄울 수 없으면 None이고, 그때는 트레이 메뉴가 묻고 알립니다.
    cards: Option<Cards>,
    clients: HashMap<u64, Client>,
    engine: Option<u64>,
    phase: Phase,
    peers: usize,
    adapter: Option<String>,
    devices_dirty: Option<Instant>,
    targets: BTreeMap<String, TargetInfo>,
    asks: HashMap<u64, Ask>,
    /// 최근 요청에서 받은 것들. 카드 버튼에 씁니다.
    received_files: BTreeMap<u64, Vec<PathBuf>>,
    /// 파일이 다 들어오기 전에 "열기"를 누른 요청.
    open_when_done: HashSet<u64>,
    jobs: BTreeMap<u64, Job>,
    next_job: u64,
    launched: Option<Instant>,
    retries: u32,
    failed_at: Option<Instant>,
    /// 사용자가 엔진의 UAC 창을 거절했습니다: 사용자가 원할 때까지 다시 묻지 않습니다.
    declined: bool,
    menu_busy: bool,
    exiting: bool,
    /// appleid가 iCloud 연락처를 마지막으로 읽은 때, 또는 읽을 필요가 없다고 본 때.
    contacts_run: Option<Instant>,
    /// appleid --refresh를 마지막으로 돌린 때.
    renew_run: Option<Instant>,
    /// 암호는 지났지만 AirDrop 인증서는 아직인 로그인들, 그리고 그 뒤 갱신이 마저 받으려 한 횟수.
    pending_signins: HashMap<PathBuf, u32>,
    /// 로그인이 진행 중입니다.
    signing_in: bool,
    /// 진행 중인 찾기: 언제부터 언제까지.
    search: Option<(Instant, Instant)>,
    started: Instant,
    /// update.rs가 새 릴리스를 마지막으로 본 때, 보고 있는지, 찾은 것.
    update_checked: Option<Instant>,
    checking_update: bool,
    /// 마지막으로 새 릴리스를 알아보지 못했습니다(인터넷 없음): [UPDATE_CHECK_RETRY] 뒤에 다시 봅니다.
    update_check_failed: bool,
    update: Option<crate::update::Manifest>,
    /// 서비스가 업데이트를 놓는 중: 그동안 엔진은 내려가 있습니다.
    updating: bool,
    /// 보내기, 받기, 묻기, 로그인을 마지막으로 하던 때. 켠 뒤로 아무것도 없었으면 None입니다.
    busy_at: Option<Instant>,
    /// 마지막 업데이트가 실패했습니다: [UPDATE_RETRY] 뒤에 다시 봅니다.
    update_failed: bool,
}

pub async fn run(events: UnboundedSender<HubEvent>, mut rx: UnboundedReceiver<HubEvent>, ui: UiHandle, greet: bool) {
    let settings = Settings::load();
    let cards = Cards::start(events.clone(), &settings.island_at);
    let menu_installed = elevate::menu_installed();
    // 켜 둔 채 부팅하면 BE6500UA는 가상 CD로 떠 있습니다 (꺼내면 USB 변화로 다시 봅니다).
    crate::driver::eject_driver_cd();
    let mut hub = Hub {
        events: events.clone(),
        ui,
        settings,
        cards,
        clients: HashMap::new(),
        engine: None,
        phase: Phase::Stopped,
        peers: 0,
        adapter: crate::win::find_adapter(),
        devices_dirty: None,
        targets: BTreeMap::new(),
        asks: HashMap::new(),
        received_files: BTreeMap::new(),
        open_when_done: HashSet::new(),
        jobs: BTreeMap::new(),
        next_job: 1,
        launched: None,
        retries: 0,
        failed_at: None,
        declined: false,
        menu_busy: false,
        exiting: false,
        contacts_run: None,
        renew_run: None,
        pending_signins: HashMap::new(),
        signing_in: false,
        search: None,
        started: Instant::now(),
        update_checked: None,
        checking_update: false,
        update_check_failed: false,
        update: None,
        updating: false,
        busy_at: None,
        update_failed: false,
    };
    tracing::info!("adapter: {:?}, Explorer menu: {menu_installed}", hub.adapter);
    tokio::spawn(serve(events.clone()));
    // 탐색기 메뉴의 AirDrop은 WirDrop의 일부입니다: 등록되어 있지 않거나 업데이트가 바꿨으면
    // 등록합니다.
    if !menu_installed || elevate::menu_outdated() {
        tracing::info!("registering the Explorer menu package (installed: {menu_installed})");
        hub.register_menu();
    }
    if greet && hub.adapter.is_none() {
        hub.notify(
            "adapter",
            t!("USB 어댑터를 꽂아 주세요", "Plug in the USB adapter"),
            t!(
                "T3U Nano나 BE6500UA를 꽂으면 바로 시작합니다.",
                "WirDrop starts as soon as a T3U Nano or BE6500UA is in."
            ),
            &[],
        );
    }
    hub.ensure_engine();
    hub.push();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(event) => hub.handle(event),
                None => break,
            },
            _ = tick.tick() => hub.on_tick(),
        }
        if hub.exiting {
            break;
        }
    }
    // 프로세스가 끝나기 전에 엔진이 링크를 내리게 합니다.
    let deadline = Instant::now() + Duration::from_secs(3);
    while hub.engine.is_some() && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(HubEvent::Disconnected(id))) if Some(id) == hub.engine => hub.engine = None,
            Ok(None) => break,
            _ => {}
        }
    }
    hub.ui.quit();
}

/// 파이프를 엽니다: 접속마다 태스크 하나가 줄을 허브로 넘깁니다.
async fn serve(events: UnboundedSender<HubEvent>) {
    let name = ipc::pipe_name();
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("can't serve {name}: {e}");
            return;
        }
    };
    let mut next = 1;
    loop {
        if let Err(e) = server.connect().await {
            tracing::warn!("pipe connect: {e}");
        }
        let fresh = match ServerOptions::new().create(&name) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("pipe: {e}");
                return;
            }
        };
        let client = std::mem::replace(&mut server, fresh);
        tokio::spawn(client_task(next, client, events.clone()));
        next += 1;
    }
}

async fn client_task(id: u64, pipe: NamedPipeServer, events: UnboundedSender<HubEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let _ = events.send(HubEvent::Connected(id, tx));
    let (mut lines, mut writer) = Conn::new(pipe).split();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(msg) = ipc::parse(&line) {
                        let _ = events.send(HubEvent::Message(id, msg));
                    }
                }
                _ => break,
            },
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                let mut bytes = serde_json::to_vec(&msg).unwrap_or_default();
                bytes.push(b'\n');
                if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = events.send(HubEvent::Disconnected(id));
}

impl Hub {
    fn handle(&mut self, event: HubEvent) {
        match event {
            HubEvent::Connected(id, tx) => {
                self.clients.insert(id, Client { tx, role: None, pid: 0 });
            }
            HubEvent::Disconnected(id) => self.disconnected(id),
            HubEvent::Message(id, msg) => self.message(id, msg),
            HubEvent::Menu(cmd) => self.menu(cmd),
            HubEvent::Pressed(args) => self.pressed(&args),
            HubEvent::Renewed(code, root, who) => self.renewed(code, root, who),
            HubEvent::SignIn(progress) => self.sign_in_progress(progress),
            HubEvent::UpdateChecked(result) => {
                self.checking_update = false;
                self.update_check_failed = result.is_err();
                match result {
                    Ok(Some(manifest)) => {
                        tracing::info!("update: {} is out", manifest.version);
                        self.update = Some(manifest);
                    }
                    Ok(None) => tracing::info!("update: {} is the latest", crate::update::CURRENT),
                    Err(e) => tracing::info!("update: can't tell ({e})"),
                }
            }
            HubEvent::Updated(result) => self.updated(result),
            HubEvent::DevicesChanged => {
                // USB 변화는 몰려서 옵니다: 잠깐 멎은 뒤에 봅니다.
                self.devices_dirty = Some(Instant::now());
                let events = self.events.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(DEVICES_SETTLE + Duration::from_millis(50)).await;
                    let _ = events.send(HubEvent::CheckDevices);
                });
            }
            HubEvent::CheckDevices => self.check_devices(),
            HubEvent::Launched(Ok(())) => self.launched = Some(Instant::now()),
            HubEvent::Launched(Err(e)) if e.raw_os_error() == Some(elevate::ERROR_CANCELLED) => {
                tracing::info!("the engine's UAC prompt was declined");
                self.launched = None;
                self.declined = true;
                self.phase = Phase::NeedsSetup;
                self.notify(
                    "engine",
                    t!("관리자 권한이 필요합니다", "Administrator permission needed"),
                    t!(
                        "AirDrop 엔진을 설치하거나 업데이트하려면 허용해 주세요.",
                        "Allow it to install or update the AirDrop engine."
                    ),
                    &[(t!("다시 시도", "Try Again"), "start")],
                );
                self.push();
            }
            HubEvent::Launched(Err(e)) => {
                tracing::warn!("engine launch failed: {e}");
                self.phase = Phase::Failed;
                self.launched = None;
                self.failed_at = Some(Instant::now());
                self.push();
            }
            HubEvent::MenuDone(result) => self.menu_done(result),
        }
    }

    fn send_to(&self, id: u64, msg: Msg) {
        if let Some(c) = self.clients.get(&id) {
            let _ = c.tx.send(msg);
        }
    }

    fn to_engine(&self, msg: Msg) -> bool {
        match self.engine {
            Some(id) => {
                self.send_to(id, msg);
                true
            }
            None => false,
        }
    }

    fn engine_config(&self) -> EngineConfig {
        let (hevc, heif) = crate::win::media_extensions();
        let apple_ids = crate::paths::apple_id_dirs();
        EngineConfig {
            name: self.settings.device_name.clone(),
            save_dir: self.settings.save_dir.clone(),
            receive: self.settings.receive,
            // "연락처만"은 누가 연락처인지 알려면 Apple ID가 필요합니다.
            everyone: !self.settings.contacts_only || apple_ids.is_empty(),
            apple_ids,
            model: self.settings.model.clone().unwrap_or_default(),
            hevc,
            heif,
            country: crate::win::country(),
        }
    }

    fn save(&self) {
        if let Err(e) = self.settings.save() {
            tracing::warn!("can't save settings: {e}");
        }
    }

    /// 알림 한 장: 아일랜드에, 아일랜드가 없으면 트레이 아이콘의 메뉴에.
    fn notify(&self, tag: &str, title: &str, body: &str, buttons: &[(&str, &str)]) {
        if let Some(c) = &self.cards {
            c.info(tag, title, body, buttons);
        }
        self.notice(tag, menu::INFO_GLYPH, title, body, buttons);
    }

    /// 아일랜드가 없어 카드를 띄울 수 없습니다: 트레이 아이콘의 메뉴가 대신 묻고 알립니다.
    fn quiet(&self) -> bool {
        self.cards.is_none()
    }

    /// 카드 tag가 할 말을 트레이 아이콘의 메뉴로 ([Hub::quiet]일 때만). buttons는 (글자, 인자).
    fn notice(&self, tag: &str, glyph: char, title: &str, detail: &str, buttons: &[(&str, &str)]) {
        self.offer(tag, menu::notice_entries(glyph, title, detail, buttons));
    }

    /// 카드 tag 대신 메뉴 entries를 트레이 아이콘에 띄웁니다 ([Hub::quiet]이고 전체 화면 앱이 없을 때).
    fn offer(&self, tag: &str, entries: Vec<wirdrop_menu::Entry>) {
        if self.quiet() && !crate::win::full_screen() {
            self.ui.show_entries(entries, notice_for(tag));
        }
    }

    fn message(&mut self, id: u64, msg: Msg) {
        let from_engine = self.engine == Some(id);
        match msg {
            Msg::Hello { role, pid } => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.role = Some(role);
                    c.pid = pid;
                }
                if role == Role::Engine {
                    if self.engine.is_some() {
                        tracing::warn!("a second engine (pid {pid}) connected; stopping it");
                        self.send_to(id, Msg::Shutdown);
                        return;
                    }
                    tracing::info!("engine connected (pid {pid})");
                    self.engine = Some(id);
                    self.launched = None;
                    // 새 엔진은 요청 번호를 다시 1부터 셉니다.
                    self.asks.clear();
                    self.received_files.clear();
                    self.open_when_done.clear();
                    self.phase = Phase::Starting;
                    self.send_to(id, Msg::Config { cfg: self.engine_config() });
                    self.push();
                }
            }
            Msg::Status { phase, detail } if from_engine => {
                tracing::info!("engine: {phase:?} {detail}");
                if phase == Phase::Ready {
                    self.retries = 0;
                    self.failed_at = None;
                    self.flush_jobs();
                }
                if phase == Phase::Failed {
                    self.failed_at = Some(Instant::now());
                    self.targets.clear();
                }
                self.phase = phase;
                self.push();
            }
            Msg::Peers { count } if from_engine => {
                self.peers = count;
                self.push();
            }
            Msg::Found { target } if from_engine => {
                self.targets.insert(target.id.clone(), target);
                self.push();
            }
            Msg::Lost { id: lost } if from_engine => {
                self.targets.remove(&lost);
                self.push();
            }
            Msg::LostAll if from_engine => {
                self.targets.clear();
                self.push();
            }
            Msg::Incoming { req } if from_engine => self.incoming(req),
            Msg::Withdrawn { id: rid } if from_engine => {
                if self.asks.remove(&rid).is_some()
                    && let Some(c) = &self.cards
                {
                    c.remove(&format!("recv-{rid}"));
                }
                self.push();
            }
            Msg::ReceiveProgress { id: rid, done } if from_engine => self.receive_progress(rid, done),
            Msg::Received { id: rid, paths } if from_engine => self.received(rid, paths),
            Msg::ReceiveFailed { id: rid, reason } if from_engine => {
                let ask = self.asks.remove(&rid);
                let sender = ask.as_ref().map(|a| a.info.sender.clone()).unwrap_or_else(|| "iPhone".into());
                self.open_when_done.remove(&rid);
                if let Some(c) = &self.cards {
                    c.receive_failed(rid, ask.as_ref().map(|a| &a.info), &sender, &reason);
                }
                let why = text::reason_text(&reason);
                self.notice(
                    &format!("recv-{rid}-end"),
                    menu::WARNING_GLYPH,
                    &tf!("{sender}에서 받지 못했습니다", "Couldn't receive from {sender}"),
                    &why,
                    &[],
                );
                self.push();
            }
            Msg::Urls { id: rid, urls } if from_engine => {
                let sender = self.asks.remove(&rid).map(|a| a.info.sender).unwrap_or_else(|| "iPhone".into());
                if let Some(url) = urls.first().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
                    if self.settings.clipboard {
                        crate::clipboard::copy_link(self.ui.hwnd(), url);
                    }
                    if let Some(c) = &self.cards {
                        c.link(rid, &sender, url);
                    }
                }
            }
            Msg::SendUpdate { job, state } if from_engine => self.job_update(job, state),
            Msg::Wake => self.wake(),
            Msg::Pick { files } => {
                self.share(files);
                self.send_to(id, Msg::Ok);
            }
            Msg::OpenSettings => {
                // 다른 WirDrop.exe가 시작했습니다: 모든 것이 있는 메뉴를 엽니다.
                self.ui.show_menu();
                self.send_to(id, Msg::Ok);
            }
            Msg::Exit => self.exit(),
            other => tracing::debug!("ignoring {other:?} from client {id}"),
        }
    }

    fn change(&mut self, change: Change) {
        tracing::info!("change: {change:?}");
        let mut reconfigure = false;
        match change {
            Change::DeviceName(name) => {
                let name = name.trim().to_string();
                if !name.is_empty() && name != self.settings.device_name {
                    self.settings.device_name = name;
                    reconfigure = true;
                }
            }
            Change::Receive(on) => {
                self.settings.receive = on;
                reconfigure = true;
            }
            Change::Receiving { on, contacts_only } => {
                self.settings.receive = on;
                self.settings.contacts_only = contacts_only;
                reconfigure = true;
            }
            Change::SaveDir(dir) => {
                if dir.is_dir() {
                    self.settings.save_dir = dir;
                    reconfigure = true;
                }
            }
            Change::Clipboard(on) => self.settings.clipboard = on,
            Change::PersonRule { key, rule } => self.settings.set_rule(&key, rule),
            Change::ForgetPerson(key) => self.settings.forget(&key),
            Change::Model(model) => {
                if model.as_deref().is_none_or(|m| crate::settings::MODELS.iter().any(|(id, _)| *id == m)) {
                    self.settings.model = model;
                    reconfigure = true;
                }
            }
            Change::SignOut(root) => {
                // 우리 로그인 폴더만. 엔진은 새 설정이 그 신원 없이 다시 시작할 때까지 신원을
                // 메모리에 둡니다.
                if crate::paths::apple_id_roots().contains(&root) {
                    match std::fs::remove_dir_all(&root) {
                        Ok(()) => tracing::info!("signed out of the Apple ID in {}", root.display()),
                        Err(e) => tracing::warn!("can't remove {}: {e}", root.display()),
                    }
                    reconfigure = true;
                }
            }
            Change::Autostart(on) => {
                if let Err(e) = elevate::set_autostart(on) {
                    tracing::warn!("autostart: {e}");
                }
            }
            Change::IslandAt(key) => {
                if wirdrop_island::Position::from_key(&key).is_some() {
                    self.settings.island_at = key;
                    if let Some(c) = &self.cards {
                        c.place(&self.settings.island_at);
                    }
                }
            }
        }
        self.save();
        if reconfigure {
            self.to_engine(Msg::Config { cfg: self.engine_config() });
        }
        self.push();
    }

    fn menu(&mut self, cmd: MenuCmd) {
        match cmd {
            MenuCmd::SendHere(files) => self.share(files),
            MenuCmd::SendTo(target, files) => self.new_job(target, files),
            MenuCmd::Change(change) => self.change(change),
            MenuCmd::SignIn(root) => self.sign_in(root),
            MenuCmd::AskSignOut(root, label) => {
                let yes = format!("signout:{}", root.display());
                self.notify(
                    "signout",
                    &tf!("{label}에서 로그아웃할까요?", "Sign out of {label}?"),
                    t!(
                        "이 PC의 AirDrop 인증서와 로그인 정보를 지웁니다.",
                        "This removes the AirDrop certificate and sign-in from this PC."
                    ),
                    &[(t!("로그아웃", "Sign Out"), &yes), (t!("취소", "Cancel"), "dismiss")],
                );
            }
            MenuCmd::OpenSaveDir => crate::win::open(&self.settings.save_dir.display().to_string()),
            MenuCmd::Press(args) => self.pressed(&args),
            MenuCmd::ToggleReceive => self.change(Change::Receive(!self.settings.receive)),
            MenuCmd::ToggleAutostart => self.change(Change::Autostart(!elevate::autostart())),
            MenuCmd::Reconnect => self.restart_engine(),
            MenuCmd::UpdateNow => self.begin_update(),
            MenuCmd::Exit => self.exit(),
        }
    }

    fn exit(&mut self) {
        tracing::info!("exiting");
        self.exiting = true;
        self.to_engine(Msg::Shutdown);
    }

    /// 카드(또는 그 대신 뜬 메뉴)의 버튼을 눌렀다. args는 "동사:값" 꼴입니다.
    fn pressed(&mut self, args: &str) {
        let (verb, rest) = args.split_once(':').unwrap_or((args, ""));
        tracing::info!("pressed: {args}");
        let id = rest.parse::<u64>().ok();
        match (verb, id) {
            ("accept", Some(id)) => self.answer(id, true, false),
            ("always", Some(id)) => self.answer(id, true, true),
            ("decline", Some(id)) => self.answer(id, false, false),
            ("cancel", Some(job)) => self.cancel(job),
            ("stop", Some(id)) => self.cancel_receive(id),
            ("signin", _) => self.sign_in((!rest.is_empty() && rest != "new").then(|| PathBuf::from(rest))),
            ("signout", _) if !rest.is_empty() => {
                if let Some(c) = &self.cards {
                    c.remove("signout");
                }
                self.change(Change::SignOut(PathBuf::from(rest)));
            }
            ("dismiss", _) => {}
            ("recvopen", Some(id)) => match self.received_files.get(&id) {
                Some(paths) => open_files(paths),
                None => {
                    self.open_when_done.insert(id);
                }
            },
            ("recvdir", Some(id)) => match self.received_files.get(&id).and_then(|p| p.first()) {
                Some(first) => crate::win::reveal(first),
                None => crate::win::open(&self.settings.save_dir.display().to_string()),
            },
            ("open", _) => crate::win::open(rest),
            ("reveal", _) => crate::win::reveal(std::path::Path::new(rest)),
            ("url", _) if rest.starts_with("http") => crate::win::open(rest),
            ("start", _) => self.restart_engine(),
            _ => self.ui.show_menu(),
        }
    }

    fn display_phase(&self) -> Phase {
        // 엔진이 아직 몰라도 뽑혔으면 뽑힌 것입니다.
        if self.adapter.is_none() { Phase::NoAdapter } else { self.phase }
    }

    fn snapshot(&self) -> Snapshot {
        let accounts = crate::paths::apple_id_dirs()
            .into_iter()
            .enumerate()
            .map(|(i, dir)| {
                let d = &dir.identity;
                // 로그인 폴더: identity\ 바로 위 폴더.
                let root = if d.ends_with("identity") {
                    d.parent().map(PathBuf::from).unwrap_or_else(|| d.clone())
                } else {
                    d.clone()
                };
                AppleAccount {
                    label: crate::paths::apple_id_label(&root, i),
                    root,
                }
            })
            .collect();
        Snapshot {
            devices: self.devices(),
            jobs: self
                .jobs
                .values()
                .map(|j| JobLine {
                    target: j.target.name.clone(),
                    state: j.state.clone(),
                })
                .collect(),
            settings: self.settings.clone(),
            apple_id: AppleId {
                can_sign_in: crate::paths::appleid().is_some(),
                accounts,
            },
            autostart: elevate::autostart(),
            asks: self
                .asks
                .iter()
                .filter(|(_, a)| a.asked.is_some())
                .map(|(id, a)| AskLine {
                    id: *id,
                    sender: a.info.sender.clone(),
                    model: a.info.model.clone(),
                    what: text::describe(&a.info.files),
                })
                .collect(),
            receiving: self.asks.values().filter(|a| a.card.is_some()).map(|a| a.info.sender.clone()).collect(),
            version: crate::update::CURRENT,
            update: self.update.as_ref().map(|m| m.version.clone()),
            updating: self.updating,
        }
    }

    /// 바뀐 상태를 트레이 아이콘과 메뉴에 알립니다.
    fn push(&mut self) {
        let phase = self.display_phase();
        let status = match phase {
            Phase::Ready => tf!("준비됨 · {}", "Ready · {}", self.settings.device_name),
            Phase::Starting | Phase::Launching => t!("USB 어댑터 연결 중…", "Connecting the USB adapter…").to_string(),
            Phase::NoAdapter => t!("USB 어댑터 없음", "No USB adapter").to_string(),
            Phase::NeedsSetup => t!(
                "관리자 권한이 필요합니다 · 눌러서 다시 시도",
                "Administrator permission needed · Click to retry"
            )
            .to_string(),
            Phase::Failed => t!("USB 어댑터 다시 연결하는 중…", "Reconnecting the USB adapter…").to_string(),
            Phase::Stopped => t!("꺼져 있음", "Off").to_string(),
        };
        let sending = self.jobs.values().filter(|j| !j.state.is_final()).count();
        let tooltip = if sending > 0 {
            tf!("WirDrop – 보내는 중 ({sending})", "WirDrop – Sending ({sending})")
        } else {
            format!("WirDrop – {}", short(&status, 100))
        };
        self.ui.update(super::TrayView {
            tooltip,
            status,
            ready: phase == Phase::Ready,
            receive: self.settings.receive,
            autostart: elevate::autostart(),
            startable: phase == Phase::NeedsSetup,
            snapshot: Some(Box::new(self.snapshot())),
        });
    }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 태그 tag인 카드의 알림이 아이콘 메뉴에 머무는 시간: 받기 요청의 질문(recv-{id})은 요청이 만료될
/// 때까지.
fn notice_for(tag: &str) -> Duration {
    if tag.starts_with("recv-") && !tag.ends_with("-end") {
        ASK_FOR
    } else {
        NOTICE_FOR
    }
}
