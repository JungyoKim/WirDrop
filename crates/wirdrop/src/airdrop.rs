//! 링크 위의 AirDrop. owly-airdrop의 tokio 드라이버로 받기(awdl0의 TLS 서버와 mDNS 응답기)와
//! 보내기(BLE 깨우기, mDNS 찾기, /Discover, /Ask, /Upload)를 합니다.
//!
//! PC에 Apple ID가 여럿이면 하나마다 받는 쪽이 따로 있습니다 ([receive_as]): 한 링크 위에 자기 mDNS
//! 서비스, 포트, 인증서를 둡니다. 보내는 기기마다 그중 하나만 대답하므로 그 폰에는 PC가 한 번, 그
//! 폰이 속한 계정의 기기로 보입니다. 보낼 때는 받는 쪽이 아는 계정을 고릅니다
//! ([Discovery::start_as]).
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use owly_airdrop::driver::mdns::{Browse, Mdns, Registration};
use owly_airdrop::driver::receiver::{Canceller, Options, Receiver};
use owly_airdrop::driver::sender::Sender;
use owly_airdrop::proto::mdns::{BrowseEvent, Burst, RECEIVER_FLAGS, Service};
use owly_airdrop::proto::receiver::Config as ReceiverConfig;
use owly_airdrop::proto::sender::Config as SenderConfig;
use owly_awdl::{MacAddr, PeerInfo};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub use owly_airdrop::driver::receiver::{ReceiverEvent, Request};
pub use owly_airdrop::driver::sender::{Event as SendEvent, Payload, Progress};
pub use owly_airdrop::proto::mdns::Found;
pub use owly_airdrop::proto::messages::{AskFile, AskRequest, Offer};
pub use owly_airdrop::proto::sender::Outcome;

use crate::ble::BleWake;
use crate::identity::{Account, Identity};
use crate::link::Interface;

/// 받는 쪽의 TCP 포트. 다른 Apple ID의 받는 쪽은 그다음 포트들을 씁니다.
pub const PORT: u16 = 8771;

/// /Ask가 사용자의 답을 기다리는 시간. 넘으면 거절합니다. owly 기본값(15초)은 알림을 보고 누르기에
/// 너무 짧고, 보내는 iPhone은 이보다 오래 기다립니다.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// 받는 쪽이 도는 동안 mDNS 알림을 되풀이하는 방식 (owly MDNS-7). 우리 기록을 AWDL 액션 프레임에
/// 싣지 않으므로 알림을 놓친 폰은 우리가 있는 줄 모르고, Apple 기기는 다시 묻지 않을 때가 많습니다
/// (시작할 때의 두 번을 놓친 iPad는 끝내 우리를 띄우지 않았습니다). owly-awdl은 멀티캐스트를 262
/// ms마다 16 ms쯤인 0번과 10번 슬롯에서만, 다시 보내지 않고 한 번 보냅니다. 0.3초 간격의 사본은
/// 서로 다른 슬롯에 들어갑니다.
const ANNOUNCE: Burst = Burst {
    count: 3,
    interval: Duration::from_millis(300),
    pause: Duration::from_secs(3),
};

/// 이 PC가 자신을 내보이는 방식.
#[derive(Debug, Clone)]
pub struct Profile {
    /// 폰에 보이는 이름.
    pub name: String,
    /// /Discover와 /Ask의 모델 이름.
    pub model: String,
    /// mDNS 인스턴스 이름이자 SenderID ([crate::identity::State::service_id] 참고).
    pub service_id: String,
    /// 이 PC가 여는 미디어 형식. 보내는 쪽은 이 형식을 그대로 보냅니다.
    pub media: Media,
}

impl Profile {
    pub fn new(name: impl Into<String>, service_id: impl Into<String>) -> Self {
        Profile {
            name: name.into(),
            model: "Windows".into(),
            service_id: service_id.into(),
            media: Media::default(),
        }
    }
}

/// 받는 쪽이 그대로 받는 형식. /Discover로 보내는 쪽에 알립니다 (`ReceiverMediaCapabilities`, owly
/// DISC-3). 그 밖의 것은 보내는 쪽이 /Ask 전에 널리 쓰는 형식으로 바꾼 사본을 만듭니다: HEIC는
/// JPEG, HEVC는 H.264. 74 MB 영상에 iPhone 7이 14초 걸렸으니, 받는 쪽은 열지 못하는 형식을 알려서는
/// 안 됩니다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Media {
    /// HEVC 동영상 (Main, Main 10, HDR 포함; Windows의 HEVC 비디오 확장).
    pub hevc: bool,
    /// HEIF 사진: HEIC와 HEIF (Windows의 HEIF 이미지 확장).
    pub heif: bool,
}

