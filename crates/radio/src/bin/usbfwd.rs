//! usbfwd: USB 어댑터(wirdrop-radio가 WinUSB로 사용자 모드에서 구동)와 이 PC의 wirdrop 라이브러리
//! 사이에서 AWDL 프레임을 루프백 UDP로 나릅니다.
//!
//!     무선 -> 호스트: [u8 1][i8 rssi, 모르면 -128][u16 le 길이] + 802.11 프레임 (FCS 없음)
//!     호스트 -> 무선: [u8 1][u8 속도: 500 kb/s 단위, 또는 0x80|MCS][u8 플래그: bit0 ACK 없음][u8 0] + 프레임
//!
//! --mac으로 오는 프레임에는 어댑터의 하드웨어 ACK/Block Ack 응답기가 답하므로 iPhone은 확인받는
//! A-MPDU를 연달아 보낼 수 있습니다.
//!
//!     usbfwd --mac aa:bb:cc:dd:ee:ff [--port 7770] [--peer 127.0.0.1:7771] [--channel 149]
//!            [--bw 20|40|80] [--stop-file PATH]
//!
//! 환경 변수:
//!
//!     USBFWD_VID / USBFWD_PID       어댑터 (기본: 꽂혀 있는 것, RTL8912AU 0bda:8912가 먼저, 아니면
//!                                   2357:012e TP-Link Archer T3U Nano)
//!     USBFWD_COUNTRY=KR             RTL8912AU 송신 출력 한도의 나라 (ISO 3166 두 글자)
//!     USBFWD_RETRY_LIMIT            유니캐스트 프레임의 하드웨어 재전송 횟수 (기본 8, RTL8912AU 32)
//!     USBFWD_TX_POWER               모든 속도에 쓰는 고정 TXAGC 지수
//!     USBFWD_DATA_VHT=nss/mcs[/sgi[/ldpc]]  채널 전체 폭을 쓰는 이 VHT 속도로 유니캐스트 데이터
//!     USBFWD_FAR_VHT=...            USBFWD_FAR_RSSI보다 약하게 들리는 상대에게 쓰는 속도
//!                                   (rtw88의 0-100 척도, 기본 50: -50 dBm); 기본 2/5
//!                                   (더 약한 상대는 더 느린 속도: WEAKER 참고)
//!     USBFWD_ACK_VHT=...            순수 TCP ACK의 속도 (기본: 데이터 속도)
//!     USBFWD_DATA_HE=nss/mcs        RTL8912AU: 신호가 센 상대에게 유니캐스트 데이터를 이 HE(Wi-Fi 6)
//!                                   속도로 (MCS 0-11, 0.8 us 보호 구간, LDPC; 기본 2/11, 0 = 끔).
//!                                   약한 상대와 순수 TCP ACK는 위 VHT 속도 그대로
//!     USBFWD_TX_BA=1                owly의 Block Ack 합의를 따라 상대에게 A-MPDU로 보냄
//!     USBFWD_AMPDU=tid/max[/density] A-MPDU 하나에 최대 이만큼의 MPDU (기본 16),
//!                                   802.11 밀도 코드 간격 (기본 6, 8 us)
//!     USBFWD_TX_AGG=frames[/kb]     RTL8912AU: USB 전송 하나에 담는 프레임 수와 KB (기본 16/20,
//!                                   1 = 프레임마다 전송 하나)
//!     USBFWD_BFEE=1                 보내는 상대의 채널 측정에 답함 (SU 빔포밍 수신)
//!     USBFWD_REORDER_MS (80), USBFWD_ACK_QUIET_US (200, RTL8912AU 300),
//!     USBFWD_ACK_HOLD_US (2000, RTL8912AU 1500)
//!     USBFWD_ACK_EOF_US             RTL8912AU: 상대의 PPDU가 끝나고 이만큼(us) 뒤 잡아 둔 TCP ACK를
//!                                   내보냄 (기본 60, 0 = 안 봄): PPDU마다 ACK 프레임 하나
//!     USBFWD_ACK_POLL=0             ACK를 잡아 두는 동안 잠들지 않고 지켜보기를 끔 (기본 켬)
//!     USBFWD_TX_AMSDU=bytes         RTL8912AU: 호스트가 연달아 보낸 같은 상대의 데이터 프레임을
//!                                   이 크기까지 A-MSDU 하나로 (기본 7935, 0 = 안 합침)
//!     USBFWD_IGI                    동적 이득 대신 고정 초기 이득
//!     USBFWD_EDCCA=1                에너지 감지 문턱이 이득을 따름 (rtw88의 적응 모드); 기본 끔
//!     USBFWD_PWRTRACK=1             온도에 맞춘 송신 출력 추적 (기본 끔)
//!     USBFWD_TX_REPORT=N            N번째 유니캐스트 데이터 프레임마다 확인 여부를 펌웨어에 물음
//!                                   (기본 8; 0 = 안 물음)
//!     USBFWD_SW_BAR=0               하드웨어가 포기한 프레임에서 상대의 수신 창이 멈춰도 여기서
//!                                   Block Ack Request를 보내지 않음 (기본 보냄)
//!     USBFWD_ACK_VO=1               순수 TCP ACK를 음성 큐로: 채널을 고정 34 us만 기다리므로 최선
//!                                   노력 송신자의 가장 이른 43 us보다 앞서, 답하는 데이터와 동시에
//!                                   시작하는 일이 없음
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::os::windows::io::AsRawSocket;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use wirdrop_radio::dm::{Dm, rssi_of};
use wirdrop_radio::radio::{AWDL_BSSID, C2h, RATE_CODES, mac_str, rate_name, rate_streams};
use wirdrop_radio::rtl8822b::{RXERR_MAC_DROP, RXERR_VHT_OK};
use wirdrop_radio::{Config, Radio, RxFrame, TxFrame, TxRate, Usb, UsbId, rtl8922a};

macro_rules! log {
    ($($t:tt)*) => {{
        eprintln!("usbfwd: {}", format!($($t)*));
    }};
}

fn env_num<T: std::str::FromStr>(k: &str) -> Option<T> {
    std::env::var(k).ok().and_then(|v| {
        let v = v.trim();
        if let Some(h) = v.strip_prefix("0x") {
            u64::from_str_radix(h, 16).ok().and_then(|n| n.to_string().parse().ok())
        } else {
            v.parse().ok()
        }
    })
}

#[derive(Clone, Copy)]
struct Vht {
    nss: u8,
    mcs: u8,
    sgi: bool,
    ldpc: bool,
}

impl Vht {
    const fn new(nss: u8, mcs: u8) -> Vht {
        Vht {
            nss,
            mcs,
            sgi: false,
            ldpc: false,
        }
    }

    /// 80 MHz, 긴 보호 구간에서의 PHY 속도 (100 kb/s 단위): 속도 비교용.
    fn speed(self) -> u32 {
        const ONE_STREAM: [u32; 10] = [293, 585, 878, 1170, 1755, 2340, 2633, 2925, 3510, 3900];
        ONE_STREAM[usize::from(self.mcs.min(9))] * u32::from(self.nss)
    }

    fn name(self) -> String {
        format!("VHT {}/{}", self.nss, self.mcs)
    }
}

/// far_rssi보다 약하게 들리는 상대는 문턱 하나 아래마다 한 단계씩 더 느린 속도를 받습니다 (rtw88의
/// 0-100 척도, 대략 dBm + 100). 하드웨어는 받은 속도 하나로만 재전송하므로 링크가 감당 못 하는
/// 속도면 프레임을 그대로 잃습니다: -75 dBm의 폰은 2/5로 보낸 우리 TCP SYN을 끝내 받지 못했습니다.
/// 이 값들은 Apple 기기가 그 신호에서 우리에게 보내는 속도를 따릅니다 (-60 ~ -70 dBm 근처에서 2SS
/// MCS1-3).
const WEAKER: [(f32, Vht); 3] = [(40.0, Vht::new(2, 3)), (32.0, Vht::new(2, 1)), (26.0, Vht::new(1, 1))];

/// nss/mcs[/sgi[/ldpc]].
fn parse_vht(k: &str) -> Option<Vht> {
    let v = std::env::var(k).ok()?;
    let p: Vec<u8> = v.split('/').filter_map(|x| x.trim().parse().ok()).collect();
    if p.len() >= 2 && (1..=2).contains(&p[0]) && p[1] <= 9 {
        Some(Vht {
            nss: p[0],
            mcs: p[1],
            sgi: p.get(2) == Some(&1),
            ldpc: p.get(3) == Some(&1),
        })
    } else {
        log!("ignoring {k}='{v}' (want nss/mcs[/sgi[/ldpc]])");
        None
    }
}

fn now_ns() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

#[derive(Default)]
struct Stats {
    rx_all: AtomicU64,
    rx_bad: AtomicU64,
    rx_fwd: AtomicU64,
    rx_to_us: AtomicU64,
    rx_retry: AtomicU64,
    rx_aggr: AtomicU64,
    tx: AtomicU64,
    tx_fail: AtomicU64,
    tx_vht: AtomicU64,
    tx_agg: AtomicU64,
    ba_open: AtomicU64,
    ba_closed: AtomicU64,
    ro_held: AtomicU64,
    ro_dup: AtomicU64,
    ro_old: AtomicU64,
    ro_skip: AtomicU64,
    ro_bar: AtomicU64,
    ack_paced: AtomicU64,
    ack_merged: AtomicU64,
    ndpa_to_us: AtomicU64,
    ndpa_other: AtomicU64,
    ppdu_frames: AtomicU64,
    ppdu_count: AtomicU64,
    /// 끝을 안 PPDU (RTL8912AU의 PPDU 상태 보고).
    ppdu_ends: AtomicU64,
    rpt_ampdu_ok: AtomicU64,
    rpt_ampdu_fail: AtomicU64,
    rpt_single_ok: AtomicU64,
    rpt_single_fail: AtomicU64,
    rpt_lost: AtomicU64,
    ba_rx: AtomicU64,
    ba_rx_holes: AtomicU64,
    bar_sent: AtomicU64,
    /// 호스트의 데이터 프레임을 합쳐 만든 A-MSDU와 그 안의 MSDU 수.
    tx_amsdu: AtomicU64,
    tx_amsdu_msdus: AtomicU64,
    rx_to_us_bytes: AtomicU64,
}

/// 상대가 우리에게 보낸 Block Ack이 알려 주는 그 수신 창.
#[derive(Clone, Copy)]
struct BaView {
    ssn: u16,
    bitmap: u64,
    /// 창 시작이 마지막으로 움직인 때.
    moved: Instant,
    /// 마지막 Block Ack이 온 때.
    last: Instant,
    /// 이 창 시작에 대해 마지막으로 Block Ack Request를 보낸 때.
    bar_at: Option<Instant>,
}

/// 상대 하나에 대한 Block Ack (USBFWD_TX_BA): owly가 Apple 상대마다 TID 0 합의를 청하고, 여기서
/// 받기로 보이는 상대의 ADDBA 응답이 그것을 엽니다. QoS 데이터는 무선으로 가면서 여기서 받는 쪽마다
/// 번호를 매기므로, 간격 조절기가 합쳐 없앤 ACK가 상대의 재정렬 창에 구멍을 남기지 않고, 우리 ADDBA
/// 요청은 세션이 시작하는 번호를 싣습니다.
#[derive(Clone, Copy, Default)]
struct BaPeer {
    mac: [u8; 6],
    used: bool,
    up: bool,
    amsdu: bool,
    window: u16,
    next_sn: u16,
    /// A-MPDU 안에 마지막으로 보낸 순서 번호와 그때.
    last_agg_sn: u16,
    last_agg_at: Option<Instant>,
}

#[derive(Default)]
struct BaTable([BaPeer; 8]);

