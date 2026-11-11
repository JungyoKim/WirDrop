//! owly의 [Air]를 UDP로: 중계 프로세스(usbfwd)가 USB 어댑터에서 AWDL 프레임을 잡아 넘기고, 우리
//! 프레임을 내보냅니다.
//!
//! 데이터그램은 양쪽 모두 4바이트 머리로 시작합니다.
//! - 무선 → 호스트: 1, RSSI (i8, 모르면 -128), 프레임 길이 (u16 LE), 그다음 MAC 헤더부터의 802.11
//!   프레임 (FCS 없이).
//! - 호스트 → 무선: 1, 속도 (500 kbit/s 단위, 또는 0x80 | HT MCS), ACK 없음 표시, 0, 그다음 프레임.
//!
//! 0 네 바이트짜리 데이터그램은 호스트가 어디서 듣는지 중계기에 알립니다.
//!
//! 무선 → 호스트 데이터그램에는 중계기가 USB 전송 하나에서 꺼낸 프레임들이 잇달아 들고, 호스트 → 무선은
//! 2, 속도, ACK 없음 표시, 0, 프레임 길이 (u16 LE), 프레임을 잇달아 담습니다: 깨어날 때마다 쌓인 것을 한
//! 데이터그램으로 (받기 1.26 GB에 프레임마다 시스템 호출과 스레드 깨우기가 양쪽에 수십만 번이었습니다).
//!
//! 호스트 → 무선 3, 채널, 0, 0은 무선을 같은 80 MHz 안의 그 주 채널로 옮깁니다 (그 전에 보낸 프레임 뒤에).
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::windows::io::AsRawSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use owly_awdl::driver::{Air, Received};
use owly_awdl::{Channel, Frame, TxRate};
use tokio::sync::mpsc;

/// 루프백 소켓의 받기·보내기 버퍼 (usbfwd 쪽과 같은 크기). Windows 기본값(64 KB)에는 iPhone이
/// 묶어 보내는 A-MSDU(약 5.5 KB)가 열 개 남짓만 들어가서, 받는 스레드가 1 ms만 밀려도 넘친
/// 프레임이 아무 기록 없이 버려지고 iPhone은 그 구멍마다 TCP를 다시 보내며 속도를 줄입니다.
const SOCK_BUF: i32 = 8 << 20;
/// 호스트 → 무선 데이터그램 하나에 담는 바이트 (UDP 한도 65507 아래).
const OUT_MAX: usize = 60_000;