impl Media {
    /// /Discover에 넣는 JSON. iPhone(iOS 26)이 보내는 꼴에서 Windows가 열지 못하는 것(ProRes, Dolby
    /// Vision, Live Photo와 편집 묶음)을 뺍니다. CodecSupport는 iPhone처럼 Codecs 안에 둡니다: 맨
    /// 위에 두면 iPhone이 HEVC를 받지 못하는 것으로 보고 영상을 H.264로 다시 만들어 보냅니다.
    pub fn capabilities(&self) -> Vec<u8> {
        let mut caps = String::from(r#"{"Version":3"#);
        if self.hevc {
            let level = r#"{"VTMaxDecodeLevel":186,"VTIsHardwareAccelerated":true}"#;
            let hvc1 = format!(r#""VTIsHDRAllowedOnDevice":true,"VTSupportedProfiles":[1,2],"VTPerProfileSupport":{{"1":{level},"2":{level}}}"#);
            caps += &format!(
                r#","Codecs":{{"hvc1":{{"Profiles":{{{hvc1}}}}},"CodecSupport":{{"VTIsHDRAllowedOnDevice":true,"VTCodecSupportDict":{{"hvc1":{{{hvc1}}}}}}}}}"#
            );
        }
        if self.heif {
            caps += r#","ContainerFormats":{"public.heif-standard":{"HeifSubtypes":["public.heic","public.heics","public.heif"]}}"#;
        }
        caps.push('}');
        caps.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::Media;

    fn json(media: Media) -> serde_json::Value {
        serde_json::from_slice(&media.capabilities()).expect("valid JSON")
    }

    #[test]
    fn capabilities_say_only_what_windows_opens() {
        assert_eq!(json(Media::default()), serde_json::json!({"Version": 3}));
        let all = json(Media { hevc: true, heif: true });
        assert_eq!(all["Codecs"]["hvc1"]["Profiles"]["VTSupportedProfiles"], serde_json::json!([1, 2]));
        assert_eq!(all["Codecs"]["hvc1"]["Profiles"]["VTPerProfileSupport"]["2"]["VTMaxDecodeLevel"], 186);
        assert_eq!(
            all["Codecs"]["CodecSupport"]["VTCodecSupportDict"]["hvc1"]["VTIsHDRAllowedOnDevice"],
            true
        );
        assert_eq!(all["ContainerFormats"]["public.heif-standard"]["HeifSubtypes"][0], "public.heic");
        let video = json(Media { hevc: true, heif: false });
        assert!(video.get("ContainerFormats").is_none() && video.get("Codecs").is_some());
    }
}

/// 돌고 있는 받는 쪽. 버리면 받기를 멈추고 mDNS 기록을 거둡니다.
pub struct Receiving {
    task: JoinHandle<()>,
    _mdns: Registration,
    addr: SocketAddrV6,
    canceller: Canceller,
}

impl Receiving {
    pub fn addr(&self) -> SocketAddrV6 {
        self.addr
    }

    /// 요청 id의 전송을 그 자리에서 멈춥니다 (묻는 중이든 올리는 중이든, 써 둔 것까지). 이벤트는 더
    /// 오지 않습니다. 이 받는 쪽의 요청이 아니거나 이미 끝났으면 false.
    pub fn cancel(&self, id: u64) -> bool {
        self.canceller.cancel(id)
    }

    pub fn stop(self) {}
}

impl Drop for Receiving {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// PC의 Apple ID들 가운데 이 받는 쪽의 자리.
#[derive(Debug, Clone)]
pub struct Seat {
    /// 받는 쪽이 있는 PC의 모든 Apple ID. 우선순위대로, 이 받는 쪽의 것도 포함합니다. 받는 쪽이
    /// 하나뿐인 PC면 비어 있습니다.
    pub household: Vec<Account>,
    /// 어느 계정에도 속하지 않은 보내는 사람에게 이름으로 대답합니다. PC마다 받는 쪽 하나가 맡아야
    /// 합니다.
    pub answers_strangers: bool,
    pub port: u16,
    /// 요청 id를 받는 곳. PC의 모든 받는 쪽이 같은 것을 써서 id가 겹치지 않습니다.
    pub ids: Arc<AtomicU64>,
}

impl Default for Seat {
    fn default() -> Self {
        Seat {
            household: Vec::new(),
            answers_strangers: true,
            port: PORT,
            ids: Arc::new(AtomicU64::new(1)),
        }
    }
}

/// destination으로 받기 시작합니다. /Ask마다 [ReceiverEvent::Request]가 오니 accept나 decline으로
/// 답하세요 (버리면 거절). 이벤트는 바로바로 읽으세요: 채널이 차 있으면 요청은 답을 못 받고 시간이
/// 지나 끝납니다.
pub async fn receive(
    interface: Interface,
    identity: &Identity,
    profile: &Profile,
    destination: impl Into<PathBuf>,
) -> io::Result<(Receiving, mpsc::Receiver<ReceiverEvent>)> {
    receive_as(interface, identity, profile, destination, &Seat::default()).await
}

/// [receive]와 같되, PC의 여러 Apple ID 중 하나로 받습니다 (identity는 그 계정의 것, profile에 그
/// 서비스 ID). 자기 기기와 연락처에 대답하고, seat가 정하면 모르는 사람에게도 대답합니다.
pub async fn receive_as(
    interface: Interface,
    identity: &Identity,
    profile: &Profile,
    destination: impl Into<PathBuf>,
    seat: &Seat,
) -> io::Result<(Receiving, mpsc::Receiver<ReceiverEvent>)> {
    crate::init_tls();
    let destination = destination.into();
    tokio::fs::create_dir_all(&destination).await?;
    let mut options = Options::new(receiver_config(identity, profile, seat), destination);
    options.ids = seat.ids.clone();
    let (receiver, events) = Receiver::bind(interface.socket_addr(seat.port), identity.tls(), options).await?;
    let canceller = receiver.canceller();
    let addr = receiver.local_addr()?;
    let service = Service {
        instance: profile.service_id.clone(),
        host: format!("airdrop-{}", profile.service_id),
        port: addr.port(),
        addr: interface.addr,
        flags: RECEIVER_FLAGS,
    };
    let mdns = Mdns::new(interface.index).with_burst(Some(ANNOUNCE)).register(service).await?;
    let task = tokio::spawn(receiver.run());
    tracing::info!("receiving as {} on {addr} (service {})", profile.name, profile.service_id);
    Ok((
        Receiving {
            task,
            _mdns: mdns,
            addr,
            canceller,
        },
        events,
    ))
}

/// 수신기 설정: 앱이 쓰는 그대로 (루프백 테스트도 이것으로 받는다).
pub fn receiver_config(identity: &Identity, profile: &Profile, seat: &Seat) -> ReceiverConfig {
    let mut config = ReceiverConfig::new(profile.name.clone());
    config.model_name = profile.model.clone();
    config.media_capabilities = profile.media.capabilities();
    // iOS는 압축 묶음을 다 보낸 뒤에도 연결을 열어 둘 수 있다: 묶음이 끝나는 곳에서 받기를 끝낸다.
    config.allow_dvzip_archive_boundary = true;
    config.ask_timeout = ASK_TIMEOUT;
    // Apple ID가 없으면 아무도 모르는 계정: 누구에게나 이름만 알려 준다.
    config.account = identity.account().cloned().unwrap_or_default();
    config.household = if seat.household.is_empty() {
        vec![config.account.clone()]
    } else {
        seat.household.clone()
    };
    config.answers_strangers = seat.answers_strangers;
    config
}

/// 송신기 설정: 앱이 쓰는 그대로 (루프백 테스트도 이것으로 보낸다).
pub fn sender_config(profile: &Profile, account: Account) -> SenderConfig {
    let mut config = SenderConfig::new(profile.name.clone(), profile.service_id.clone());
    config.model_name = profile.model.clone();
    config.account = account;
    config
}

/// /Discover에 대답한 받는 기기.
#[derive(Debug, Clone)]
pub struct Target {
    /// 폰에 보이는 그 이름.
    pub name: String,
    pub model: Option<String>,
    /// Apple ID 확인 기록을 보냈습니다.
    pub verified: bool,
    pub service: Found,
    pub addr: SocketAddrV6,
    /// Discovery의 계정 가운데 무엇으로 보낼지: 그 기기가 가장 잘 아는 계정.
    pub via: usize,
}

#[derive(Debug, Clone)]
pub enum Discovered {
    Found(Target),
    /// 떠난 받는 기기의 mDNS 인스턴스 이름.
    Lost(String),
}

/// 받는 기기를 찾고 거기로 보냅니다. 살아 있는 동안 계속 듣고, 폰을 깨우는
/// 동안([Discovery::set_wake])에는 BLE 광고로 주변 iPhone의 AirDrop도 켜므로 전송이 끝날 때까지 켜
/// 두세요.
pub struct Discovery {
    events: mpsc::Receiver<Discovered>,
    senders: Arc<[Arc<Sender>]>,
    task: JoinHandle<()>,
    wake: Option<BleWake>,
    /// 로그인한 계정들. BLE 광고에 그 해시가 실립니다.
    signed_in: Vec<Account>,
    /// 모든 계정, 첫 계정이 먼저. 두드리기([Discovery::knock_peers])에 씁니다.
    accounts: Arc<[Account]>,
    interface: Interface,
    found: mpsc::Sender<Discovered>,
    /// BLE 광고가 켜져 있습니다: 그때만 AWDL 상대를 두드립니다.
    waking: watch::Sender<bool>,
    seen: Arc<Mutex<Seen>>,
    knock: Option<JoinHandle<()>>,
}

/// 연결을 끊거나 /Discover를 무시하는 받는 기기에 다시 물어보는 시간: 막 깨어난 폰은 처음 연결 몇
/// 개를 거부합니다.
const PROBE_FOR: Duration = Duration::from_secs(30);

/// 알리지 않은 Apple 받는 기기가 /Discover를 받는 곳 (owly DISC-5).
const APPLE_PORT: u16 = 8770;
/// 답하지 않은 AWDL 상대를 다시 두드리는 간격과, 한 번 두드리고 기다리는 시간.
const KNOCK_EVERY: Duration = Duration::from_millis(1500);
const KNOCK_WAIT: Duration = Duration::from_secs(25);
/// 두드려서 찾은 받는 기기가 아직 연결을 받는지 보는 간격, 한 번 기다리는 시간, 연달아 놓쳐도 되는
/// 횟수. AirDrop을 꺼도 mDNS 작별 인사를 보내지 않고 AWDL은 다른 일로 켜져 있을 수 있어, 이것
/// 말고는 목록에서 뺄 방법이 없습니다.
const CHECK_EVERY: Duration = Duration::from_secs(2);
const CHECK_WAIT: Duration = Duration::from_secs(4);
const CHECK_MISSES: u32 = 2;

/// 지금까지 찾은 받는 기기와 찾은 방법. 두 방법으로 찾은 기기도 한 번만 띄웁니다.
#[derive(Default)]
struct Seen {
    /// 확인 중이거나 띄운 mDNS 인스턴스와 그 주소.
    mdns: HashMap<String, Ipv6Addr>,
    /// 두드려서 띄운 주소와 그 인스턴스 이름.
    knocked: HashMap<Ipv6Addr, String>,
}

impl Seen {
    fn has(&self, addr: &Ipv6Addr) -> bool {
        self.knocked.contains_key(addr) || self.mdns.values().any(|a| a == addr)
    }
}

fn seen(s: &Mutex<Seen>) -> MutexGuard<'_, Seen> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

impl Discovery {
    /// 찾기를 시작합니다. wake면 BLE로 광고도 합니다. Apple ID 신원이면 그 인증서가 TLS
    /// 핸드셰이크에 들어가고, "연락처만" 모드의 폰은 우리를 띄우기 전에 확인 기록과 대조합니다.
    pub async fn start(interface: Interface, identity: &Identity, profile: &Profile, wake: bool) -> io::Result<Discovery> {
        Self::start_as(interface, &[(identity.clone(), profile.clone())], wake).await
    }

    /// [Discovery::start]와 같되, PC의 Apple ID마다 보내는 쪽을 둡니다 (그 신원과 profile, 첫
    /// 계정이 먼저). 찾은 받는 기기마다 계정을 차례로 바꿔 물어 그 계정의 기기인 것을 찾고, 없으면
    /// 기록을 받는 첫 계정(연락처), 그것도 없으면 이름을 받는 첫 계정으로 합니다. [Target::via]가
    /// 그 계정을 알려 주고, 거기로 보낼 때 그 계정으로 보냅니다.
    pub async fn start_as(interface: Interface, accounts: &[(Identity, Profile)], wake: bool) -> io::Result<Discovery> {
        crate::init_tls();
        if accounts.is_empty() {
            return Err(io::Error::other("no account to send as"));
        }
        let handles: Vec<Account> = accounts.iter().map(|(i, _)| i.account().cloned().unwrap_or_default()).collect();
        let signed_in: Vec<Account> = handles.iter().filter(|a| a.get().is_some()).cloned().collect();
        let mut senders = Vec::new();
        for ((identity, profile), account) in accounts.iter().zip(&handles) {
            let config = sender_config(profile, account.clone());
            senders.push(Arc::new(Sender::with_client_identity(
                config,
                identity.is_apple_id().then(|| identity.tls()),
            )?));
        }
        let senders: Arc<[Arc<Sender>]> = senders.into();
        let accounts: Arc<[Account]> = handles.into();
        let seen_so_far = Arc::new(Mutex::new(Seen::default()));
        let browse = Mdns::new(interface.index).browse().await?;
        let (tx, events) = mpsc::channel(32);
        let task = tokio::spawn(browse_loop(
            browse,
            senders.clone(),
            accounts.clone(),
            interface,
            seen_so_far.clone(),
            tx.clone(),
        ));
        let mut discovery = Discovery {
            events,
            senders,
            task,
            wake: None,
            signed_in,
            accounts,
            interface,
            found: tx,
            waking: watch::Sender::new(false),
            seen: seen_so_far,
            knock: None,
        };
        discovery.set_wake(wake).await;
        Ok(discovery)
    }

    /// mDNS에 안 보이는 상대 가운데서도 받는 기기를 찾습니다 (owly DISC-5). BLE 광고가 켜져 있는
    /// 동안 AWDL 상대마다 링크 로컬 주소로 /Discover를 보내고, 답할 때까지 [KNOCK_EVERY]마다 다시
    /// 보냅니다. 광고로 깨어난 폰은 _airdrop._tcp를 우리가 듣는 mDNS에 싣지 않을 수 있지만 (Apple
    /// 기기는 AWDL 자체 프레임에 싣기도 합니다) /Discover는 받습니다. peers는 링크의 것입니다
    /// ([crate::link::Link::peers]).
    pub fn knock_peers(&mut self, peers: watch::Receiver<Vec<PeerInfo>>) {
        if let Some(old) = self.knock.take() {
            old.abort();
        }
        let task = knock_loop(
            peers,
            self.waking.subscribe(),
            self.senders.clone(),
            self.accounts.clone(),
            self.interface,
            self.seen.clone(),
            self.found.clone(),
        );
        self.knock = Some(tokio::spawn(task));
    }

    /// 폰 깨우기를 켜거나 끕니다: 화면이 켜진 주변 iPhone이 AirDrop을 켜게 하는 BLE 광고. 찾은 받는
    /// 기기는 어느 쪽이든 떠날 때까지 목록에 남습니다.
    pub async fn set_wake(&mut self, on: bool) {
        if !on {
            self.wake = None;
            self.waking.send_replace(false);
            return;
        }
        if self.wake.is_some() {
            return;
        }
        let signed_in = self.signed_in.clone();
        self.wake = match tokio::task::spawn_blocking(move || BleWake::start_for(&signed_in)).await {
            Ok(Ok(w)) => Some(w),
            Ok(Err(e)) => {
                tracing::warn!("no BLE advert, so only awake phones will be found: {e}");
                None
            }
            Err(e) => {
                tracing::warn!("no BLE advert: {e}");
                None
            }
        };
        self.waking.send_replace(self.wake.is_some());
    }

    /// BLE 광고가 켜져 있습니다.
    pub fn waking(&self) -> bool {
        self.wake.is_some()
    }

    /// 다음으로 찾았거나 잃은 받는 기기. 찾기가 멈추면 None. 취소해도 안전해 select! 반복문에 둘 수
    /// 있습니다.
    pub async fn next(&mut self) -> Option<Discovered> {
        self.events.recv().await
    }

    /// target에게 payload를 받을지 묻고, 받으면 올립니다. 진행률은 압축된 바이트로 셉니다.
    pub async fn send(&self, target: &Target, payload: &Payload, progress: Option<mpsc::UnboundedSender<SendEvent>>) -> io::Result<Outcome> {
        self.sender().send(target, payload, progress).await
    }

    /// 이 Discovery가 계속 찾는 동안 보내는 손잡이. [Discovery::next]를 부르는 반복문 옆에서 전송을
    /// 따로 돌릴 수 있습니다. 끝날 때까지 Discovery를 살려 두세요: 그 BLE 광고가 폰을 깨어 있게
    /// 합니다.
    pub fn sender(&self) -> SendHandle {
        SendHandle {
            senders: self.senders.clone(),
        }
    }
}

/// [Discovery]를 거쳐 보냅니다. [Discovery::sender] 참고.
#[derive(Clone)]
pub struct SendHandle {
    senders: Arc<[Arc<Sender>]>,
}

impl SendHandle {
    /// [Discovery::send]와 같고, [Target::via]가 가리키는 계정으로 보냅니다.
    pub async fn send(&self, target: &Target, payload: &Payload, progress: Option<mpsc::UnboundedSender<SendEvent>>) -> io::Result<Outcome> {
        let sender = self.senders.get(target.via).unwrap_or(&self.senders[0]);
        sender.send(target.addr, payload, target.service.flags, progress).await
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(knock) = &self.knock {
            knock.abort();
        }
    }
}

/// mDNS가 찾는 받는 기기를 모두 확인합니다. 하나마다 자기 태스크에서, 기록이 바뀌면 다시
/// 확인합니다.
async fn browse_loop(
    mut browse: Browse,
    senders: Arc<[Arc<Sender>]>,
    accounts: Arc<[Account]>,
    interface: Interface,
    seen_so_far: Arc<Mutex<Seen>>,
    tx: mpsc::Sender<Discovered>,
) {
    let mut probes = HashMap::<String, tokio::task::AbortHandle>::new();
    while let Some(event) = browse.events.recv().await {
        match event {
            BrowseEvent::Lost(instance) => {
                if let Some(p) = probes.remove(&instance) {
                    p.abort();
                }
                if seen(&seen_so_far).mdns.remove(&instance).is_none() {
                    continue;
                }
                if tx.send(Discovered::Lost(instance)).await.is_err() {
                    break;
                }
            }
            BrowseEvent::Found(found) if found.addr != interface.addr => {
                {
                    let mut s = seen(&seen_so_far);
                    if let Some(listed) = s.knocked.get(&found.addr) {
                        tracing::info!("mDNS: {} at {}, listed already as {listed} by knocking", found.instance, found.addr);
                        continue;
                    }
                    s.mdns.insert(found.instance.clone(), found.addr);
                }
                if let Some(p) = probes.remove(&found.instance) {
                    p.abort();
                }
                let instance = found.instance.clone();
                let task = tokio::spawn(probe(senders.clone(), accounts.clone(), interface, found, tx.clone()));
                probes.insert(instance, task.abort_handle());
            }
            BrowseEvent::Found(_) => {}
        }
    }
    for p in probes.values() {
        p.abort();
    }
}

/// 계정을 차례로 바꿔 /Discover합니다 ([Discovery::start_as] 참고). [PROBE_FOR] 동안 간격을 늘려
/// 가며 다시 해 봅니다.
async fn probe(senders: Arc<[Arc<Sender>]>, accounts: Arc<[Account]>, interface: Interface, found: Found, tx: mpsc::Sender<Discovered>) {
    let addr = SocketAddrV6::new(found.addr, found.port, 0, interface.index);
    tracing::info!("mDNS: {} at {addr}, flags {:#x}", found.instance, found.flags);
    let deadline = tokio::time::Instant::now() + PROBE_FOR;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if let Ok((via, r)) = ask(&senders, &accounts, addr, attempt, true).await {
            let target = Target {
                name: r.computer_name,
                model: r.model_name,
                verified: r.record.is_some(),
                service: found,
                addr,
                via,
            };
            let _ = tx.send(Discovered::Found(target)).await;
            return;
        }
        let pause = Duration::from_secs(u64::from(attempt.min(3)));
        if tokio::time::Instant::now() + pause > deadline {
            return;
        }
        tokio::time::sleep(pause).await;
    }
}

/// addr에 계정을 차례로 바꿔 /Discover를 한 번씩, 그 계정의 기기인 것이 나올 때까지: 보낼 계정과
/// 대답, 아니면 실패한 까닭. 실패는 드러날 때만 로그에 남깁니다.
async fn ask(
    senders: &[Arc<Sender>],
    accounts: &[Account],
    addr: SocketAddrV6,
    attempt: u32,
    loud: bool,
) -> Result<(usize, owly_airdrop::proto::sender::Receiver), String> {
    // 지금까지 가장 좋은 대답: (우리를 얼마나 잘 아는지, 계정, 대답).
    let mut best: Option<(u8, usize, owly_airdrop::proto::sender::Receiver)> = None;
    let mut why = String::new();
    for (i, sender) in senders.iter().enumerate() {
        match sender.discover(addr).await {
            Ok(Some(r)) => {
                // 기록이 누구의 기기인지 알려 줍니다: 우리 계정의 것이면 그 계정으로 보냅니다.
                let theirs = r.record.as_deref().and_then(owly_airdrop::proto::apple_id::alt_dsid);
                let owner = theirs.and_then(|a| accounts.iter().position(|acc| acc.alt_dsid().as_deref() == Some(a.as_str())));
                let (rank, via) = match owner {
                    Some(k) => (2, k),
                    None if r.record.is_some() => (1, i),
                    None => (0, i),
                };
                if best.as_ref().is_none_or(|(b, _, _)| rank > *b) {
                    best = Some((rank, via, r));
                }
                if rank == 2 {
                    break;
                }
            }
            Ok(None) => {
                why = "/Discover answered without a name: it doesn't take us".into();
                if loud {
                    tracing::info!("/Discover {addr} as account {i}: no name (attempt {attempt})");
                }
            }
            Err(e) => {
                why = e.to_string();
                if loud {
                    tracing::info!("/Discover {addr} as account {i}: {e} (attempt {attempt})");
                }
            }
        }
    }
    let Some((rank, via, r)) = best else { return Err(why) };
    let how = ["by name", "as a contact", "as its own device"][usize::from(rank)];
    tracing::info!(
        "/Discover {addr}: {} knows us {how}, sending as account {via} (attempt {attempt})",
        r.computer_name
    );
    Ok((via, r))
}

/// 두드리기나 확인의 결과.
enum Done {
    Knock(MacAddr, Result<Target, String>),
    Check(MacAddr, io::Result<()>),
}

/// 두드려서 띄운 받는 기기.
struct Knocked {
    instance: String,
    addr: SocketAddrV6,
    checked: Instant,
    misses: u32,
}

/// BLE 광고가 켜져 있는 동안 mDNS에 안 보인 AWDL 상대를 두드리고 ([Discovery::knock_peers] 참고),
/// 띄운 것은 연결을 받지 않을 때까지 확인합니다. Discovery가 사라지면 끝납니다.
async fn knock_loop(
    mut peers: watch::Receiver<Vec<PeerInfo>>,
    mut waking: watch::Receiver<bool>,
    senders: Arc<[Arc<Sender>]>,
    accounts: Arc<[Account]>,
    interface: Interface,
    seen_so_far: Arc<Mutex<Seen>>,
    tx: mpsc::Sender<Discovered>,
) {
    let mut busy = HashSet::<MacAddr>::new();
    let mut last = HashMap::<MacAddr, (Instant, u32)>::new();
    let mut why = HashMap::<MacAddr, String>::new();
    let mut listed = HashMap::<MacAddr, Knocked>::new();
    let mut work = tokio::task::JoinSet::<Done>::new();
    let mut round = tokio::time::interval(KNOCK_EVERY);
    round.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = round.tick() => {}
            changed = waking.changed() => if changed.is_err() { return },
            changed = peers.changed() => if changed.is_err() { return },
            Some(done) = work.join_next() => {
                let Ok(done) = done else { continue };
                match done {
                    Done::Knock(mac, answer) => {
                        busy.remove(&mac);
                        let target = match answer {
                            Ok(target) => target,
                            Err(reason) => {
                                let tries = last.get(&mac).map_or(0, |(_, n)| *n);
                                // 새 까닭마다, 그리고 열 번째 시도마다: 오래 걸려도 계속 도는 것이
                                // 보이게.
                                if why.get(&mac) != Some(&reason) || tries.is_multiple_of(10) {
                                    tracing::info!("knock {tries} on {mac}: {reason}");
                                }
                                why.insert(mac, reason);
                                continue;
                            }
                        };
                        let fresh = {
                            let mut s = seen(&seen_so_far);
                            let fresh = !s.has(target.addr.ip());
                            if fresh {
                                s.knocked.insert(*target.addr.ip(), target.service.instance.clone());
                            }
                            fresh
                        };
                        if !fresh {
                            continue;
                        }
                        let instance = target.service.instance.clone();
                        listed.insert(mac, Knocked { instance, addr: target.addr, checked: Instant::now(), misses: 0 });
                        if tx.send(Discovered::Found(target)).await.is_err() {
                            return;
                        }
                    }
                    Done::Check(mac, result) => {
                        busy.remove(&mac);
                        let Some(k) = listed.get_mut(&mac) else { continue };
                        let gone = match &result {
                            Ok(()) => {
                                k.misses = 0;
                                None
                            }
                            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => Some(format!("its AirDrop went off ({e})")),
                            Err(e) => {
                                k.misses += 1;
                                (k.misses >= CHECK_MISSES).then(|| format!("{} checks in a row unanswered ({e})", k.misses))
                            }
                        };
                        let Some(reason) = gone else { continue };
                        let Some(k) = listed.remove(&mac) else { continue };
                        seen(&seen_so_far).knocked.remove(k.addr.ip());
                        last.remove(&mac);
                        why.remove(&mac);
                        tracing::info!("{mac} unlisted: {reason}");
                        if tx.send(Discovered::Lost(k.instance)).await.is_err() {
                            return;
                        }
                    }
                }
                continue;
            }
        }
        let now = Instant::now();
        // 띄운 것들은 깨우는 중이든 아니든: 아직 연결을 받는지?
        for (mac, k) in &mut listed {
            if busy.contains(mac) || now.duration_since(k.checked) < CHECK_EVERY {
                continue;
            }
            k.checked = now;
            busy.insert(*mac);
            let (mac, addr) = (*mac, k.addr);
            work.spawn(async move { Done::Check(mac, check(addr).await) });
        }
        if !*waking.borrow() {
            continue;
        }
        let present: Vec<PeerInfo> = peers.borrow_and_update().clone();
        for peer in present {
            let ip = peer.mac.link_local();
            if busy.contains(&peer.mac) || seen(&seen_so_far).has(&ip) {
                continue;
            }
            let (at, tries) = last.entry(peer.mac).or_insert((now - KNOCK_EVERY, 0));
            if now.duration_since(*at) < KNOCK_EVERY {
                continue;
            }
            *at = now;
            *tries += 1;
            if *tries == 1 {
                tracing::info!(
                    "knocking on AWDL peer {} ({}, class {}, RSSI {:?}): /Discover at its link-local address",
                    peer.mac,
                    peer.name,
                    peer.devclass,
                    peer.rssi
                );
            }
            busy.insert(peer.mac);
            let (senders, accounts, mac, attempt) = (senders.clone(), accounts.clone(), peer.mac, *tries);
            work.spawn(async move {
                let (mac, answer) = knock(senders, accounts, interface, mac, attempt).await;
                Done::Knock(mac, answer)
            });
        }
    }
}

/// addr의 받는 기기가 아직 연결을 받는지: TCP 연결만 맺고 바로 닫습니다. 거부되면 그 AirDrop 서버가
/// 꺼진 것입니다.
async fn check(addr: SocketAddrV6) -> io::Result<()> {
    match tokio::time::timeout(CHECK_WAIT, tokio::net::TcpStream::connect(addr)).await {
        Ok(stream) => stream.map(drop),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "no TCP answer")),
    }
}

