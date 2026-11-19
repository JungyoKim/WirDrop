//! awdl0, AWDL 링크의 호스트 쪽 끝: Windows 소켓과 owly 노드 사이로 IPv6를 나르는 Wintun 어댑터
//! (서명된 드라이버라 Secure Boot를 켠 채로 씁니다).
//!
//! AWDL 상대들은 AWDL MAC으로 만든 수정 EUI-64 링크 로컬 주소로 서로를 부르고, owly는 이 주소로
//! 패킷을 프레임에 대응시킵니다. 그래서 awdl0에는 우리 주소 하나만 두고, Windows가 붙이는 무작위
//! 링크 로컬 주소는 막습니다. 어댑터를 만들려면 관리자 권한이 필요합니다.
use std::collections::VecDeque;
use std::io;
use std::net::Ipv6Addr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use owly_awdl::driver::Netif;
use owly_awdl::{Ipv6Packet, MacAddr};
use tokio::sync::{mpsc, watch};
use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, ERROR_OBJECT_ALREADY_EXISTS, NO_ERROR, WIN32_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CreateUnicastIpAddressEntry, DeleteUnicastIpAddressEntry, FreeMibTable, GetIpInterfaceEntry, GetUnicastIpAddressTable,
    InitializeIpInterfaceEntry, InitializeUnicastIpAddressEntry, MIB_IPINTERFACE_ROW, MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
    SetIpInterfaceEntry,
};
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::Networking::WinSock::{AF_INET6, IpDadStatePreferred, LinkLocalAlwaysOff, RouterDiscoveryDisabled};

/// 우리 어댑터의 GUID. 비정상 종료한 실행이 남긴 어댑터를 다시 씁니다.
const GUID: u128 = u128::from_be_bytes(*b"WirDrop awdl0 v1");
const RING_CAPACITY: u32 = 0x40_0000;
/// 모든 속도 측정에 쓴 IPv6 MTU.
pub const MTU: u32 = 1450;
/// 사라졌던 우리 주소가 되돌린 뒤 이만큼 그대로 있어야 IPv6가 돌아왔다고 봅니다. VPN이 켜져 있는
/// 동안에는 2초마다 다시 지우므로, 그동안은 소켓을 다시 열지 않습니다.
const SETTLE: Duration = Duration::from_secs(10);

/// 이 프로세스가 도는 동안 다음 링크를 위해 남겨 두는 어댑터. 새로 만들면 Windows가 1~2초 걸리고,
/// 어댑터는 프로세스와 함께 사라지므로 (Wintun이 자기가 만든 것을 지웁니다) 다시 시작한 링크(USB
/// 어댑터를 다시 꽂았을 때)는 awdl0을 바로 얻습니다.
static KEPT: std::sync::Mutex<Option<Arc<wintun::Adapter>>> = std::sync::Mutex::new(None);

/// 버리면 [KEPT]로 돌아가는 어댑터. 세션 뒤의 필드입니다: 필드는 순서대로 버려지므로 그때는 그 위의
/// 세션이 끝나 있습니다.
struct Keep(Option<Arc<wintun::Adapter>>);

impl Drop for Keep {
    fn drop(&mut self) {
        if let Some(adapter) = self.0.take() {
            *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(adapter);
        }
    }
}

pub struct WintunNetif {
    session: Arc<wintun::Session>,
    _adapter: Keep,
    rx: mpsc::Receiver<Vec<Ipv6Packet>>,
    /// 읽는 스레드가 한꺼번에 넘긴 것 가운데 남은 패킷.
    queue: VecDeque<Ipv6Packet>,
    reader: Option<JoinHandle<()>>,
    keeper: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    skipped: Arc<AtomicU64>,
    dropped: u64,
    index: u32,
    addr: Ipv6Addr,
    resets: watch::Receiver<u64>,
}