impl BaTable {
    fn peer(&mut self, mac: &[u8]) -> &mut BaPeer {
        if let Some(i) = self.0.iter().position(|p| p.used && p.mac == mac[..6]) {
            return &mut self.0[i];
        }
        let i = self.0.iter().position(|p| !p.used).unwrap_or(0);
        self.0[i] = BaPeer {
            used: true,
            ..Default::default()
        };
        self.0[i].mac.copy_from_slice(&mac[..6]);
        &mut self.0[i]
    }
}

// --- 802.11 수신 재정렬 (Block Ack 세션에서 mac80211의 재정렬 버퍼가 하는 일) ---
// 어댑터가 iPhone의 A-MPDU에 하드웨어 Block Ack로 답하므로, Block Ack 비트맵에서 빠져 iPhone이 다시
// 보낸 MPDU는 뒤의 것들보다 늦게 오고, iPhone이 Block Ack을 놓친 MPDU는 두 번 옵니다. 오는 대로
// 올리면 TCP가 순서가 뒤바뀐 세그먼트와 중복을 보고, iPhone의 빠른 재전송이 창을 줄입니다. 그래서
// 우리에게 온 유니캐스트 QoS 데이터는 (TA, TID)마다 순서 번호대로 중복을 빼고 내보냅니다. 구멍은
// 길어야 USBFWD_REORDER_MS 기다리고, Block Ack Request는 창을 옮깁니다.
const WIN: usize = 64;

struct Held {
    d: Vec<u8>,
    t: Instant,
    used: bool,
}

struct Reorder {
    head: u16,
    last: Instant,
    stored: usize,
    old_run: usize,
    /// 최근 64~128개 순서 번호 가운데 호스트로 간 것: 창 뒤의 프레임은 우리가 내보냈을 때만 진짜
    /// 중복입니다 (버림).
    rel: [u64; 2],
    rel_block: i32,
    slot: Vec<Held>,
}

impl Reorder {
    fn new(head: u16, now: Instant) -> Reorder {
        Reorder {
            head,
            last: now,
            stored: 0,
            old_run: 0,
            rel: [0; 2],
            rel_block: -1,
            slot: (0..WIN)
                .map(|_| Held {
                    d: Vec::new(),
                    t: now,
                    used: false,
                })
                .collect(),
        }
    }
    fn rel_test(&self, sn: u16) -> bool {
        (self.rel[((sn >> 6) & 1) as usize] >> (sn & 63)) & 1 != 0
    }
    fn rel_mark(&mut self, sn: u16) {
        let block = (sn >> 6) as i32;
        if block != self.rel_block {
            self.rel[(block & 1) as usize] = 0;
            self.rel_block = block;
        }
        self.rel[(block & 1) as usize] |= 1u64 << (sn & 63);
    }
}

struct Relay {
    ours: [u8; 6],
    sock: UdpSocket,
    peer: Mutex<Option<SocketAddr>>,
    last_rssi: AtomicI64,
    last_rx_ns: AtomicI64,
    /// 마지막으로 PPDU가 끝난 것을 안 시각 (now_ns).
    eof_ns: AtomicI64,
    stats: Stats,
    tx_ba: bool,
    /// 순수 TCP ACK를 음성 큐로 (USBFWD_ACK_VO).
    ack_vo: bool,
    /// far_rssi보다 약하게 들리는 상대의 데이터 속도 (WEAKER도 참고), 그리고 상대마다 프레임이
    /// 마지막으로 간 단계 (바뀌면 로그에 남김).
    far_vht: Vht,
    far_rssi: f32,
    rate_step: Mutex<HashMap<[u8; 6], usize>>,
    ba: Mutex<BaTable>,
    reorder_ms: u64,
    ro: Mutex<HashMap<u64, Reorder>>,
    bfee: bool,
    bfer_want: Mutex<Option<[u8; 6]>>,
    bfer_dirty: AtomicBool,
    rate_hist: Mutex<HashMap<u8, u64>>,
    bw_hist: [AtomicU64; 4],
    path: Mutex<PathStats>,
    last_ppdu: Mutex<i32>,
    ndpa_logged: AtomicU64,
    stop: AtomicBool,
    /// RTL8912AU의 안전장치: 신호가 센 상대가 우리에게 보낸 프레임을 거의 모두 거듭 보내면 상대가
    /// 우리 응답(ACK, Block Ack)을 받지 못하는 것이므로, 최근 우리에게 온 유니캐스트 프레임과 그중
    /// 재전송을 세어 (창의 시작, 수, 재전송) 알아차리고 어댑터를 다시 올립니다. 그렇게 되던 알려진
    /// 원인 (비워 둔 상대의 CMAC 표, [rtl8922a::Rtl8922a::add_peer])은 고쳤습니다.
    answers: Mutex<AnswerWindow>,
    check_answers: bool,
    /// 이때까지는 보지 않습니다 (다시 올리기가 너무 잦았을 때).
    answers_paused: Mutex<Option<Instant>>,
    /// 어댑터를 다시 올려야 합니다 (stop과 함께 켭니다).
    restart: AtomicBool,
    /// 순수 TCP ACK를 보내기 전에 기다리는 조용한 시간(µs, 0이면 곧바로 보냄).
    ack_quiet_us: i64,
    /// AWDL 상대마다 부드럽게 한 RSSI(0-100)와 마지막으로 들린 때: 가장 약한 것이 수신 이득의
    /// 한계를 정합니다.
    peers: Mutex<HashMap<[u8; 6], (f32, Instant)>>,
    /// 최근 우리에게 유니캐스트 데이터를 보낸 상대들: DIG가 신경 쓰는 링크.
    data_peers: Mutex<HashMap<[u8; 6], Instant>>,
    /// 청한 송신 보고: 번호 -> (A-MPDU인지, 때).
    reports: Mutex<HashMap<u8, (bool, Instant)>>,
    reports_logged: AtomicU64,
    /// Block Ack이 보여 주는 상대마다의 창.
    ba_view: Mutex<HashMap<[u8; 6], BaView>>,
    sw_bar: bool,
    /// 상대마다 마지막으로 들린 때 (우리에게 온 프레임이든 AWDL의 것이든): 그때 보낸 프레임은
    /// 닿습니다.
    heard: Mutex<HashMap<[u8; 6], Instant>>,
    /// 상대가 보내는 데이터의 신호(0-100), 마지막 때, 지난 watchdog 뒤 프레임 수: 수신 이득은
    /// 전송하는 상대에 맞아야 합니다.
    data_rssi: Mutex<HashMap<[u8; 6], (f32, Instant, u32)>>,
    presence: Mutex<Presence>,
    /// 지난 통계 줄 뒤 PHY가 받은 CRC 정상과 실패 MPDU (모든 송신자: OFDM, HT, VHT).
    crc: Mutex<[(u64, u64); 3]>,
    /// 데이터 프레임(페이로드 있음)이 마지막으로 온 때, 그리고 그 뒤 호스트의 TCP ACK가 돌아와
    /// 전파를 탄 시간(µs): 보내는 쪽 창이 기다리는 ACK 순환.
    last_data_ns: AtomicI64,
    ack_host_us: Mutex<Vec<u32>>,
    ack_air_us: Mutex<Vec<u32>>,
    /// 상대가 보내는 파일 내용만 본 이번 통계 구간 ([Bulk]).
    bulk: Mutex<Bulk>,
    /// 마지막으로 받은 프레임의 PPDU 카운터, 그리고 지금 PPDU에서 큰 데이터 프레임을 이미 셌는지:
    /// 묶음 크기를 PPDU 단위로 셉니다.
    prev_ppdu: AtomicU8,
    ppdu_bulk: AtomicBool,
}

/// 1초 창 안에서 받은 유니캐스트 수와 그중 재전송, 그리고 (보낸 이, 순서 번호)마다 받은 횟수:
/// 응답하지 못하는 칩에서는 상대가 프레임마다 재전송 한도까지 (iPhone은 일고여덟 번) 거듭 보냅니다.
struct AnswerWindow {
    start: Instant,
    frames: u32,
    retries: u32,
    seen: HashMap<([u8; 6], u16), u8>,
}

impl AnswerWindow {
    fn new() -> AnswerWindow {
        AnswerWindow {
            start: Instant::now(),
            frames: 0,
            retries: 0,
            seen: HashMap::new(),
        }
    }
}

/// 데이터를 주고받는 상대가 우리 채널에 머무는 비율: AWDL 상대는 자기 Wi-Fi 네트워크의 채널도
/// 돌보므로 전송 중 프레임 사이의 틈은 모두 데이터가 멈춘 시간입니다.
#[derive(Default)]
struct Presence {
    last: Option<Instant>,
    /// 전송이 도는 동안 (200 ms 안의 데이터): 전체 시간과 4 ms 넘는 틈.
    active: Duration,
    away: Duration,
    gaps: u64,
    /// 길이별 틈: 4-12, 12-24, 24-48, 48-100, 100-200 ms.
    hist: [u32; 5],
    /// 지금 데이터 흐름이 시작한 때, 그리고 처음 1초의 시간과 틈.
    run_start: Option<Instant>,
    first_active: Duration,
    first_away: Duration,
}

#[derive(Default)]
struct PathStats {
    n: i64,
    pwr: [i64; 2],
    snr: [i64; 2],
    evm: [i64; 2],
}

/// 이보다 큰 데이터 프레임을 파일 내용으로 봅니다. 상대가 우리 데이터에 답하는 TCP ACK와 Null
/// 프레임은 작고 묶이지 않으며 20 MHz로도 와서, 누적 "rx rate" 줄을 흐립니다.
const BULK: usize = 1000;
/// 속도 부호의 수 ([rate_name]: VHT 4스트림, HE 2스트림까지).
const RATES: usize = RATE_CODES;

/// 우리에게 온 큰 데이터 프레임만 본 한 통계 구간: 상대가 어떤 속도로, 몇 개씩 묶어 보내는지, 그리고
/// 같은 스트림 수의 프레임이 우리 쪽에서 얼마나 깨지는지. 상대가 2스트림을 두고 1스트림을 고른다면
/// 그 까닭이 우리 수신(2스트림이 깨짐)인지, 확인 응답(재전송이 많음)인지를 가릅니다.
struct Bulk {
    frames: u64,
    bytes: u64,
    /// 재전송 비트가 켜진 프레임: 상대가 우리 확인을 못 받았다고 본 것.
    retries: u64,
    /// 큰 데이터 프레임이 하나라도 든 PPDU.
    ppdus: u64,
    /// 속도 코드마다 받은 바이트.
    bytes_at: [u64; RATES],
    /// PPDU의 대역폭 (PHY 상태는 PPDU의 첫 MPDU에만 있습니다): 20, 40, 80, 160 MHz.
    bw: [u64; 4],
    /// 속도 코드마다 CRC가 맞은 것과 깨진 HT·VHT MPDU. 깨진 프레임은 주소를 믿을 수 없어 모든
    /// 송신자를 셉니다 (전송 중에는 거의 모두 그 상대입니다).
    ok_at: [u64; RATES],
    bad_at: [u64; RATES],
    /// 우리에게 온 채널 측정 (VHT NDPA): 상대가 우리 쪽으로 빔포밍하려고 묻는 것.
    ndpa: u64,
}

impl Bulk {
    const EMPTY: Bulk = Bulk {
        frames: 0,
        bytes: 0,
        retries: 0,
        ppdus: 0,
        bytes_at: [0; RATES],
        bw: [0; 4],
        ok_at: [0; RATES],
        bad_at: [0; RATES],
        ndpa: 0,
    };

