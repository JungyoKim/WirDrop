//! Windows 앱을 위한 AirDrop입니다.
//!
//! 아래에서부터 쌓인 부품:
//!
//! - [usbfwd]: WinUSB에 묶인 Realtek USB 어댑터(TP-Link Archer T3U Nano, RTL8812BU)를 사용자 모드의
//!   usbfwd 자식 프로세스가 구동합니다. ACK와 Block Ack은 하드웨어가 보내고, 프레임은 루프백 UDP로
//!   오갑니다 ([air]).
//! - [link]: owly-awdl의 AWDL 노드가 자기 스레드에서 그 무선과 [netif](Wintun 어댑터 awdl0) 사이를
//!   잇습니다. 우리 커널 드라이버가 없어 Secure Boot를 켠 채로 쓰고, Wintun에는 관리자 권한이
//!   필요합니다.
//! - [airdrop]: awdl0 위의 owly-airdrop 받는 쪽과 보내는 쪽. TLS 신원(있으면 Apple ID 신원)은
//!   [identity]가, 보낼 때 폰 깨우기는 [ble]가 맡습니다.
//!
//! 받는 앱의 예:
//!
//! ~~~no_run
//! # async fn run() -> std::io::Result<()> {
//! use wirdrop::{airdrop, Identity, Link, LinkConfig, Profile, State};
//! let state = State::open(State::default_dir().unwrap())?;
//! let identity = Identity::load(wirdrop::default_apple_id_dir().as_deref(), state.dir())?;
//! let config = LinkConfig::usb("My PC", state.awdl_mac()?, r"C:\app\usbfwd.exe", r"C:\app\wintun.dll");
//! let link = tokio::task::spawn_blocking(move || Link::start(config)).await??;
//! let _firewall = wirdrop::FirewallRules::allow_current_exe(airdrop::PORT)?;
//! let profile = Profile::new("My PC", state.service_id()?);
//! let (_receiving, mut events) = airdrop::receive(link.interface(), &identity, &profile, r"C:\Downloads").await?;
//! while let Some(event) = events.recv().await {
//!     if let airdrop::ReceiverEvent::Request(request) = event {
//!         request.accept();
//!     }
//! }
//! # Ok(()) }
//! ~~~
pub mod air;
pub mod airdrop;
pub mod ble;
pub mod config;
pub mod firewall;
pub mod identity;
pub mod link;
pub mod netif;
pub mod usbfwd;

pub use airdrop::{Discovered, Discovery, Profile, Receiving, Seat, SendHandle, Target};
pub use config::{Tuning, VhtMaxMpdu};
pub use firewall::FirewallRules;
pub use identity::{Account, Identity, State, default_apple_id_dir};
pub use link::{Interface, Link, LinkConfig};
pub use usbfwd::UsbfwdOptions;

pub use owly_airdrop;
pub use owly_awdl;
pub use owly_awdl::PeerInfo;

/// 앱이 설치하지 않았다면 ring을 rustls의 암호 공급자로 설치합니다.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
