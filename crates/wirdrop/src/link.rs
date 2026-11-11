//! AWDL 링크: 무선, 자기 스레드에서 도는 owly 노드, 그리고 awdl0.
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;

use owly_awdl::driver::{self, Running};
use owly_awdl::{MacAddr, PeerInfo};
use tokio::sync::watch;

use crate::air::UdpAir;
use crate::config::{Tuning, awdl_config, same_80mhz};
use crate::netif::{self, WintunNetif};
use crate::usbfwd::{Usbfwd, UsbfwdOptions};

#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// 상대에게 보이는 AWDL 호스트 이름.
    pub name: String,
    /// 우리 AWDL 주소: 유니캐스트, 로컬 관리 주소, 실행이 바뀌어도 유지
    /// ([crate::identity::State::awdl_mac] 참고).
    pub mac: [u8; 6],
    /// AWDL 채널. 149는 Apple 기기들이 쓰는 5 GHz 소셜 채널입니다.
    pub channel: u8,
    /// WinUSB에 묶인 Realtek USB 어댑터를 맡는 usbfwd 자식 프로세스.
    pub usbfwd: UsbfwdOptions,
    pub tuning: Tuning,
    /// wintun.dll (wintun.net의 서명된 빌드).
    pub wintun_dll: PathBuf,
    pub interface_name: String,
    pub mtu: u32,
}

impl LinkConfig {
    /// usbfwd를 거치는 USB 어댑터, 채널 149, 재서 가장 좋았던 설정.
    pub fn usb(name: impl Into<String>, mac: [u8; 6], usbfwd_exe: impl Into<PathBuf>, wintun_dll: impl Into<PathBuf>) -> Self {
        LinkConfig {
            name: name.into(),
            mac,
            channel: 149,
            usbfwd: UsbfwdOptions::new(usbfwd_exe),
            tuning: Tuning::usb_adapter(),
            wintun_dll: wintun_dll.into(),
            interface_name: "awdl0".into(),
            mtu: netif::MTU,
        }
    }
}

/// 소켓에서 보는 awdl0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interface {
    pub index: u32,
    pub addr: Ipv6Addr,
}

impl Interface {
    pub fn socket_addr(&self, port: u16) -> SocketAddrV6 {
        SocketAddrV6::new(self.addr, port, 0, self.index)
    }
}

/// 돌고 있는 링크. 버리면 awdl0을 내리고 어댑터를 놓아 줍니다.
pub struct Link {
    node: Option<Running>,
    usbfwd: Option<Usbfwd>,
    peers: watch::Receiver<Vec<PeerInfo>>,
    failed: watch::Receiver<Option<String>>,
    interface: Interface,
    _timer: TimerPeriod,
}

impl Link {
    /// 링크를 올립니다. awdl0을 만들고 USB 어댑터를 올리는 동안(나란히, 몇 초) 막히므로 블로킹
    /// 스레드(tokio::task::spawn_blocking)에서 부르세요. awdl0 때문에 관리자 권한이 필요합니다.
    pub fn start(cfg: LinkConfig) -> io::Result<Link> {
        let timer = TimerPeriod::new();
        let mut awdl = awdl_config(cfg.mac, &cfg.name, cfg.channel, &cfg.tuning)?;
        if cfg.tuning.follow_channels && cfg.usbfwd.bandwidth_mhz == 80 {
            awdl.follow = same_80mhz(cfg.channel);
        }
        let mut usb = cfg.usbfwd.clone();
        if cfg.tuning.tx_ampdu {
            usb.env.push(("USBFWD_TX_BA".into(), "1".into()));
        }
        if cfg.tuning.beamformee {
            usb.env.push(("USBFWD_BFEE".into(), "1".into()));
        }
        // awdl0(Wintun 어댑터)을 만드는 일과 USB 어댑터를 올리는 일은 각각 몇 초씩 걸리고 서로를 기다리지
        // 않으므로 나란히 합니다. awdl0이 실패하면 띄운 usbfwd를 버리고, 그러면 usbfwd도 멈춥니다.
        let (netif, started) = std::thread::scope(|s| {
            let usbfwd = s.spawn(|| Usbfwd::start(&usb, cfg.mac, cfg.channel));
            let netif = WintunNetif::open(&cfg.wintun_dll, &cfg.interface_name, MacAddr(cfg.mac), cfg.mtu);
            let started = usbfwd.join().unwrap_or_else(|_| Err(io::Error::other("usbfwd's start panicked")));
            (netif, started)
        });
        let netif = netif?;
        let usbfwd = started?;
        let interface = Interface {
            index: netif.index(),
            addr: netif.addr(),
        };
        let air = UdpAir::open(
            SocketAddr::from(([127, 0, 0, 1], usb.host_port)),
            SocketAddr::from(([127, 0, 0, 1], usb.radio_port)),
        )?;
        let (peers_tx, peers) = watch::channel(Vec::new());
        let node = driver::spawn(
            awdl,
            move || {
                // AWDL 노드는 받은 세그먼트를 모두 호스트 패킷으로, TCP ACK를 모두 프레임으로
                // 바꿉니다. 평범한 일보다 먼저 하게 해, 바쁜 PC에서도 보내는 쪽 창이 기다리지 않게
                // 합니다.
                // SAFETY: 부르는 (드라이버) 스레드 자신의 우선순위.
                unsafe {
                    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
                }
                Ok((air, netif))
            },
            peers_tx,
        )?;
        let failed = node.failed();
        tracing::info!(
            "AWDL up: {} on channel {}, address {}, {}%{}",
            cfg.name,
            cfg.channel,
            MacAddr(cfg.mac),
            interface.addr,
            interface.index
        );
        Ok(Link {
            node: Some(node),
            usbfwd: Some(usbfwd),
            peers,
            failed,
            interface,
            _timer: timer,
        })
    }

    pub fn interface(&self) -> Interface {
        self.interface
    }

    /// 유효한 AWDL 상대들. 오고 갈 때마다 바뀝니다.
    pub fn peers(&self) -> watch::Receiver<Vec<PeerInfo>> {
        self.peers.clone()
    }

    /// 링크가 스스로 멈췄다면 그 까닭: 노드가 실패했거나 usbfwd가 끝났습니다.
    pub fn check(&mut self) -> Option<String> {
        if let Some(why) = self.failed.borrow().clone() {
            return Some(why);
        }
        if let Some(u) = &mut self.usbfwd
            && !u.is_running()
        {
            return Some("usbfwd exited".into());
        }
        None
    }

    /// 노드를 멈추고 awdl0을 내리고 어댑터를 놓아 줍니다. 몇 초까지 막힙니다.
    pub fn stop(self) {}
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(node) = self.node.take() {
            node.stop();
        }
        self.usbfwd.take();
        tracing::info!("AWDL down");
    }
}

/// 링크가 도는 동안 타이머 해상도 1 ms: AWDL 슬롯은 16 ms이고 owly의 타이머가 제때 울려야 합니다.
struct TimerPeriod(bool);

impl TimerPeriod {
    fn new() -> Self {
        // SAFETY: 짝을 Drop에서 부르는 winmm 호출.
        TimerPeriod(unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) } == 0)
    }
}

impl Drop for TimerPeriod {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: new의 호출과 짝입니다.
            unsafe { windows_sys::Win32::Media::timeEndPeriod(1) };
        }
    }
}