    /// 통계 한 줄: 받은 양, 스트림 수마다 몫과 가장 많이 쓴 속도와 깨진 비율, 대역폭, 묶음 크기,
    /// 재전송, 채널 측정.
    fn summary(&self) -> String {
        let pct = |part: u64, whole: u64| 100.0 * part as f64 / whole.max(1) as f64;
        let mut per_stream = Vec::new();
        for nss in 1..=2u8 {
            let codes = || (0..RATES as u8).filter(move |&c| rate_streams(c) == Some(nss));
            let bytes: u64 = codes().map(|c| self.bytes_at[c as usize]).sum();
            let (ok, bad) = codes().fold((0, 0), |(o, b), c| (o + self.ok_at[c as usize], b + self.bad_at[c as usize]));
            let top = codes()
                .filter(|&c| self.bytes_at[c as usize] > 0)
                .max_by_key(|&c| self.bytes_at[c as usize])
                .map(rate_name)
                .unwrap_or_else(|| "-".into());
            let lost = if ok + bad > 0 {
                format!("{:.0}%", pct(bad, ok + bad))
            } else {
                "-".into()
            };
            per_stream.push(format!("{nss}SS {:.0}% (mostly {top}, CRC lost {lost})", pct(bytes, self.bytes)));
        }
        format!(
            "bulk to us: {} frames {:.1} MB | {} | PPDU bw 20/40/80/160 {}/{}/{}/{} | {:.1} MPDU per PPDU | retry {:.0}% | NDPA {}",
            self.frames,
            self.bytes as f64 / 1e6,
            per_stream.join(" | "),
            self.bw[0],
            self.bw[1],
            self.bw[2],
            self.bw[3],
            self.frames as f64 / self.ppdus.max(1) as f64,
            pct(self.retries, self.frames),
            self.ndpa
        )
    }
}

impl Relay {
    /// 약한 상대일수록 더 느린 속도: 설정한 속도는 팔 길이 거리에서 잰 것이고, 하드웨어는 재전송 때
    /// 속도를 낮추지 않습니다 (DISDATAFB). v보다 빠르지 않습니다.
    fn rate_for(&self, dst: &[u8], v: Vht) -> Vht {
        let Some(rssi) = self.peers.lock().unwrap().get(dst).map(|(r, _)| *r) else {
            return v;
        };
        // 신호가 그 아래인 단계 가운데 가장 느린 것 (v보다 느리면). 단계 0은 v 자체.
        let (step, rate) = std::iter::once((self.far_rssi, self.far_vht))
            .chain(WEAKER)
            .enumerate()
            .filter(|(_, (below, r))| rssi < *below && r.speed() < v.speed())
            .min_by_key(|(_, (_, r))| r.speed())
            .map_or((0, v), |(i, (_, r))| (i + 1, r));
        let Ok(peer) = <[u8; 6]>::try_from(dst) else { return rate };
        if self.rate_step.lock().unwrap().insert(peer, step) != Some(step) {
            log!("{} heard at RSSI {rssi:.0}: data at {}", mac_str(dst), rate.name());
        }
        rate
    }

    fn add_crc(&self, c: &wirdrop_radio::rtl8822b::phy::PhyCounters) {
        let mut t = self.crc.lock().unwrap();
        for (i, (ok, bad)) in [c.ofdm, c.ht, c.vht].into_iter().enumerate() {
            t[i].0 += ok as u64;
            t[i].1 += bad as u64;
        }
    }

    fn send_up(&self, d: &[u8]) {
        if let Some(to) = *self.peer.lock().unwrap()
            && self.sock.send_to(d, to).is_ok()
        {
            self.stats.rx_fwd.fetch_add(1, Relaxed);
        }
    }

    fn on_ba_action(&self, f: &[u8]) {
        if f[25] == 1 && f.len() >= 33 {
            let status = u16::from_le_bytes([f[27], f[28]]);
            let params = u16::from_le_bytes([f[29], f[30]]);
            if (params >> 2) & 0x0f != 0 {
                return;
            }
            let mut t = self.ba.lock().unwrap();
            let p = t.peer(&f[10..16]);
            p.up = status == 0;
            p.amsdu = params & 1 != 0;
            p.window = (params >> 6) & 0x3ff;
            if p.up {
                self.stats.ba_open.fetch_add(1, Relaxed);
            }
            log!(
                "Block Ack to {} {} (status {status}, window {}, A-MSDU {})",
                mac_str(&f[10..16]),
                if p.up { "open" } else { "refused" },
                p.window,
                if p.amsdu { "yes" } else { "no" }
            );
        } else if f[25] == 2 && f.len() >= 28 {
            let params = u16::from_le_bytes([f[26], f[27]]);
            // Initiator가 켜져 있습니다: 상대가 끝내는 것은 우리 세션이 아니라 자기 세션(우리 수신
            // 쪽)입니다.
            if (params >> 12) != 0 || params & 0x0800 != 0 {
                return;
            }
            let mut t = self.ba.lock().unwrap();
            let p = t.peer(&f[10..16]);
            if p.up {
                self.stats.ba_closed.fetch_add(1, Relaxed);
            }
            p.up = false;
            log!("Block Ack to {} closed by the peer", mac_str(&f[10..16]));
        }
    }

    /// 이 상대와 Block Ack 합의가 열려 있고 그 안에서 A-MSDU를 받는지.
    fn amsdu_ok(&self, dst: &[u8]) -> bool {
        let mut t = self.ba.lock().unwrap();
        let p = t.peer(dst);
        p.up && p.amsdu
    }

    /// 무선으로 가는 프레임에 번호를 매기고 A-MPDU에 실어도 되는지 말합니다.
    fn prepare_tx(&self, f: &mut [u8]) -> bool {
        let n = f.len();
        if n >= 33 && f[0] == 0xd0 && f[24] == 3 && f[25] == 1 {
            // 상대의 Block Ack 요청에 우리가 내는 답: 상대가 한 번에 보낼 수 있는 양을 정합니다.
            let params = u16::from_le_bytes([f[29], f[30]]);
            log!(
                "Block Ack answered to {}: status {}, window {}, A-MSDU {}",
                mac_str(&f[4..10]),
                u16::from_le_bytes([f[27], f[28]]),
                (params >> 6) & 0x3ff,
                if params & 1 != 0 { "yes" } else { "no" }
            );
        }
        if !self.tx_ba || n < 24 || f[4] & 1 != 0 {
            return false;
        }
        if f[0] == 0xd0 && n >= 33 && f[24] == 3 && f[25] == 0 {
            // 우리 ADDBA 요청: 새 요청은 합의를 바꿉니다
            let mut t = self.ba.lock().unwrap();
            let p = t.peer(&f[4..10]);
            p.up = false;
            let ssc = p.next_sn << 4;
            f[31..33].copy_from_slice(&ssc.to_le_bytes());
            return false;
        }
        if f[0] != 0x88 || n < 26 || f[24] & 0x0f != 0 {
            return false;
        }
        let mut t = self.ba.lock().unwrap();
        let p = t.peer(&f[4..10]);
        let sn = p.next_sn;
        f[22..24].copy_from_slice(&(sn << 4).to_le_bytes());
        p.next_sn = (p.next_sn + 1) & 0x0fff;
        let normal_ack = (f[24] >> 5) & 3 == 0;
        let amsdu = f[24] & 0x80 != 0;
        let agg = p.up && normal_ack && (!amsdu || p.amsdu);
        if agg {
            p.last_agg_sn = sn;
            p.last_agg_at = Some(Instant::now());
        }
        agg
    }

    // ---- 재정렬 버퍼

    fn ro_emit(&self, r: &mut Reorder, i: usize) {
        let d = std::mem::take(&mut r.slot[i].d);
        self.send_up(&d);
        r.slot[i].used = false;
        r.stored -= 1;
    }

    fn ro_release(&self, r: &mut Reorder) {
        while r.slot[r.head as usize % WIN].used {
            let h = r.head;
            r.rel_mark(h);
            self.ro_emit(r, h as usize % WIN);
            r.head = (r.head + 1) & 0xfff;
        }
    }

    fn ro_advance(&self, r: &mut Reorder, to: u16) {
        while r.head != to {
            let i = r.head as usize % WIN;
            if r.slot[i].used {
                let h = r.head;
                r.rel_mark(h);
                self.ro_emit(r, i);
            } else {
                self.stats.ro_skip.fetch_add(1, Relaxed);
            }
            r.head = (r.head + 1) & 0xfff;
        }
    }

    fn ro_input(&self, dg: &[u8], ta: &[u8], tid: u8, sn: u16, now: Instant) {
        let mut map = self.ro.lock().unwrap();
        let fresh = !map.contains_key(&ro_key(ta, tid));
        let r = map.entry(ro_key(ta, tid)).or_insert_with(|| Reorder::new(sn, now));
        if fresh || (r.stored == 0 && now - r.last > Duration::from_secs(1)) {
            r.head = sn;
        }
        r.last = now;
        let mut d = sn.wrapping_sub(r.head) & 0xfff;
        if d >= 2048 {
            // 창 뒤
            if r.rel_test(sn) {
                self.stats.ro_dup.fetch_add(1, Relaxed);
                return;
            }
            self.stats.ro_old.fetch_add(1, Relaxed);
            self.send_up(dg);
            r.old_run += 1;
            if r.old_run < WIN {
                return;
            }
            let to = (r.head + WIN as u16) & 0xfff;
            self.ro_advance(r, to); // 상대가 번호를 다시 시작했습니다: 다시 맞춤
            r.head = sn;
            d = 0;
        }
        r.old_run = 0;
        let i = sn as usize % WIN;
        if d == 0 && !r.slot[i].used {
            r.rel_mark(sn);
            self.send_up(dg);
            r.head = (r.head + 1) & 0xfff;
            self.ro_release(r);
            return;
        }
        if d as usize >= WIN {
            let to = sn.wrapping_sub(WIN as u16 - 1) & 0xfff;
            self.ro_advance(r, to);
        }
        if r.slot[i].used {
            self.stats.ro_dup.fetch_add(1, Relaxed);
            return;
        }
        r.slot[i] = Held {
            d: dg.to_vec(),
            t: now,
            used: true,
        };
        r.stored += 1;
        if sn != r.head {
            self.stats.ro_held.fetch_add(1, Relaxed);
        }
        self.ro_release(r);
    }

    fn ro_bar(&self, ta: &[u8], tid: u8, ssn: u16) {
        let mut map = self.ro.lock().unwrap();
        let Some(r) = map.get_mut(&ro_key(ta, tid)) else { return };
        if ssn.wrapping_sub(r.head) & 0xfff >= 2048 {
            return;
        }
        self.stats.ro_bar.fetch_add(1, Relaxed);
        self.ro_advance(r, ssn);
        self.ro_release(r);
    }

    fn ro_tick(&self, now: Instant) {
        let limit = Duration::from_millis(self.reorder_ms);
        let mut map = self.ro.lock().unwrap();
        for r in map.values_mut() {
            while r.stored > 0 {
                let oldest = r.slot.iter().filter(|h| h.used).map(|h| h.t).min().unwrap_or(now);
                if now - oldest < limit {
                    break;
                }
                let i = r.head as usize % WIN;
                if r.slot[i].used {
                    let h = r.head;
                    r.rel_mark(h);
                    self.ro_emit(r, i);
                } else {
                    self.stats.ro_skip.fetch_add(1, Relaxed);
                }
                r.head = (r.head + 1) & 0xfff;
                self.ro_release(r);
            }
        }
    }

    // ---- 받기