pub struct UdpAir {
    rx: mpsc::Receiver<Vec<Received>>,
    /// 받는 스레드가 한꺼번에 넘긴 것 가운데 남은 프레임.
    queue: VecDeque<Received>,
    /// 다음 flush까지 모으는 호스트 → 무선 프레임들.
    out: Vec<u8>,
    sock: UdpSocket,
    peer: SocketAddr,
    stop: Arc<AtomicBool>,
    got: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl UdpAir {
    /// local에서 peer의 중계기를 기다립니다.
    pub fn open(local: SocketAddr, peer: SocketAddr) -> io::Result<UdpAir> {
        let sock = UdpSocket::bind(local)?;
        let (before, after) = tune(&sock);
        tracing::info!("UDP radio: socket buffer {} KB -> {} KB", before / 1024, after / 1024);
        sock.send_to(&[0, 0, 0, 0], peer)?;
        let (rx_in, rx) = mpsc::channel::<Vec<Received>>(1024);
        let stop = Arc::new(AtomicBool::new(false));
        let got = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let (reader, stopped, n_got, n_dropped) = (sock.try_clone()?, stop.clone(), got.clone(), dropped.clone());
        std::thread::Builder::new().name("udp-air-rx".into()).spawn(move || {
            let mut buf = vec![0u8; 65536];
            while !stopped.load(Relaxed) {
                // 멈추라는 표시를 보려고 200 ms마다 깨어납니다 (받기 시간 제한은 데이터그램을 삼킵니다: [readable]).
                if !readable(&reader, 200) {
                    continue;
                }
                // 와 있는 데이터그램을 모두 한 묶음으로: 노드를 한 번만 깨웁니다.
                let mut batch = Vec::new();
                while let Ok((n, _)) = reader.recv_from(&mut buf) {
                    let at = Instant::now();
                    let mut off = 0;
                    while off + 4 <= n && buf[off] == 1 {
                        let len = u16::from_le_bytes([buf[off + 2], buf[off + 3]]) as usize;
                        if off + 4 + len > n {
                            break;
                        }
                        let rssi = buf[off + 1] as i8;
                        batch.push(Received {
                            bytes: buf[off + 4..off + 4 + len].to_vec(),
                            rssi: (rssi != -128).then_some(rssi),
                            received_at: Some(at),
                        });
                        off += 4 + len;
                    }
                    if batch.len() >= 256 || !pending(&reader) {
                        break;
                    }
                }
                if batch.is_empty() {
                    continue;
                }
                let n = batch.len() as u64;
                n_got.fetch_add(n, Relaxed);
                // 받는 쪽이 밀리면 버립니다: AWDL 노드가 늦게 받는 것보다 낫습니다.
                if rx_in.try_send(batch).is_err() {
                    n_dropped.fetch_add(n, Relaxed);
                }
            }
        })?;
        // 소켓은 막히는 모드로 둡니다: try_clone은 같은 소켓을 가리켜 non-blocking 설정도 함께
        // 바뀌므로, 켜면 위 스레드의 recv_from이 기다리지 않고 헛돌며 CPU 코어 하나를 다 씁니다.
        // 루프백 UDP 보내기는 막히지 않습니다 (받는 쪽이 가득 차면 거기서 버립니다).
        Ok(UdpAir {
            rx,
            queue: VecDeque::new(),
            out: Vec::with_capacity(OUT_MAX + 4096),
            sock,
            peer,
            stop,
            got,
            dropped,
        })
    }
}

/// 소켓 버퍼를 SOCK_BUF로 키우고 (전, 후) 받기 버퍼 크기를 돌려줍니다.
fn tune(sock: &UdpSocket) -> (i32, i32) {
    use windows_sys::Win32::Networking::WinSock::{SO_RCVBUF, SO_SNDBUF, SOL_SOCKET, getsockopt, setsockopt};
    let s = sock.as_raw_socket() as usize;
    let rcvbuf = || {
        let mut v = 0i32;
        let mut n = 4i32;
        // SAFETY: 우리 소켓의 int 옵션 하나를 v에 읽습니다.
        unsafe { getsockopt(s, SOL_SOCKET, SO_RCVBUF, &mut v as *mut i32 as *mut u8, &mut n) };
        v
    };
    let before = rcvbuf();
    let size = SOCK_BUF.to_ne_bytes();
    // SAFETY: 우리 소켓에 대한 평범한 int 소켓 옵션.
    unsafe {
        setsockopt(s, SOL_SOCKET, SO_RCVBUF, size.as_ptr(), 4);
        setsockopt(s, SOL_SOCKET, SO_SNDBUF, size.as_ptr(), 4);
    }
    (before, rcvbuf())
}

/// 받을 데이터그램이 생길 때까지 최대 `ms` 밀리초 기다립니다. 소켓에 받기 시간 제한(SO_RCVTIMEO)을 걸고
/// 기다리면 Windows는 시간이 다 되는 바로 그때 도착한 데이터그램을 가끔 삼킵니다 (1 ms 제한으로 20만 개 중
/// 7~159개, usbfwd의 readable과 같은 까닭).
fn readable(sock: &UdpSocket, ms: i32) -> bool {
    use windows_sys::Win32::Networking::WinSock::{POLLRDNORM, WSAPOLLFD, WSAPoll};
    let mut fd = WSAPOLLFD {
        fd: sock.as_raw_socket() as usize,
        events: POLLRDNORM,
        revents: 0,
    };
    // SAFETY: 우리 소켓 하나를 기다립니다.
    match unsafe { WSAPoll(&mut fd, 1, ms) } {
        0 => false,
        n if n > 0 => true,
        _ => {
            std::thread::sleep(Duration::from_millis(ms.max(1) as u64));
            false
        }
    }
}

/// 바로 받을 데이터그램이 있는지.
fn pending(sock: &UdpSocket) -> bool {
    use windows_sys::Win32::Networking::WinSock::{FIONREAD, ioctlsocket};
    let mut n = 0u32;
    // SAFETY: 우리 소켓에 대한 FIONREAD.
    unsafe { ioctlsocket(sock.as_raw_socket() as usize, FIONREAD, &mut n) == 0 && n > 0 }
}

impl Air for UdpAir {
    async fn recv(&mut self) -> io::Result<Received> {
        if let Some(f) = self.queue.pop_front() {
            return Ok(f);
        }
        // 기다린 뒤 돌려주기까지 다른 await가 없으므로, select!가 이 future를 버려도 프레임을 잃지 않습니다.
        let batch = self.rx.recv().await.ok_or_else(|| io::Error::other("the UDP radio stopped"))?;
        self.queue.extend(batch);
        self.queue.pop_front().ok_or_else(|| io::Error::other("an empty batch"))
    }

    fn try_recv(&mut self) -> Option<Received> {
        if self.queue.is_empty() {
            self.queue.extend(self.rx.try_recv().ok()?);
        }
        self.queue.pop_front()
    }

    fn send(&mut self, frame: Frame) -> Result<(), Frame> {
        let rate = match frame.rate {
            TxRate::Legacy(r) => r,
            TxRate::Ht { mcs, .. } => 0x80 | (mcs & 0x7f),
        };
        if self.out.len() + 6 + frame.bytes.len() > OUT_MAX {
            self.flush();
        }
        let len = (frame.bytes.len() as u16).to_le_bytes();
        self.out.extend_from_slice(&[2, rate, frame.no_ack as u8, 0, len[0], len[1]]);
        self.out.extend_from_slice(&frame.bytes);
        // 동기 TLV와 타임스탬프를 담은 액션 프레임은 바로 내보냅니다.
        if frame.is_action() {
            self.flush();
        }
        Ok(())
    }

    fn flush(&mut self) {
        if self.out.is_empty() {
            return;
        }
        if let Err(e) = self.sock.send_to(&self.out, self.peer) {
            tracing::warn!("UDP radio send failed: {e}");
        }
        self.out.clear();
    }

    fn tune(&mut self, channel: Channel) {
        self.flush();
        if let Err(e) = self.sock.send_to(&[3, channel.0, 0, 0], self.peer) {
            tracing::warn!("UDP radio channel change failed: {e}");
        }
    }
}

impl Drop for UdpAir {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        tracing::info!(
            "UDP radio closed ({} frames in, {} dropped with the queue full)",
            self.got.load(Relaxed),
            self.dropped.load(Relaxed)
        );
    }
}