impl WintunNetif {
    /// dll의 wintun.dll로 이름이 name인 어댑터를 mac의 링크 로컬 주소로 만듭니다.
    pub fn open(dll: &Path, name: &str, mac: MacAddr, mtu: u32) -> io::Result<WintunNetif> {
        let kept = KEPT.lock().unwrap_or_else(|e| e.into_inner()).take();
        let reused = kept.and_then(|adapter| match adapter.start_session(RING_CAPACITY) {
            Ok(session) => {
                tracing::info!("{name}: the adapter from the last link");
                Some((adapter, session))
            }
            Err(e) => {
                tracing::warn!("{name}: the last link's adapter won't start ({e}), so a new one");
                None
            }
        });
        let (adapter, session) = match reused {
            Some(s) => s,
            None => {
                // SAFETY: wintun.dll은 부르는 쪽이 함께 배포하는 WireGuard의 서명된 빌드입니다.
                let wintun = unsafe { wintun::load_from_path(dll) }
                    .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("loading {}: {e}", dll.display())))?;
                let adapter = wintun::Adapter::create(&wintun, name, "WirDrop", Some(GUID)).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("creating the Wintun adapter {name} (administrator rights needed): {e}"),
                    )
                })?;
                let session = adapter.start_session(RING_CAPACITY).map_err(other)?;
                (adapter, session)
            }
        };
        let session = Arc::new(session);
        // SAFETY: Value가 공용체 전체를 덮습니다.
        let luid = unsafe { adapter.get_luid().Value };
        let index = adapter.get_adapter_index().map_err(other)?;
        let addr = mac.link_local();
        configure(luid, mtu, Duration::from_secs(5)).map_err(|e| context(e, "configuring IPv6 on the adapter"))?;
        add_address(luid, addr).map_err(|e| context(e, &format!("adding {addr}")))?;
        tidy(luid, addr).map_err(|e| context(e, "checking the adapter's addresses"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (resets_tx, resets) = watch::channel(0);
        let skipped = Arc::new(AtomicU64::new(0));
        // 묶음 16개: 많아야 패킷 1024개가 노드 밖에서 기다리고, 나머지는 Wintun 링에서.
        let (tx, rx) = mpsc::channel(16);
        let (s, sk) = (session.clone(), skipped.clone());
        let reader = std::thread::Builder::new().name("awdl0-rx".into()).spawn(move || read_loop(s, tx, sk))?;
        let st = stop.clone();
        let keeper = std::thread::Builder::new()
            .name("awdl0-addr".into())
            .spawn(move || keep_address(luid, addr, mtu, st, resets_tx))?;
        tracing::info!("{name}: interface {index}, {addr}, MTU {mtu}");
        Ok(WintunNetif {
            session,
            _adapter: Keep(Some(adapter)),
            rx,
            queue: VecDeque::new(),
            reader: Some(reader),
            keeper: Some(keeper),
            stop,
            skipped,
            dropped: 0,
            index,
            addr,
            resets,
        })
    }

    /// 인터페이스 번호. awdl0 링크 로컬 주소의 scope ID입니다.
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn addr(&self) -> Ipv6Addr {
        self.addr
    }

    /// awdl0의 IPv6가 리셋되었다가 돌아온 횟수. 바뀔 때마다 awdl0에서 듣는 소켓을 새로 열어야
    /// 합니다 ([keep_address] 참고).
    pub fn resets(&self) -> watch::Receiver<u64> {
        self.resets.clone()
    }
}

impl Netif for WintunNetif {
    async fn recv(&mut self) -> io::Result<Ipv6Packet> {
        if let Some(p) = self.queue.pop_front() {
            return Ok(p);
        }
        // 기다린 뒤 돌려주기까지 다른 await가 없어, select!가 버려도 패킷을 잃지 않습니다.
        let batch = self.rx.recv().await.ok_or_else(|| io::Error::other("awdl0 closed"))?;
        self.queue.extend(batch);
        self.queue.pop_front().ok_or_else(|| io::Error::other("an empty batch"))
    }

    fn try_recv(&mut self) -> Option<Ipv6Packet> {
        if self.queue.is_empty() {
            self.queue.extend(self.rx.try_recv().ok()?);
        }
        self.queue.pop_front()
    }

    fn deliver(&mut self, packet: Ipv6Packet) {
        let n = packet.data.len();
        if n == 0 || n > usize::from(u16::MAX) {
            return;
        }
        match self.session.allocate_send_packet(n as u16) {
            Ok(mut out) => {
                out.bytes_mut().copy_from_slice(&packet.data);
                self.session.send_packet(out);
            }
            // 링이 가득 찼습니다: 가득 찬 큐처럼 버립니다.
            Err(_) => self.dropped += 1,
        }
    }
}