    fn on_rx(&self, p: &RxFrame) {
        let s = &self.stats;
        if p.ampdu_end {
            // 상대의 PPDU가 끝남: 잡아 둔 TCP ACK를 내보낼 때 (tx_loop).
            self.eof_ns.store(now_ns(), Relaxed);
            s.ppdu_ends.fetch_add(1, Relaxed);
        }
        if p.data.is_empty() {
            // 끝만 알리는 표시.
            return;
        }
        s.rx_all.fetch_add(1, Relaxed);
        self.last_rx_ns.store(now_ns(), Relaxed);
        if self.prev_ppdu.swap(p.ppdu_cnt, Relaxed) != p.ppdu_cnt {
            self.ppdu_bulk.store(false, Relaxed);
        }
        let ht = rate_streams(p.rate).is_some();
        if p.crc_err || p.icv_err {
            s.rx_bad.fetch_add(1, Relaxed);
            if p.crc_err && ht {
                self.bulk.lock().unwrap().bad_at[p.rate as usize] += 1;
            }
            return;
        }
        if ht {
            self.bulk.lock().unwrap().ok_at[p.rate as usize] += 1;
        }
        if let Some(phy) = p.phy {
            self.last_rssi.store(phy.power[0] as i64, Relaxed);
        }
        let f = p.data;
        let len = f.len();
        if !(24..=9000).contains(&len) {
            return;
        }
        let ty = (f[0] >> 2) & 3;
        let to_us = f[4..10] == self.ours;
        if to_us && ty != 1 && f[10] & 1 == 0 && self.check_answers {
            self.check_answered(f);
        }
        if to_us || f[16..22] == AWDL_BSSID {
            let mut ta = [0u8; 6];
            ta.copy_from_slice(&f[10..16]);
            let now = Instant::now();
            self.heard.lock().unwrap().insert(ta, now);
            // 데이터를 보내는 상대의 존재: 데이터가 흐르는 동안 프레임 사이의 틈.
            if self
                .data_peers
                .lock()
                .unwrap()
                .get(&ta)
                .is_some_and(|t| now - *t < Duration::from_millis(200))
            {
                let mut p = self.presence.lock().unwrap();
                if let Some(last) = p.last {
                    let gap = now - last;
                    if gap < Duration::from_millis(200) {
                        let first = p.run_start.is_some_and(|s| now - s <= Duration::from_secs(1));
                        p.active += gap;
                        if first {
                            p.first_active += gap;
                        }
                        if gap > Duration::from_millis(4) {
                            p.away += gap;
                            p.gaps += 1;
                            if first {
                                p.first_away += gap;
                            }
                            let ms = gap.as_millis();
                            let b = [12, 24, 48, 100].iter().position(|&e| ms < e).unwrap_or(4);
                            p.hist[b] += 1;
                        }
                    } else {
                        p.run_start = Some(now);
                    }
                } else {
                    p.run_start = Some(now);
                }
                p.last = Some(now);
            }
        }
        if ty != 1
            && let Some(phy) = p.phy
            && (to_us || f[16..22] == AWDL_BSSID)
        {
            // rtw88은 스테이션마다 ewma(rssi, 10, 16)를 둡니다.
            let rssi = rssi_of(&phy.power) as f32;
            let mut ta = [0u8; 6];
            ta.copy_from_slice(&f[10..16]);
            let mut peers = self.peers.lock().unwrap();
            let e = peers.entry(ta).or_insert((rssi, Instant::now()));
            e.0 += (rssi - e.0) / 16.0;
            e.1 = Instant::now();
        }
        if ty == 1 {
            // 우리에게 온 압축 Block Ack: 상대의 수신 창.
            if to_us && f[0] == 0x94 && len >= 28 && u16::from_le_bytes([f[16], f[17]]) & 0x0004 != 0 {
                let ssn = u16::from_le_bytes([f[18], f[19]]) >> 4;
                let bitmap = u64::from_le_bytes(f[20..28].try_into().unwrap());
                s.ba_rx.fetch_add(1, Relaxed);
                // 빠진 것 뒤에 받은 프레임: 상대가 기다리는 구멍.
                if bitmap != 0 && (bitmap >> bitmap.trailing_ones()) != 0 {
                    s.ba_rx_holes.fetch_add(1, Relaxed);
                }
                let mut ta = [0u8; 6];
                ta.copy_from_slice(&f[10..16]);
                let now = Instant::now();
                let mut views = self.ba_view.lock().unwrap();
                let v = views.entry(ta).or_insert(BaView {
                    ssn,
                    bitmap,
                    moved: now,
                    last: now,
                    bar_at: None,
                });
                if v.ssn != ssn {
                    v.moved = now;
                    v.bar_at = None;
                }
                v.ssn = ssn;
                v.bitmap = bitmap;
                v.last = now;
            }
            // 제어 프레임은 전파에 남고, 우리에게 온 Block Ack Request는 창을 옮깁니다
            if self.reorder_ms > 0 && to_us && f[0] == 0x84 && len >= 20 {
                self.ro_bar(&f[10..16], f[17] >> 4, u16::from_le_bytes([f[18], f[19]]) >> 4);
            }
            if f[0] == 0x54 && len >= 19 {
                // VHT NDPA: 채널을 측정하는 상대
                if to_us {
                    s.ndpa_to_us.fetch_add(1, Relaxed);
                    self.bulk.lock().unwrap().ndpa += 1;
                    if self.ndpa_logged.fetch_add(1, Relaxed) < 3 {
                        let info = u16::from_le_bytes([f[17], f[18]]);
                        log!(
                            "NDPA from {}: AID {}, {} feedback, Nc {}",
                            mac_str(&f[10..16]),
                            info & 0xfff,
                            if (info >> 12) & 1 != 0 { "MU" } else { "SU" },
                            (info >> 13) + 1
                        );
                    }
                } else {
                    s.ndpa_other.fetch_add(1, Relaxed);
                }
            }
            return;
        }
        if !to_us && f[16..22] != AWDL_BSSID {
            return;
        }
        if self.tx_ba && to_us && f[0] == 0xd0 && len >= 26 && f[24] == 3 {
            self.on_ba_action(f);
        }
        if to_us && f[0] == 0xd0 && len >= 33 && f[24] == 3 && f[25] == 0 {
            // 상대의 Block Ack 요청: 상대가 원하는 창.
            let params = u16::from_le_bytes([f[27], f[28]]);
            log!(
                "Block Ack asked by {}: TID {}, window {}, A-MSDU {}",
                mac_str(&f[10..16]),
                (params >> 2) & 0x0f,
                (params >> 6) & 0x3ff,
                if params & 1 != 0 { "yes" } else { "no" }
            );
        }
        if to_us && ty == 2 {
            s.rx_to_us.fetch_add(1, Relaxed);
            s.rx_to_us_bytes.fetch_add(len as u64, Relaxed);
            if len > 200 {
                self.last_data_ns.store(now_ns(), Relaxed);
            }
            if len >= BULK {
                let mut b = self.bulk.lock().unwrap();
                b.frames += 1;
                b.bytes += len as u64;
                if f[1] & 0x08 != 0 {
                    b.retries += 1;
                }
                if !self.ppdu_bulk.swap(true, Relaxed) {
                    b.ppdus += 1;
                }
                if let Some(at) = b.bytes_at.get_mut(p.rate as usize) {
                    *at += len as u64;
                }
                if let Some(phy) = p.phy {
                    b.bw[(phy.bw as usize).min(3)] += 1;
                }
            }
            let mut ta = [0u8; 6];
            ta.copy_from_slice(&f[10..16]);
            self.data_peers.lock().unwrap().insert(ta, Instant::now());
            if let Some(phy) = p.phy {
                let rssi = rssi_of(&phy.power) as f32;
                let mut m = self.data_rssi.lock().unwrap();
                let e = m.entry(ta).or_insert((rssi, Instant::now(), 0));
                e.0 += (rssi - e.0) / 16.0;
                e.1 = Instant::now();
                e.2 += 1;
            }
            if f[1] & 0x08 != 0 {
                s.rx_retry.fetch_add(1, Relaxed);
            }
            *self.rate_hist.lock().unwrap().entry(p.rate).or_insert(0) += 1;
            if self.bfee && f[0] == 0x88 {
                let mut want = self.bfer_want.lock().unwrap();
                if want.map(|m| m[..] != f[10..16]).unwrap_or(true) {
                    let mut m = [0u8; 6];
                    m.copy_from_slice(&f[10..16]);
                    *want = Some(m);
                    self.bfer_dirty.store(true, Relaxed);
                }
            }
            if let Some(phy) = p.phy {
                self.bw_hist[(phy.bw as usize).min(3)].fetch_add(1, Relaxed);
                let mut ps = self.path.lock().unwrap();
                ps.n += 1;
                for i in 0..2 {
                    ps.pwr[i] += phy.power[i] as i64;
                    ps.snr[i] += phy.snr[i] as i64;
                    ps.evm[i] += phy.evm[i] as i64;
                }
            }
            s.ppdu_frames.fetch_add(1, Relaxed);
            let mut last = self.last_ppdu.lock().unwrap();
            if p.ppdu_cnt as i32 != *last {
                *last = p.ppdu_cnt as i32;
                s.ppdu_count.fetch_add(1, Relaxed);
            } else {
                s.rx_aggr.fetch_add(1, Relaxed);
            }
        }
        if self.peer.lock().unwrap().is_none() {
            return;
        }
        let mut out = Vec::with_capacity(len + 4);
        out.push(1);
        out.push(self.last_rssi.load(Relaxed) as i8 as u8);
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(f);
        // 우리에게 온 유니캐스트 QoS 데이터 (서브타입 8만), No Ack이 아닌 모든 ACK 정책은 재정렬
        // 버퍼를 거칩니다.
        if self.reorder_ms > 0 && to_us && f[0] == 0x88 && len >= 26 && (f[24] >> 5) & 3 != 1 {
            self.ro_input(&out, &f[10..16], f[24] & 0x0f, u16::from_le_bytes([f[22], f[23]]) >> 4, Instant::now());
        } else {
            self.send_up(&out);
        }
    }

    /// 우리에게 온 유니캐스트 프레임 하나: 1초 창 안에서 서로 다른 프레임 셋 이상이 저마다 다섯 번
    /// 넘게 오면 상대가 우리 응답을 받지 못하는 것으로 봅니다. 재전송 비율만으로는 보지 않습니다:
    /// 전송 끝 무렵에는 Block Ack이 빠뜨린 MPDU의 재전송(한두 번이면 닿음)이 남은 프레임의 대부분이
    /// 될 수 있습니다. 다시 올린 직후에는 상대가 그동안 쌓인 프레임을 재전송으로 보내므로 잠시 보지
    /// 않고 (run의 다시 올리기), 먼 상대는 약한 신호 탓에 우리 ACK을 놓치므로 신호가 약한 상대(RSSI 40
    /// 아래)는 보지 않습니다.
    fn check_answered(&self, f: &[u8]) {
        if self.answers_paused.lock().unwrap().is_some_and(|t| Instant::now() < t) {
            return;
        }
        let mut ta = [0u8; 6];
        ta.copy_from_slice(&f[10..16]);
        if self.peers.lock().unwrap().get(&ta).is_none_or(|(rssi, _)| *rssi < 40.0) {
            return;
        }
        let seq = u16::from_le_bytes([f[22], f[23]]) >> 4;
        let mut a = self.answers.lock().unwrap();
        if a.start.elapsed() >= Duration::from_secs(1) {
            *a = AnswerWindow::new();
        }
        a.frames += 1;
        if f[1] & 0x08 != 0 {
            a.retries += 1;
        }
        let (frames, retries) = (a.frames, a.retries);
        let n = a.seen.entry((ta, seq)).or_default();
        *n = n.saturating_add(1);
        let repeated = if *n == 5 {
            a.seen.iter().filter(|((t, _), c)| *t == ta && **c >= 5).count()
        } else {
            0
        };
        if repeated >= 3 {
            *a = AnswerWindow::new();
            log!(
                "the adapter isn't answering: {retries} of {frames} frames came again ({repeated} of them 5 times or more), last from {}; bringing it up again",
                mac_str(&ta)
            );
            self.restart.store(true, Relaxed);
            self.stop.store(true, Relaxed);
        }
    }

