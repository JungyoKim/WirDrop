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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use owly_awdl::driver::{Air, Received};
use owly_awdl::{Frame, TxRate};
use tokio::sync::mpsc;

pub struct UdpAir {
    rx: mpsc::Receiver<Received>,
    sock: UdpSocket,
    peer: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl UdpAir {
    /// local에서 peer의 중계기를 기다립니다.
    pub fn open(local: SocketAddr, peer: SocketAddr) -> io::Result<UdpAir> {
        let sock = UdpSocket::bind(local)?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        sock.send_to(&[0, 0, 0, 0], peer)?;
        let (rx_in, rx) = mpsc::channel(8192);
        let stop = Arc::new(AtomicBool::new(false));
        let (reader, stopped) = (sock.try_clone()?, stop.clone());
        std::thread::Builder::new().name("udp-air-rx".into()).spawn(move || {
            let mut buf = vec![0u8; 9000];
            while !stopped.load(Relaxed) {
                let Ok((n, _)) = reader.recv_from(&mut buf) else { continue };
                if n < 4 || buf[0] != 1 {
                    continue;
                }
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
                let _ = rx_in.try_send(frame);
            }
        })?;
        sock.set_nonblocking(true)?;
        Ok(UdpAir { rx, sock, peer, stop })
    }
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
    }
}