impl Drop for WintunNetif {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        let _ = self.session.shutdown();
        self.rx.close();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = self.keeper.take() {
            let _ = t.join();
        }
        tracing::info!(
            "awdl0 closed ({} packets to the host dropped, {} from it not AWDL-addressable)",
            self.dropped,
            self.skipped.load(Relaxed)
        );
    }
}

/// 호스트의 IPv6 패킷을 노드에 넘깁니다. 깨어날 때마다 링에 와 있는 것을 64개까지 한 묶음으로 넘겨
/// 노드를 한 번만 깨웁니다. 주소를 붙일 수 없는 것(IPv4, 또는 EUI-64 링크 로컬이 아닌 주소에서 온 것)은
/// 건너뜁니다. 노드가 밀리면 막히고, 그동안 패킷은 Wintun 링에서 기다립니다.
fn read_loop(session: Arc<wintun::Session>, tx: mpsc::Sender<Vec<Ipv6Packet>>, skipped: Arc<AtomicU64>) {
    while let Ok(first) = session.receive_blocking() {
        let mut batch = Vec::with_capacity(16);
        let mut next = Some(first);
        while let Some(packet) = next {
            let bytes = packet.bytes();
            let ip = if bytes.first().is_some_and(|b| b >> 4 == 6) {
                Ipv6Packet::from_ip(bytes.to_vec())
            } else {
                None
            };
            drop(packet);
            match ip {
                Some(p) => batch.push(p),
                None => {
                    skipped.fetch_add(1, Relaxed);
                }
            }
            if batch.len() >= 64 {
                break;
            }
            next = session.try_receive().ok().flatten();
        }
        if !batch.is_empty() && tx.blocking_send(batch).is_err() {
            return;
        }
    }
}