    fn print_stats(&self, radio: &Radio) {
        let s = &self.stats;
        let g = |a: &AtomicU64| a.load(Relaxed);
        log!(
            "rx {} bad {} fwd {} | data_to_us {} retry {} aggr {} | tx {} fail {} vht {} | rssi {}",
            g(&s.rx_all),
            g(&s.rx_bad),
            g(&s.rx_fwd),
            g(&s.rx_to_us),
            g(&s.rx_retry),
            g(&s.rx_aggr),
            g(&s.tx),
            g(&s.tx_fail),
            g(&s.tx_vht),
            self.last_rssi.load(Relaxed)
        );
        if self.reorder_ms > 0 {
            log!(
                "reorder held {} dup {} old {} skip {} bar {}",
                g(&s.ro_held),
                g(&s.ro_dup),
                g(&s.ro_old),
                g(&s.ro_skip),
                g(&s.ro_bar)
            );
        }
        log!(
            "ack paced {} merged {} | PPDU ends {} | host frames merged into {} A-MSDUs of {} MSDUs",
            g(&s.ack_paced),
            g(&s.ack_merged),
            g(&s.ppdu_ends),
            g(&s.tx_amsdu),
            g(&s.tx_amsdu_msdus)
        );
        {
            let pct = |v: &mut Vec<u32>| -> Option<(u32, u32, u32)> {
                if v.len() < 20 {
                    v.clear();
                    return None;
                }
                v.sort_unstable();
                let at = |q: usize| v[(v.len() - 1) * q / 100];
                let r = (at(50), at(90), at(99));
                v.clear();
                Some(r)
            };
            let host = pct(&mut self.ack_host_us.lock().unwrap());
            let air = pct(&mut self.ack_air_us.lock().unwrap());
            if let (Some(h), Some(a)) = (host, air) {
                log!(
                    "TCP ACK after the data it acknowledges: back from the host p50 {} p90 {} p99 {} us | on the air p50 {} p90 {} p99 {} us",
                    h.0,
                    h.1,
                    h.2,
                    a.0,
                    a.1,
                    a.2
                );
            }
        }
        {
            let mut p = self.presence.lock().unwrap();
            if p.active > Duration::from_millis(500) {
                log!(
                    "data peer on our channel {:.0}% of {:.1} s of transfer ({} gaps over 4 ms, {:.0} ms away)",
                    100.0 * (1.0 - p.away.as_secs_f64() / p.active.as_secs_f64()),
                    p.active.as_secs_f64(),
                    p.gaps,
                    p.away.as_secs_f64() * 1000.0
                );
                let h = p.hist;
                let first = if p.first_active > Duration::from_millis(200) {
                    format!(
                        "{:.0}% of the first {:.1} s",
                        100.0 * (1.0 - p.first_away.as_secs_f64() / p.first_active.as_secs_f64()),
                        p.first_active.as_secs_f64()
                    )
                } else {
                    "-".into()
                };
                log!(
                    "gaps 4-12/12-24/24-48/48-100/100-200 ms: {}/{}/{}/{}/{} | on our channel in each run's first second: {first}",
                    h[0],
                    h[1],
                    h[2],
                    h[3],
                    h[4]
                );
            }
            let (last, run_start) = (p.last, p.run_start);
            *p = Presence {
                last,
                run_start,
                ..Presence::default()
            };
        }
        {
            let mut rp = self.reports.lock().unwrap();
            let before = rp.len();
            rp.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(2));
            s.rpt_lost.fetch_add((before - rp.len()) as u64, Relaxed);
        }
        if g(&s.rpt_ampdu_ok) + g(&s.rpt_ampdu_fail) + g(&s.rpt_single_ok) + g(&s.rpt_single_fail) + g(&s.rpt_lost) > 0 {
            log!(
                "tx reports: A-MPDU ok {} fail {} | single ok {} fail {} | no report {}",
                g(&s.rpt_ampdu_ok),
                g(&s.rpt_ampdu_fail),
                g(&s.rpt_single_ok),
                g(&s.rpt_single_fail),
                g(&s.rpt_lost)
            );
        }
        if self.tx_ba {
            log!(
                "tx A-MPDU-marked {} | Block Ack sessions opened {} closed {} | Block Acks in {} (gap in the bitmap {}) | Block Ack Requests out {}",
                g(&s.tx_agg),
                g(&s.ba_open),
                g(&s.ba_closed),
                g(&s.ba_rx),
                g(&s.ba_rx_holes),
                g(&s.bar_sent)
            );
        }
        let mut ps = self.path.lock().unwrap();
        if ps.n > 0 {
            let n = ps.n as f64;
            log!(
                "rx paths ({} PPDUs): A {:.1} dBm SNR {:.1} dB | B {:.1} dBm SNR {:.1} dB | EVM {:.1} / {:.1} dB | IGI 0x{:02x}",
                ps.n,
                ps.pwr[0] as f64 / n,
                ps.snr[0] as f64 / (2.0 * n),
                ps.pwr[1] as f64 / n,
                ps.snr[1] as f64 / (2.0 * n),
                ps.evm[0] as f64 / (2.0 * n),
                ps.evm[1] as f64 / (2.0 * n),
                radio.rtw88().and_then(|c| c.igi().ok()).unwrap_or(0)
            );
            *ps = PathStats::default();
        }
        if self.bfee {
            log!("NDPA to us {}, to others {}", g(&s.ndpa_to_us), g(&s.ndpa_other));
        }
        let hist = self.rate_hist.lock().unwrap();
        let tot: u64 = hist.values().sum();
        if tot > 0 {
            let mut v: Vec<_> = hist.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let top = v
                .iter()
                .take(3)
                .map(|(r, n)| format!("{} {}%", rate_name(**r), 100 * **n / tot))
                .collect::<Vec<_>>();
            let pc = g(&s.ppdu_count);
            log!(
                "rx rate {} | bw 20/40/80/160 {}/{}/{}/{} | mean aggregate {:.1} MPDU of {:.0} bytes",
                top.join(", "),
                g(&self.bw_hist[0]),
                g(&self.bw_hist[1]),
                g(&self.bw_hist[2]),
                g(&self.bw_hist[3]),
                if pc > 0 { g(&s.ppdu_frames) as f64 / pc as f64 } else { 0.0 },
                g(&s.rx_to_us_bytes) as f64 / g(&s.rx_to_us).max(1) as f64
            );
        }
        {
            // 파일 내용만 본 이번 구간: 전송이 있었을 때만 남기고, 구간마다 비웁니다.
            let b = std::mem::replace(&mut *self.bulk.lock().unwrap(), Bulk::EMPTY);
            // 무선으로 멀쩡히 받고도 MAC이 버린 것 (FIFO가 USB보다 빨리 참).
            let mac = radio
                .rtw88()
                .and_then(|c| c.rx_reports(&[RXERR_MAC_DROP, RXERR_VHT_OK]).ok())
                .unwrap_or_default();
            let at = |i: usize| mac.get(i).copied().unwrap_or(0);
            if b.frames >= 200 {
                log!("{}", b.summary());
            }
            if at(0) > 0 || b.frames >= 200 {
                log!("MAC rx report: dropped {} after receiving them | VHT ok {}", at(0), at(1));
            }
        }
        {
            let mut c = self.crc.lock().unwrap();
            if c.iter().map(|(ok, bad)| ok + bad).sum::<u64>() >= 100 {
                let f = |(ok, bad): (u64, u64)| format!("ok {ok} failed {bad} ({:.0}%)", 100.0 * bad as f64 / (ok + bad).max(1) as f64);
                log!("PHY CRC, all senders: VHT {} | HT {} | OFDM {}", f(c[2]), f(c[1]), f(c[0]));
            }
            *c = Default::default();
        }
    }
}

fn ro_key(ta: &[u8], tid: u8) -> u64 {
    ta[..6].iter().fold(0u64, |k, &b| (k << 8) | b as u64) << 4 | tid as u64
}

impl Relay {
    fn on_c2h(&self, m: C2h) {
        if let C2h::TxReport { sn, ok, raw } = m {
            if self.reports_logged.fetch_add(1, Relaxed) < 4 {
                log!("tx report {sn}: {} (raw {:02x?})", if ok { "acked" } else { "FAILED" }, raw);
            }
            let Some((ampdu, _)) = self.reports.lock().unwrap().remove(&sn) else {
                return;
            };
            let s = &self.stats;
            match (ampdu, ok) {
                (true, true) => &s.rpt_ampdu_ok,
                (true, false) => &s.rpt_ampdu_fail,
                (false, true) => &s.rpt_single_ok,
                (false, false) => &s.rpt_single_fail,
            }
            .fetch_add(1, Relaxed);
        }
    }

    /// 멈춘 창에 대한 Block Ack Request: 하드웨어가 A-MPDU의 MPDU 하나를 포기하면 (상대가 자리를
    /// 비웠을 때 등) 상대의 재정렬 버퍼가 그것을 기다리며 뒤의 모든 것을 붙잡아 TCP 흐름이
    /// 멈춥니다. 멈춤이란: 우리가 그 시작 너머의 프레임을 보내는 동안 상대가 우리 A-MPDU에 (30 ms
    /// 안의 Block Ack으로) 50 ms 동안 같은 창 시작으로 답하는 것. 그러면 구멍과 그 뒤에 상대가 이미
    /// 가진 프레임들 너머로 창을 옮기는 BAR를 보내고, 잃은 것은 TCP가 다시 보냅니다.
    fn stalled_windows(&self) -> Vec<([u8; 6], u16)> {
        if !self.sw_bar {
            return Vec::new();
        }
        let now = Instant::now();
        let ba = self.ba.lock().unwrap();
        let heard = self.heard.lock().unwrap();
        let mut views = self.ba_view.lock().unwrap();
        let mut out = Vec::new();
        for (mac, v) in views.iter_mut() {
            let Some(p) = ba.0.iter().find(|p| p.used && p.up && p.mac == *mac) else {
                continue;
            };
            // 상대가 우리 채널에 있을 때만: 최근 3 ms 안에 들렸습니다.
            if heard.get(mac).is_none_or(|t| now - *t > Duration::from_millis(3)) {
                continue;
            }
            let sending = p.last_agg_at.is_some_and(|t| now - t < Duration::from_millis(100));
            let ahead = p.last_agg_sn.wrapping_sub(v.ssn) & 0xfff;
            let answering = now - v.last < Duration::from_millis(30);
            let stalled = sending && answering && now - v.moved > Duration::from_millis(50);
            let again = v.bar_at.is_none_or(|t| now - t > Duration::from_millis(100));
            if (2..2048).contains(&ahead) && stalled && again {
                // 창 시작의 구멍과 그 뒤에 상대가 이미 가진 흐름 너머로.
                let run = (v.bitmap >> 1).trailing_ones() as u16;
                let to = (v.ssn + 1 + run).min(v.ssn + ahead) & 0xfff;
                v.bar_at = Some(now);
                out.push((*mac, to));
            }
        }
        out
    }
}

/// 마지막 데이터 프레임 뒤 TCP ACK가 여기까지 오는 데 걸린 시간을 적습니다 (20 ms 안: 전송 중).
fn ack_latency(relay: &Relay, into: &Mutex<Vec<u32>>) {
    let d = now_ns() - relay.last_data_ns.load(Relaxed);
    if (0..20_000_000).contains(&d) {
        let mut v = into.lock().unwrap();
        if v.len() < 200_000 {
            v.push((d / 1000) as u32);
        }
    }
}

/// TID 0에 대한 압축 Block Ack Request (802.11 헤더, BAR 제어, 시작 순서 번호).
fn bar_frame(ra: &[u8; 6], ta: &[u8; 6], ssn: u16) -> Vec<u8> {
    let mut f = Vec::with_capacity(20);
    f.extend_from_slice(&[0x84, 0x00, 0x00, 0x00]);
    f.extend_from_slice(ra);
    f.extend_from_slice(ta);
    f.extend_from_slice(&0x0004u16.to_le_bytes()); // 보통 ACK, 압축 비트맵, TID 0
    f.extend_from_slice(&(ssn << 4).to_le_bytes());
    f
}