/// mac의 상대를 한 번 두드립니다: 대답하면 그 받는 기기.
async fn knock(
    senders: Arc<[Arc<Sender>]>,
    accounts: Arc<[Account]>,
    interface: Interface,
    mac: MacAddr,
    attempt: u32,
) -> (MacAddr, Result<Target, String>) {
    let addr = SocketAddrV6::new(mac.link_local(), APPLE_PORT, 0, interface.index);
    // 실패한 까닭은 반복문으로 돌려보내고, 반복문이 바뀔 때만 로그에 남깁니다: 거부(AirDrop 서버
    // 없음), TCP나 TLS 무응답(닿지 않음), 이름 없음(우리를 받지 않음).
    let answer = tokio::time::timeout(KNOCK_WAIT, ask(&senders, &accounts, addr, attempt, false))
        .await
        .unwrap_or_else(|_| Err(format!("no answer within {}s", KNOCK_WAIT.as_secs())));
    let target = answer.map(|(via, r)| {
        tracing::info!("AWDL peer {mac} is {}, found by knocking (attempt {attempt})", r.computer_name);
        Target {
            name: r.computer_name,
            model: r.model_name,
            verified: r.record.is_some(),
            service: Found {
                instance: format!("awdl-{}", mac.to_string().replace(':', "")),
                host: String::new(),
                port: APPLE_PORT,
                addr: *addr.ip(),
                // iPhone들의 알림이 주는 대로: gzip으로 올립니다.
                flags: 0,
            },
            addr,
            via,
        }
    });
    (mac, target)
}