/// Windows가 주소를 바꾸면 우리 주소를 되돌리고 나머지를 지웁니다. 처음엔 자주, 그다음엔 2초마다.
///
/// 우리 주소가 사라졌다면 무언가 이 인터페이스의 IPv6를 리셋한 것입니다. Proton VPN은 켜져 있는
/// 동안 다른 어댑터의 IPv6를 끕니다. 그러면 awdl0에서 듣던 소켓은 열린 채로 남아도 mDNS 그룹에서
/// 빠져, iPhone이 이 PC를 찾지 못합니다. 그래서 되돌린 주소가 [SETTLE] 동안 그대로 있으면 resets를
/// 하나 올려, 링크를 쓰는 쪽이 소켓을 새로 열게 합니다. 리셋된 인터페이스는 Windows의 기본값으로
/// 돌아오므로 우리 설정도 다시 겁니다.
fn keep_address(luid: u64, addr: Ipv6Addr, mtu: u32, stop: Arc<AtomicBool>, resets: watch::Sender<u64>) {
    let started = Instant::now();
    // 우리 주소가 사라진 때부터: 마지막으로 다시 붙였거나 붙이지 못한 때, 그리고 그동안의 횟수.
    let mut lost: Option<(Instant, Instant, u32)> = None;
    while !stop.load(Relaxed) {
        match tidy(luid, addr) {
            Ok(false) => {
                if let Some((since, last, tries)) = lost
                    && last.elapsed() >= SETTLE
                {
                    lost = None;
                    tracing::info!(
                        "awdl0: IPv6 is back after {}s gone ({tries} times put back), so what listens on awdl0 starts again",
                        last.duration_since(since).as_secs()
                    );
                    resets.send_modify(|n| *n += 1);
                }
            }
            put_back => {
                let now = Instant::now();
                match &mut lost {
                    Some((_, last, tries)) => {
                        *last = now;
                        *tries += 1;
                    }
                    None => {
                        tracing::warn!("awdl0: our address {addr} is gone (something reset IPv6 here, a VPN perhaps); putting it back");
                        lost = Some((now, now, 1));
                    }
                }
                match put_back {
                    Ok(_) => {
                        if let Err(e) = configure(luid, mtu, Duration::ZERO) {
                            tracing::debug!("awdl0: setting IPv6 up again failed: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("awdl0 address check failed: {e}"),
                }
            }
        }
        let pause = if started.elapsed() < Duration::from_secs(10) { 2 } else { 20 };
        for _ in 0..pause {
            if stop.load(Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn context(e: io::Error, what: &str) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

fn check(e: WIN32_ERROR) -> io::Result<()> {
    if e == NO_ERROR {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e as i32))
    }
}

/// awdl0의 IPv6: 우리 MTU, DAD 없음, 라우터 탐색 없음, Windows가 받아 주면 스스로 만드는 링크 로컬
/// 주소 없음 (아니면 [tidy]가 지웁니다). 스택에 인터페이스가 아직 없으면 wait 동안 기다립니다.
fn configure(luid: u64, mtu: u32, wait: Duration) -> io::Result<()> {
    match set_interface(luid, mtu, true, wait) {
        Err(e) if e.raw_os_error() == Some(87) => {
            tracing::debug!("awdl0: Windows won't turn its link-local address off ({e})");
            set_interface(luid, mtu, false, wait)
        }
        r => r,
    }
}

fn set_interface(luid: u64, mtu: u32, no_link_local: bool, wait: Duration) -> io::Result<()> {
    // 어댑터가 나타나고 잠깐은 스택에 인터페이스가 없을 수 있습니다.
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: API가 요구하는 대로 초기화한 평범한 C 구조체.
        let result = unsafe {
            let mut row: MIB_IPINTERFACE_ROW = std::mem::zeroed();
            InitializeIpInterfaceEntry(&mut row);
            row.Family = AF_INET6;
            row.InterfaceLuid = NET_LUID_LH { Value: luid };
            match GetIpInterfaceEntry(&mut row) {
                NO_ERROR => {
                    row.NlMtu = mtu;
                    row.DadTransmits = 0;
                    row.RouterDiscoveryBehavior = RouterDiscoveryDisabled;
                    if no_link_local {
                        row.LinkLocalAddressBehavior = LinkLocalAlwaysOff;
                    }
                    check(SetIpInterfaceEntry(&mut row))
                }
                e => Err(io::Error::from_raw_os_error(e as i32)),
            }
        };
        match result {
            Err(e) if e.raw_os_error() == Some(ERROR_NOT_FOUND as i32) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            r => return r,
        }
    }
}

fn add_address(luid: u64, addr: Ipv6Addr) -> io::Result<()> {
    // SAFETY: API가 요구하는 대로 초기화한 평범한 C 구조체.
    let e = unsafe {
        let mut row: MIB_UNICASTIPADDRESS_ROW = std::mem::zeroed();
        InitializeUnicastIpAddressEntry(&mut row);
        row.InterfaceLuid = NET_LUID_LH { Value: luid };
        row.Address.Ipv6.sin6_family = AF_INET6;
        row.Address.Ipv6.sin6_addr.u.Byte = addr.octets();
        row.OnLinkPrefixLength = 64;
        row.DadState = IpDadStatePreferred;
        CreateUnicastIpAddressEntry(&row)
    };
    if e == ERROR_OBJECT_ALREADY_EXISTS { Ok(()) } else { check(e) }
}

/// 인터페이스에서 우리 것 말고 모든 IPv6 주소를 지우고, 우리 것이 없으면 더합니다. 우리 것이 없어
/// 더했으면 true.
fn tidy(luid: u64, addr: Ipv6Addr) -> io::Result<bool> {
    let mut present = false;
    // SAFETY: API가 준 표를 범위 안에서 읽고 한 번 해제합니다.
    unsafe {
        let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
        let e = GetUnicastIpAddressTable(AF_INET6, &mut table);
        if e != NO_ERROR && e != ERROR_NOT_FOUND {
            return check(e).map(|()| false);
        }
        if !table.is_null() {
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
            for row in rows {
                if row.InterfaceLuid.Value != luid {
                    continue;
                }
                let a = Ipv6Addr::from(row.Address.Ipv6.sin6_addr.u.Byte);
                if a == addr {
                    present = true;
                } else {
                    tracing::info!("awdl0: removing {a}");
                    DeleteUnicastIpAddressEntry(row);
                }
            }
            FreeMibTable(table as *const _);
        }
    }
    if !present {
        add_address(luid, addr)?;
    }
    Ok(!present)
}