/// AWDL 데이터 프레임 안의 TCP 헤더 (802.11 헤더, Apple OUI의 LLC/SNAP, 6바이트 AWDL 데이터 헤더,
/// ethertype, 그다음 IPv6). 있으면.
fn awdl_tcp(f: &[u8]) -> Option<usize> {
    let len = f.len();
    if len < 26 || f[0] & 0x0c != 0x08 {
        return None;
    }
    let mut h = if f[0] & 0x80 != 0 {
        26 + if f[1] & 0x80 != 0 { 4 } else { 0 }
    } else {
        24
    };
    if f[0] & 0x80 != 0 && f[24] & 0x80 != 0 {
        h += 14; // A-MSDU 서브프레임 헤더
    }
    let (mut et, mut ip) = (h + 6, h + 8);
    if len > h + 6 && f[h..h + 6] == [0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2] {
        et = h + 14;
        ip = h + 16;
    }
    if len < ip + 40 + 20 || f[et] != 0x86 || f[et + 1] != 0xdd || f[ip + 6] != 6 {
        return None;
    }
    Some(ip + 40)
}

/// 순수 누적 TCP ACK (페이로드도 SYN/FIN/RST도 없음): 그 ACK 번호.
fn pure_ack(f: &[u8]) -> Option<u32> {
    let tcp = awdl_tcp(f)?;
    let ip = tcp - 40;
    let plen = ((f[ip + 4] as usize) << 8 | f[ip + 5] as usize).checked_sub(((f[tcp + 12] >> 4) as usize) * 4)?;
    if plen != 0 || f[tcp + 13] & 0x07 != 0 || f[tcp + 13] & 0x10 == 0 {
        return None;
    }
    Some(u32::from_be_bytes(f[tcp + 8..tcp + 12].try_into().unwrap()))
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let v: Vec<u8> = s.split(':').map(|x| u8::from_str_radix(x, 16)).collect::<Result<_, _>>().ok()?;
    let m: [u8; 6] = v.try_into().ok()?;
    (m[0] & 1 == 0).then_some(m)
}

fn usage() -> io::Error {
    eprintln!("usage: usbfwd --mac aa:bb:cc:dd:ee:ff [--port 7770] [--peer 127.0.0.1:7771] [--channel 149] [--bw 80] [--stop-file PATH]");
    io::Error::other("bad arguments")
}

fn sock_tune(sock: &UdpSocket) {
    use windows::Win32::Networking::WinSock::{SO_RCVBUF, SO_SNDBUF, SOCKET, SOL_SOCKET, WSAIoctl, setsockopt};
    let s = SOCKET(sock.as_raw_socket() as usize);
    let size = (8i32 << 20).to_ne_bytes();
    // SAFETY: 우리 소켓에 대한 평범한 소켓 옵션.
    unsafe {
        let _ = setsockopt(s, SOL_SOCKET, SO_RCVBUF, Some(&size));
        let _ = setsockopt(s, SOL_SOCKET, SO_SNDBUF, Some(&size));
        // ICMP 포트 도달 불가 (호스트가 아직 안 들음)가 다음 읽기를 망치면 안 됩니다.
        const SIO_UDP_CONNRESET: u32 = 0x9800_000c;
        let off = 0u32;
        let mut got = 0u32;
        let _ = WSAIoctl(
            s,
            SIO_UDP_CONNRESET,
            Some(&off as *const u32 as *const _),
            4,
            None,
            0,
            &mut got,
            None,
            None,
        );
    }
}

fn pending(sock: &UdpSocket) -> bool {
    use windows::Win32::Networking::WinSock::{FIONREAD, SOCKET, ioctlsocket};
    let mut n = 0u32;
    // SAFETY: 우리 소켓에 대한 FIONREAD.
    unsafe { ioctlsocket(SOCKET(sock.as_raw_socket() as usize), FIONREAD, &mut n) == 0 && n > 0 }
}

/// 부르는 스레드를 평범한 일보다 앞에: 무선의 프레임과 거기 실리는 TCP ACK가 1 ms씩 이것을
/// 기다립니다.
fn raise_thread() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
    // SAFETY: 우리 스레드 자신의 우선순위.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
}

fn main() {
    // 드라이버 자신의 진행 줄(올리기, 보정)도 우리 것처럼 stderr로 갑니다.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_level(false)
        .without_time()
        .with_ansi(false)
        .init();
    if let Err(e) = run() {
        log!("{e}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    // Windows는 타이머 틱 단위(기본 15.6 ms)로 잠듭니다. 올리기와 ACK 간격 조절기는 둘 다 µs 단위로
    // 기다립니다.
    // SAFETY: 평범한 호출.
    unsafe { windows::Win32::Media::timeBeginPeriod(1) };
    {
        use windows::Win32::System::Threading::{GetCurrentProcess, HIGH_PRIORITY_CLASS, SetPriorityClass};
        // SAFETY: 우리 프로세스 자신의 우선순위 클래스.
        let _ = unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) };
    }
    let mut args = std::env::args().skip(1);
    let (mut mac, mut port, mut peer, mut channel, mut bw, mut stop_file) = (None, 7770u16, "127.0.0.1:7771".to_string(), 149u8, 80u16, None);
    while let Some(a) = args.next() {
        let v = args.next().ok_or_else(usage)?;
        match a.as_str() {
            "--mac" => mac = parse_mac(&v),
            "--port" => port = v.parse().map_err(|_| usage())?,
            "--peer" => peer = v,
            "--channel" => channel = v.parse().map_err(|_| usage())?,
            "--bw" => bw = v.parse().map_err(|_| usage())?,
            "--stop-file" => stop_file = Some(std::path::PathBuf::from(v)),
            _ => return Err(usage()),
        }
    }
    let ours = mac.ok_or_else(|| {
        log!("--mac must be a unicast aa:bb:cc:dd:ee:ff");
        usage()
    })?;
    // 어댑터: 환경 변수로 고른 것, 아니면 꽂혀 있는 RTL8912AU, 아니면 T3U Nano.
    let usb = match (env_num("USBFWD_VID"), env_num("USBFWD_PID")) {
        (Some(vid), Some(pid)) => UsbId { vid, pid },
        _ if Usb::present(UsbId::RTL8912AU) => UsbId::RTL8912AU,
        _ => UsbId::T3U_NANO,
    };
    let rtw89 = rtl8922a::USB_IDS.contains(&usb);
    if !([20, 40, 80].contains(&bw) || rtw89 && bw == 160) {
        return Err(usage());
    }
    let bw_code = match bw {
        160 => 3,
        80 => 2,
        40 => 1,
        _ => 0,
    };
    let vht = parse_vht("USBFWD_DATA_VHT");
    let ack_vht = parse_vht("USBFWD_ACK_VHT").or(vht);
    let far_vht = parse_vht("USBFWD_FAR_VHT").unwrap_or(Vht::new(2, 5));
    // RTL8912AU는 신호가 센 상대에게 데이터를 HE 2SS MCS11로: 1.26 GB 영상의 PC→iPhone이 VHT
    // 2SS MCS7의 35.8 MB/s에서 50.2 MB/s로 (Block Ack이 빠뜨린 MPDU 없음).
    let he_data = match std::env::var("USBFWD_DATA_HE") {
        Ok(s) => {
            let p: Vec<u8> = s.split('/').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() >= 2 && (1..=2).contains(&p[0]) && p[1] <= 11).then(|| (p[0], p[1]))
        }
        Err(_) => Some((2, 11)),
    }
    .filter(|_| rtw89);
    let reorder_ms = env_num::<u64>("USBFWD_REORDER_MS").unwrap_or(80);
    // 순수 TCP ACK를 상대가 이만큼 조용할 때까지, 길어야 hold만큼 모읍니다. RTL8912AU는 상대의 PPDU가
    // 끝날 때 내보내므로(USBFWD_ACK_EOF_US) 이 둘은 그 끝을 놓쳤을 때만 씁니다: 짧으면 USB로 PPDU가
    // 나뉘어 오는 틈에도 ACK가 나가 PPDU마다 ACK 프레임이 셋쯤 됩니다. 1.26 GB 영상을 받는 데 100/500 us가
    // 44.0 MB/s, 300/1500이 47.1, 여기에 PPDU 끝을 더하면 51~57. T3U의 200/2000은 그대로.
    let ack_quiet_us = env_num::<i64>("USBFWD_ACK_QUIET_US").unwrap_or(if rtw89 { 300 } else { 200 }).max(0);
    let ack_hold_us = env_num::<u64>("USBFWD_ACK_HOLD_US").unwrap_or(if rtw89 { 1500 } else { 2000 }).max(100);
    let tx_ba = env_num::<u32>("USBFWD_TX_BA").unwrap_or(0) != 0;
    let ack_vo = env_num::<u32>("USBFWD_ACK_VO").unwrap_or(0) != 0;
    let bfee = env_num::<u32>("USBFWD_BFEE").unwrap_or(0) != 0;

    let sock = UdpSocket::bind(SocketAddrV4::new([127, 0, 0, 1].into(), port))
        .map_err(|e| io::Error::other(format!("cannot bind UDP 127.0.0.1:{port} ({e})")))?;
    sock_tune(&sock);
    sock.set_read_timeout(Some(Duration::from_millis(1)))?;
    let peer_addr: SocketAddr = peer.parse().map_err(|_| usage())?;

    let ampdu_spec = std::env::var("USBFWD_AMPDU").unwrap_or_default();
    let ampdu_field = |i: usize| ampdu_spec.split('/').nth(i).and_then(|m| m.trim().parse::<u8>().ok());
    let ampdu_max = ampdu_field(1).unwrap_or(16);
    let ampdu_density = ampdu_field(2).unwrap_or(6).min(7);
    let country = std::env::var("USBFWD_COUNTRY")
        .ok()
        .and_then(|s| match s.trim().to_ascii_uppercase().as_bytes() {
            &[a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => Some([a, b]),
            _ => None,
        });
    // RTL8912AU: USB 전송 하나에 담는 프레임 수/KB (1이면 하나씩).
    let agg_spec = std::env::var("USBFWD_TX_AGG").unwrap_or_default();
    let agg_field = |i: usize| agg_spec.split('/').nth(i).and_then(|m| m.trim().parse::<usize>().ok());
    let usb_tx_agg = wirdrop_radio::rtl8922a::TxAgg::new(agg_field(0).unwrap_or(16), agg_field(1).unwrap_or(20) << 10);
    let cfg = Config {
        usb,
        channel,
        bw_mhz: bw,
        mac: ours,
        tx_power: env_num("USBFWD_TX_POWER"),
        // RTL8912AU: 32. AWDL 상대는 슬롯 사이에 수십 ms씩 채널을 비우는데, 그때 칩에 쌓인
        // 프레임이 8번 재전송을 금방 다 쓰고 버려져 TCP가 속도를 반으로 줄였습니다 (1.26 GB를
        // iPhone에 보내며 빠른 재전송 70여 번, 23.2 MB/s). 32번이면 백오프가 길어져 상대가 돌아올
        // 때까지 버티고 (빠른 재전송 1번), 같은 설정에서 35.8 MB/s.
        retry_limit: env_num("USBFWD_RETRY_LIMIT").unwrap_or(if rtw89 { 32 } else { 8 }),
        igi: env_num("USBFWD_IGI"),
        ampdu_max,
        ampdu_density,
        country,
        usb_tx_agg,
        ..Config::default()
    };
    let mut radio = Radio::open(cfg.clone())?;
    log!("adapter {} ({:04x}:{:04x})", radio.name(), usb.vid, usb.pid);
    let relay = Relay {
        ours,
        sock,
        peer: Mutex::new(Some(peer_addr)),
        last_rssi: AtomicI64::new(-128),
        last_rx_ns: AtomicI64::new(0),
        eof_ns: AtomicI64::new(0),
        stats: Stats::default(),
        tx_ba,
        ack_vo,
        far_vht,
        far_rssi: env_num::<f32>("USBFWD_FAR_RSSI").unwrap_or(50.0),
        rate_step: Mutex::new(HashMap::new()),
        ba: Mutex::new(BaTable::default()),
        reorder_ms,
        ro: Mutex::new(HashMap::new()),
        bfee,
        bfer_want: Mutex::new(None),
        bfer_dirty: AtomicBool::new(false),
        rate_hist: Mutex::new(HashMap::new()),
        bw_hist: Default::default(),
        path: Mutex::new(PathStats::default()),
        last_ppdu: Mutex::new(-1),
        ndpa_logged: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        peers: Mutex::new(HashMap::new()),
        data_peers: Mutex::new(HashMap::new()),
        reports: Mutex::new(HashMap::new()),
        reports_logged: AtomicU64::new(0),
        ba_view: Mutex::new(HashMap::new()),
        sw_bar: env_num::<u32>("USBFWD_SW_BAR").unwrap_or(1) != 0,
        heard: Mutex::new(HashMap::new()),
        data_rssi: Mutex::new(HashMap::new()),
        presence: Mutex::new(Presence::default()),
        crc: Mutex::new(Default::default()),
        last_data_ns: AtomicI64::new(0),
        ack_host_us: Mutex::new(Vec::new()),
        ack_air_us: Mutex::new(Vec::new()),
        bulk: Mutex::new(Bulk::EMPTY),
        prev_ppdu: AtomicU8::new(0),
        ppdu_bulk: AtomicBool::new(false),
        answers: Mutex::new(AnswerWindow::new()),
        check_answers: radio.rtw89().is_some(),
        answers_paused: Mutex::new(None),
        restart: AtomicBool::new(false),
        ack_quiet_us,
    };
    if tx_ba {
        if radio.rtw89().is_some() {
            log!(
                "Block Ack mode on: A-MPDU of up to {ampdu_max} MPDUs to peers that agree, USB transfers of up to {} frames / {} KB",
                usb_tx_agg.frames,
                usb_tx_agg.bytes >> 10
            );
        } else {
            log!("Block Ack mode on: A-MPDU of up to {ampdu_max} MPDUs to peers that agree, USB TX aggregation 3");
        }
    }
    // 백오프 없는 AIFSN 2: 최선 노력 송신자는 AIFSN 3에 백오프까지 기다립니다.
    let voice_access = |radio: &Radio| {
        if ack_vo {
            match radio.set_voice_access(34, 0, 2) {
                Ok(()) => log!("pure TCP ACKs through the voice queue: 34 us, no backoff"),
                Err(e) => log!("voice queue access: {e}"),
            }
        }
    };
    voice_access(&radio);
    let fmt_vht = |v: Option<Vht>| v.map(Vht::name).unwrap_or_else(|| "off".into());
    log!(
        "ch {channel}/{bw} MHz, mac {}, ack responder armed, retries {}, data {}, ack {}, far {}, reorder {reorder_ms} ms, ack pacing {ack_quiet_us}/{ack_hold_us} us, UDP 127.0.0.1:{port} <-> {peer}",
        mac_str(&ours),
        radio.cfg.retry_limit,
        he_data.map_or_else(|| fmt_vht(vht), |(n, m)| format!("HE {n}/{m}")),
        fmt_vht(ack_vht),
        far_vht.name()
    );

    // 다시 올린 때들: 2분에 다섯 번이면 칩이 아니라 먼 상대의 약한 링크로 보고 2분 동안 보지 않습니다.
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        std::thread::scope(|s| {
            let rx_thread = s.spawn(|| {
                raise_thread();
                let mut rx = match radio.receiver(8) {
                    Ok(rx) => rx,
                    Err(e) => {
                        log!("{e}");
                        relay.stop.store(true, Relaxed);
                        return;
                    }
                };
                while !relay.stop.load(Relaxed) {
                    if let Err(e) = rx.next(|buf| radio.parse_with_c2h(buf, |f| relay.on_rx(&f), |m| relay.on_c2h(m))) {
                        if !relay.stop.load(Relaxed) {
                            log!("receive: {e}");
                        }
                        relay.stop.store(true, Relaxed);
                    }
                }
            });
            s.spawn(|| watchdog(&radio, &relay));
            raise_thread();
            tx_loop(&radio, &relay, stop_file.clone(), bw_code, vht, ack_vht, he_data, ack_hold_us);
            relay.stop.store(true, Relaxed);
            radio.abort_rx();
            let _ = rx_thread.join();
        });
        if !relay.restart.swap(false, Relaxed) {
            break;
        }
        restarts.retain(|t| t.elapsed() < Duration::from_secs(120));
        restarts.push(Instant::now());
        if restarts.len() >= 5 {
            log!("brought the adapter up again 5 times in 2 minutes: it's the link, not the adapter; not checking for 2 minutes");
            *relay.answers_paused.lock().unwrap() = Some(Instant::now() + Duration::from_secs(120));
            restarts.clear();
        }
        let t = Instant::now();
        // 상대들은 우리를 그대로 기억하고 곧바로 RTS나 A-MPDU를 보내므로, 앞의 주소 CAM과 Block Ack
        // 합의를 새 칩에 다시 씁니다.
        let peers = radio.peers();
        radio.close();
        radio = Radio::open(cfg.clone())?;
        radio.restore_peers(peers);
        voice_access(&radio);
        *relay.answers.lock().unwrap() = AnswerWindow::new();
        // 그동안 쌓였던 프레임이 재전송으로 몰려옵니다.
        let mut paused = relay.answers_paused.lock().unwrap();
        let hold = Instant::now() + Duration::from_secs(4);
        if paused.is_none_or(|t| t < hold) {
            *paused = Some(hold);
        }
        drop(paused);
        relay.stop.store(false, Relaxed);
        log!("adapter up again in {} ms", t.elapsed().as_millis());
    }
    relay.print_stats(&radio);
    radio.close();
    log!("stopped");
    Ok(())
}

