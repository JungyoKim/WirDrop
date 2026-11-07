//! owly의 [Air]를 UDP로: 중계 프로세스(usbfwd)가 USB 어댑터에서 AWDL 프레임을 잡아 넘기고, 우리
//! 프레임을 내보냅니다.
//!
//! 데이터그램은 양쪽 모두 4바이트 머리로 시작합니다.
//! - 무선 → 호스트: 1, RSSI (i8, 모르면 -128), 프레임 길이 (u16 LE), 그다음 MAC 헤더부터의 802.11
//!   프레임 (FCS 없이).
//! - 호스트 → 무선: 1, 속도 (500 kbit/s 단위, 또는 0x80 | HT MCS), ACK 없음 표시, 0, 그다음 프레임.
//!
//! 0 네 바이트짜리 데이터그램은 호스트가 어디서 듣는지 중계기에 알립니다.
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::windows::io::AsRawSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use owly_awdl::driver::{Air, Received};
use owly_awdl::{Frame, TxRate};
use tokio::sync::mpsc;

/// 루프백 소켓의 받기·보내기 버퍼 (usbfwd 쪽과 같은 크기). Windows 기본값(64 KB)에는 iPhone이
/// 묶어 보내는 A-MSDU(약 5.5 KB)가 열 개 남짓만 들어가서, 받는 스레드가 1 ms만 밀려도 넘친
/// 프레임이 아무 기록 없이 버려지고 iPhone은 그 구멍마다 TCP를 다시 보내며 속도를 줄입니다.
const SOCK_BUF: i32 = 8 << 20;

pub struct UdpAir {
    rx: mpsc::Receiver<Received>,
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
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        sock.send_to(&[0, 0, 0, 0], peer)?;
        let (rx_in, rx) = mpsc::channel(8192);
        let stop = Arc::new(AtomicBool::new(false));
        let got = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let (reader, stopped, n_got, n_dropped) = (sock.try_clone()?, stop.clone(), got.clone(), dropped.clone());
        std::thread::Builder::new().name("udp-air-rx".into()).spawn(move || {
            let mut buf = vec![0u8; 9000];
            while !stopped.load(Relaxed) {
                let Ok((n, _)) = reader.recv_from(&mut buf) else { continue };
                if n < 4 || buf[0] != 1 {
                    continue;
                }
                n_got.fetch_add(1, Relaxed);
                let len = u16::from_le_bytes([buf[2], buf[3]]) as usize;
                if 4 + len > n {
                    continue;
                }
                let rssi = buf[1] as i8;
                let f = &buf[4..4 + len];
                let frame = Received {
                    bytes: f.to_vec(),
                    rssi: (rssi != -128).then_some(rssi),
                    received_at: Some(Instant::now()),
                };
                // 받는 쪽이 밀리면 버립니다: AWDL 노드가 늦게 받는 것보다 낫습니다.
                if rx_in.try_send(frame).is_err() {
                    n_dropped.fetch_add(1, Relaxed);
                }
            }
        })?;
        // 소켓은 막히는 모드로 둡니다: try_clone은 같은 소켓을 가리켜 non-blocking 설정도 함께
        // 바뀌므로, 켜면 위 스레드의 recv_from이 기다리지 않고 헛돌며 CPU 코어 하나를 다 씁니다.
        // 루프백 UDP 보내기는 막히지 않습니다 (받는 쪽이 가득 차면 거기서 버립니다).
        Ok(UdpAir {
            rx,
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

impl Air for UdpAir {
    async fn recv(&mut self) -> io::Result<Received> {
        self.rx.recv().await.ok_or_else(|| io::Error::other("the UDP radio stopped"))
    }

    fn send(&mut self, frame: Frame) -> Result<(), Frame> {
        let rate = match frame.rate {
            TxRate::Legacy(r) => r,
            TxRate::Ht { mcs, .. } => 0x80 | (mcs & 0x7f),
        };
        let mut d = Vec::with_capacity(4 + frame.bytes.len());
        d.extend_from_slice(&[1, rate, frame.no_ack as u8, 0]);
        d.extend_from_slice(&frame.bytes);
        match self.sock.send_to(&d, self.peer) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(frame),
            Err(e) => {
                tracing::warn!("UDP radio send failed: {e}");
                Ok(())
            }
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