/// 2초마다 도는 watchdog: 최근 10초에 들린 가장 약한 AWDL 상대로 한계를 둔 동적 초기 이득
/// (없으면 커버리지 모드). RTL8822B는 그것을 따르는 EDCCA 문턱과 송신 출력 추적도 합니다.
fn watchdog(radio: &Radio, relay: &Relay) {
    let mut dm = Dm::default();
    dm.edcca = env_num::<u32>("USBFWD_EDCCA").unwrap_or(0) != 0;
    dm.pwr_track = env_num::<u32>("USBFWD_PWRTRACK").unwrap_or(0) != 0;
    let tx_power = radio.cfg.tx_power.unwrap_or(0x3f);
    let mut runs = 0u32;
    let mut logged_igi = 0u8;
    while !relay.stop.load(Relaxed) {
        for _ in 0..20 {
            if relay.stop.load(Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let min_rssi = {
            // 전송하는 상대 가운데 가장 약한 것 (2초에 데이터 프레임 수십 개), 없으면 최근 데이터를
            // 보낸 가장 약한 것, 그것도 없으면 들린 가장 약한 AWDL 상대: 약한 쉬는 기기에 맞춘
            // 이득이면 팔 길이 거리의 폰이 수신기를 과하게 몰아붙입니다.
            let mut data = relay.data_rssi.lock().unwrap();
            data.retain(|_, (_, t, _)| t.elapsed() < Duration::from_secs(10));
            let mut peers = relay.peers.lock().unwrap();
            peers.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(10));
            let busy = data.values().filter(|(_, _, n)| *n >= 20).map(|(r, _, _)| *r as u8).min();
            let linked = data.values().map(|(r, _, _)| *r as u8).min();
            data.values_mut().for_each(|e| e.2 = 0);
            busy.or(linked).or_else(|| peers.values().map(|(r, _)| *r as u8).min())
        };
        if let Some(chip) = radio.rtw89() {
            if radio.cfg.igi.is_none() {
                match chip.watchdog(radio.cfg.bw_mhz, min_rssi) {
                    Ok(rep) => {
                        runs += 1;
                        if runs % 15 == 1 || rep.pd.abs_diff(logged_igi) >= 2 {
                            logged_igi = rep.pd;
                            log!(
                                "watchdog: packet detect floor {} (weakest peer RSSI {})",
                                if rep.pd == 0 {
                                    "off".to_string()
                                } else {
                                    format!("{} dBm", rep.floor_dbm)
                                },
                                min_rssi.map(|r| r.to_string()).unwrap_or_else(|| "-".into())
                            );
                        }
                    }
                    Err(e) => log!("watchdog: {e}"),
                }
            }
            continue;
        }
        let Some(chip) = radio.rtw88() else { continue };
        if radio.cfg.igi.is_some() {
            if let Ok(c) = chip.counters() {
                relay.add_crc(&c);
            }
            continue;
        }
        match chip.watchdog(&mut dm, min_rssi, tx_power) {
            Ok(rep) => {
                relay.add_crc(&rep.counters);
                runs += 1;
                if rep.iqk || runs % 15 == 1 || rep.igi.abs_diff(logged_igi) >= 4 {
                    logged_igi = rep.igi;
                    log!(
                        "watchdog: false alarms {} | IGI 0x{:02x} (weakest peer RSSI {}) | thermal {} (efuse {}){}",
                        rep.false_alarms,
                        rep.igi,
                        min_rssi.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                        rep.thermal.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                        chip.efuse.thermal_meter,
                        if rep.iqk { ", IQK again" } else { "" }
                    );
                }
            }
            Err(e) => log!("watchdog: {e}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tx_loop(
    radio: &Radio,
    relay: &Relay,
    stop_file: Option<std::path::PathBuf>,
    bw_code: u8,
    vht: Option<Vht>,
    ack_vht: Option<Vht>,
    he_data: Option<(u8, u8)>,
    ack_hold_us: u64,
) {
    const BATCH: usize = 32;
    let mut buf = vec![0u8; 16384];
    // 프레임, 속도, A-MPDU, ACK 없음, 송신 보고 번호
    type Queued = (Vec<u8>, TxRate, bool, bool, Option<u8>, bool);
    let mut pool: Vec<Queued> = Vec::with_capacity(BATCH);
    // 호스트가 연달아 보낸 같은 상대의 데이터 프레임을 이 크기(MPDU 바이트)까지 A-MSDU 하나로 합칩니다
    // (0 = 안 합침): 칩이 USB 전송과 A-MPDU에 담는 프레임 수가 한도라, 하나에 MSDU 4~5개를 실으면 PC→iPhone이
    // 46.8에서 약 73 MB/s가 됩니다.
    let amsdu_max = env_num::<usize>("USBFWD_TX_AMSDU")
        .unwrap_or(if radio.rtw89().is_some() { 7935 } else { 0 })
        .min(7935);
    // 잡아 둔 TCP ACK가 있으면 recv_from(1 ms 시간 제한이 실제로는 약 1.5 ms)으로 잠들지 않고 들어온 것과
    // 내보낼 때를 계속 봅니다.
    let ack_poll = env_num::<u32>("USBFWD_ACK_POLL").unwrap_or(1) != 0;
    // 합치는 중인 A-MSDU: 프레임, 받는 쪽, MSDU 수, 속도, ACK 없음.
    type Merging = (Vec<u8>, [u8; 6], usize, u8, bool);
    let mut merge: Option<Merging> = None;
    // 합친 것을 보낼 프레임으로: 하나뿐이면 원래 모양으로.
    let merged = |m: Merging| -> (Vec<u8>, u8, bool) {
        let (mut f, _, k, rate, noack) = m;
        if k == 1 {
            let mut d = Vec::with_capacity(f.len() - 14);
            d.extend_from_slice(&f[..26]);
            d[24] &= !0x80;
            d.extend_from_slice(&f[26 + 14..]);
            return (d, rate, noack);
        }
        relay.stats.tx_amsdu.fetch_add(1, Relaxed);
        relay.stats.tx_amsdu_msdus.fetch_add(k as u64, Relaxed);
        f.shrink_to_fit();
        (f, rate, noack)
    };
    // 송신 보고는 RTL8822B 펌웨어만 답합니다.
    let report_every = if radio.has_tx_reports() {
        env_num::<u32>("USBFWD_TX_REPORT").unwrap_or(8)
    } else {
        0
    };
    let (rpt_count, rpt_sn) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u8));
    let mut last = Instant::now();
    let mut last_stop = last;
    let mut last_bf = last;
    let mut bfer_armed: Option<[u8; 6]> = None;
    // 조용한 순간을 기다리는 ACK. 보내기는 이 스레드만 합니다.
    let mut pend: Option<(Vec<u8>, u8, bool, u32)> = None;
    let mut pend_since = Instant::now();
    // 상대의 PPDU가 끝나고 이만큼 지나면 잡아 둔 ACK를 내보냅니다 (0 = 안 봄): 그 PPDU의 데이터에 대한
    // 호스트의 ACK(중앙값 약 35 us 뒤)가 다 나왔을 때, PPDU마다 한 번만.
    let ack_eof = env_num::<i64>("USBFWD_ACK_EOF_US")
        .unwrap_or(if radio.rtw89().is_some() { 60 } else { 0 })
        .max(0)
        * 1000;
    if ack_eof > 0 {
        log!("TCP ACKs let go {} us after a PPDU ends", ack_eof / 1000);
    }
    // 마지막으로 ACK를 내보낸 시각 (now_ns): 그 뒤에 끝난 PPDU만 셉니다.
    let mut released_ns = 0i64;
    let release = |pend_since: Instant, released_ns: i64| -> bool {
        let now = now_ns();
        let eof = relay.eof_ns.load(Relaxed);
        (ack_eof > 0 && eof > released_ns && now - eof >= ack_eof)
            || now - relay.last_rx_ns.load(Relaxed) >= relay.ack_quiet_us * 1000
            || pend_since.elapsed() >= Duration::from_micros(ack_hold_us)
    };
    let flush = |pool: &mut Vec<Queued>| {
        if pool.is_empty() {
            return;
        }
        let frames: Vec<TxFrame> = pool
            .iter()
            .map(|(d, r, a, n, rp, v)| TxFrame {
                data: d,
                rate: *r,
                ampdu: *a,
                no_ack: *n,
                report: *rp,
                voice: *v,
            })
            .collect();
        let n = frames.len() as u64;
        match radio.send(&frames) {
            Ok(ok) => {
                relay.stats.tx.fetch_add(ok as u64, Relaxed);
                relay.stats.tx_fail.fetch_add(n - ok as u64, Relaxed);
            }
            Err(e) => {
                relay.stats.tx_fail.fetch_add(n, Relaxed);
                log!("send: {e}");
            }
        }
        pool.clear();
    };
    // 순수 TCP ACK는 Block Ack 세션 밖에서 평범한 데이터로 보냅니다: 세션 안이면 반이중 링크가 잃은
    // ACK 하나가 뒤의 모든 ACK를 상대의 재정렬 버퍼에 붙잡아 둡니다.
    let air = |pool: &mut Vec<Queued>, f: &[u8], rate: u8, noack: bool, v: Option<Vht>, pure: bool| {
        if pool.len() == BATCH {
            flush(pool);
        }
        let unicast_data = f.len() >= 24 && f[0] & 0x0c == 0x08 && f[4] & 1 == 0;
        let mut d = if relay.tx_ba && pure && f.len() >= 26 && f[0] == 0x88 && f[24] & 0x80 == 0 {
            let mut d = Vec::with_capacity(f.len() - 2);
            d.extend_from_slice(&f[..24]);
            d[0] = 0x08; // QoS 데이터 -> 데이터, QoS 제어 필드 없이
            d.extend_from_slice(&f[26..]);
            d
        } else {
            f.to_vec()
        };
        let use_vht = v.filter(|_| unicast_data).map(|v| relay.rate_for(&f[4..10], v));
        let agg = relay.prepare_tx(&mut d) && use_vht.is_some();
        let r = match use_vht {
            // 낮춘 속도를 받지 않은(신호가 센) 상대에게 가는 데이터는 HE로.
            Some(rv) if !pure && v.is_some_and(|v| (v.nss, v.mcs) == (rv.nss, rv.mcs)) && he_data.is_some() => {
                let (nss, mcs) = he_data.unwrap_or((2, 7));
                TxRate::he(nss, mcs, bw_code, true, true)
            }
            Some(v) => TxRate::vht(v.nss, v.mcs, bw_code, v.sgi, v.ldpc),
            None => TxRate::legacy(rate),
        };
        if use_vht.is_some() {
            relay.stats.tx_vht.fetch_add(1, Relaxed);
        }
        if agg {
            relay.stats.tx_agg.fetch_add(1, Relaxed);
        }
        // N번째 유니캐스트 데이터 프레임마다 확인되었는지 펌웨어에 묻습니다.
        let mut report = None;
        if unicast_data && report_every > 0 {
            rpt_count.set(rpt_count.get() + 1);
            if rpt_count.get() % report_every == 0 {
                let sn = (rpt_sn.get() + 1) & 0x3f;
                rpt_sn.set(sn);
                relay.reports.lock().unwrap().insert(sn, (agg, Instant::now()));
                report = Some(sn);
            }
        }
        pool.push((d, r, agg, noack, report, pure && relay.ack_vo));
    };
    while !relay.stop.load(Relaxed) {
        let got = if ack_poll && pend.is_some() {
            loop {
                if pending(&relay.sock) {
                    break relay.sock.recv_from(&mut buf);
                }
                if release(pend_since, released_ns) {
                    break Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                std::thread::yield_now();
            }
        } else {
            relay.sock.recv_from(&mut buf)
        };
        let now = Instant::now();
        let ack_quiet_us = relay.ack_quiet_us;
        if relay.reorder_ms > 0 {
            relay.ro_tick(now);
        }
        for (peer, ssn) in relay.stalled_windows() {
            let n = relay.stats.bar_sent.fetch_add(1, Relaxed);
            if n < 5 || n.is_multiple_of(100) {
                log!("{}'s receive window stalled; Block Ack Request to sequence {ssn}", mac_str(&peer));
            }
            pool.push((bar_frame(&peer, &relay.ours, ssn), TxRate::legacy(48), false, false, None, false));
        }
        if let Some((d, rate, noack, _)) = pend.as_ref()
            && release(pend_since, released_ns)
        {
            relay.stats.ack_paced.fetch_add(1, Relaxed);
            let (d, rate, noack) = (d.clone(), *rate, *noack);
            pend = None;
            released_ns = now_ns();
            ack_latency(relay, &relay.ack_air_us);
            air(&mut pool, &d, rate, noack, ack_vht, true);
        }
        if now - last >= Duration::from_secs(10) {
            last = now;
            relay.print_stats(radio);
        }
        if now - last_stop >= Duration::from_millis(500) {
            last_stop = now;
            if stop_file.as_ref().is_some_and(|p| p.exists()) {
                break;
            }
        }
        if relay.bfee && relay.bfer_dirty.load(Relaxed) && now - last_bf >= Duration::from_secs(1) {
            last_bf = now;
            relay.bfer_dirty.store(false, Relaxed);
            let want = *relay.bfer_want.lock().unwrap();
            if let Some(m) = want
                && bfer_armed != Some(m)
            {
                let ok = radio.arm_beamformee(m).is_ok();
                bfer_armed = Some(m);
                log!("beamformee for {} {}", mac_str(&m), if ok { "armed" } else { "REFUSED" });
            }
        }
        match got {
            Ok((n, from)) => {
                *relay.peer.lock().unwrap() = Some(from);
                if n >= 4 + 10 && buf[0] == 1 {
                    let rate = buf[1];
                    let noack = buf[2] & 1 != 0;
                    let f = &buf[4..n];
                    let ack = (f[4] & 1 == 0).then(|| pure_ack(f)).flatten();
                    if ack.is_some() {
                        ack_latency(relay, &relay.ack_host_us);
                    }
                    match ack.filter(|_| ack_quiet_us > 0) {
                        Some(ackno) => {
                            // 누적: 더 앞선 쪽을 남깁니다.
                            let newer = pend.as_ref().is_none_or(|p| ackno.wrapping_sub(p.3) as i32 >= 0);
                            if pend.is_some() {
                                relay.stats.ack_merged.fetch_add(1, Relaxed);
                            } else {
                                pend_since = now;
                            }
                            if newer {
                                pend = Some((f.to_vec(), rate, noack, ackno));
                            }
                        }
                        None => {
                            if let Some((d, rate, noack, _)) = pend.take() {
                                relay.stats.ack_paced.fetch_add(1, Relaxed);
                                released_ns = now_ns();
                                ack_latency(relay, &relay.ack_air_us);
                                air(&mut pool, &d, rate, noack, ack_vht, true);
                            }
                            if ack.is_some() {
                                ack_latency(relay, &relay.ack_air_us);
                            }
                            let dst: [u8; 6] = f[4..10].try_into().unwrap();
                            // 순수 ACK가 아닌 큰 QoS 데이터(TID 0, +HTC 없음, 아직 A-MSDU 아님)를 A-MSDU를
                            // 받는 Block Ack 합의 안의 유니캐스트 상대에게.
                            let mergeable = amsdu_max > 0
                                && relay.tx_ba
                                && ack.is_none()
                                && f.len() >= BULK
                                && f[0] == 0x88
                                && f[1] & 0x80 == 0
                                && dst[0] & 1 == 0
                                && f[24] & 0x8f == 0
                                && relay.amsdu_ok(&dst);
                            if mergeable {
                                // 서브프레임: DA, SA, 길이(빅 엔디언), 페이로드, 다음 것 앞은 4바이트 경계.
                                let payload = &f[26..];
                                let fits = merge.as_ref().is_some_and(|(m, d, k, ..)| {
                                    *d == dst && *k < 8 && 26 + (m.len() - 26).next_multiple_of(4) + 14 + payload.len() <= amsdu_max
                                });
                                if fits {
                                    let m = merge.as_mut().unwrap();
                                    while !(m.0.len() - 26).is_multiple_of(4) {
                                        m.0.push(0);
                                    }
                                    m.0.extend_from_slice(&f[4..16]);
                                    m.0.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                                    m.0.extend_from_slice(payload);
                                    m.2 += 1;
                                } else {
                                    if let Some((d, r, na)) = merge.take().map(merged) {
                                        air(&mut pool, &d, r, na, vht, false);
                                    }
                                    let mut m = Vec::with_capacity(amsdu_max);
                                    m.extend_from_slice(&f[..26]);
                                    m[24] |= 0x80;
                                    m.extend_from_slice(&f[4..16]);
                                    m.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                                    m.extend_from_slice(payload);
                                    merge = Some((m, dst, 1, rate, noack));
                                }
                            } else {
                                if let Some((d, r, na)) = merge.take().map(merged) {
                                    air(&mut pool, &d, r, na, vht, false);
                                }
                                air(&mut pool, f, rate, noack, vht, false);
                            }
                        }
                    }
                }
                // 이미 기다리는 프레임은 이번 묶음에 합쳐 무선이 한꺼번에 받게 합니다.
                if pool.len() < BATCH && pending(&relay.sock) {
                    continue;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => {}
        }
        if let Some((d, r, na)) = merge.take().map(merged) {
            air(&mut pool, &d, r, na, vht, false);
        }
        flush(&mut pool);
    }
    flush(&mut pool);
}
