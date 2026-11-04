//! 임시 (로컬 실험): RTL8912AU(ipTIME BE6500UA)를 열고 전원을 켠 뒤 펌웨어를 내려받아 돌리고, MAC과
//! BB/RF를 초기화해 한 채널에 맞추고 RF를 보정한 다음 모든 프레임을 받아 셉니다.
//! 사용: be_fw [초] [대역:중심:주채널:폭] [crc] [nok] [follow]
//!   예) be_fw 5 5:155:149:80      be_fw 60 6:15:17:160
//!   crc: 깨진 프레임도 받기, nok: RF 보정 건너뛰기,
//!   follow: AWDL TLV 32(6 GHz 보조 일정)가 보이면 그 채널로 옮겨 듣기
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use wirdrop_radio::rtl8922a::{Band, Bw, Chan, Rtl8922a, rx};

fn parse_chan(s: &str) -> Option<Chan> {
    let p: Vec<u32> = s.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let band = match p.first()? {
        2 => Band::G2,
        5 => Band::G5,
        6 => Band::G6,
        _ => return None,
    };
    let bw = match p.get(3)? {
        20 => Bw::B20,
        40 => Bw::B40,
        80 => Bw::B80,
        160 => Bw::B160,
        _ => return None,
    };
    Some(Chan::new(band, *p.get(1)? as u8, *p.get(2)? as u8, bw))
}

/// rtw89 수신 속도 부호 (BE 세대: 10..8 방식, 7..5 NSS-1, 4..0 MCS).
fn rate_name(rate: u16) -> String {
    const LEGACY: [&str; 12] = ["1M", "2M", "5.5M", "11M", "6M", "9M", "12M", "18M", "24M", "36M", "48M", "54M"];
    let (mode, v) = ((rate >> 8) & 7, rate & 0xff);
    match mode {
        0 => LEGACY.get(v as usize).map_or(format!("legacy {v}"), |s| s.to_string()),
        1 => format!("HT MCS{v}"),
        m => {
            let name = ["", "", "VHT", "HE", "EHT"].get(m as usize).copied().unwrap_or("?");
            format!("{name} {}SS MCS{}", (v >> 5) + 1, v & 0x1f)
        }
    }
}

fn mac(a: &[u8]) -> String {
    a.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// AWDL 상대 하나에서 본 것.
#[derive(Default)]
struct Awdl {
    actions: usize,
    tlvs: BTreeSet<u8>,
    /// 마지막 TLV 32..35 값
    last: [Option<Vec<u8>>; 4],
}

#[derive(Default)]
struct Pair {
    frames: usize,
    bytes: usize,
    ampdu: usize,
    rates: BTreeMap<(u16, u8), usize>,
    per_sec: BTreeMap<u64, usize>,
}

/// (보낸 쪽, 받는 쪽) 주소 쌍.
type Link = ([u8; 6], [u8; 6]);
/// 개수와 속도 부호별 개수.
type RateCount = (usize, BTreeMap<u16, usize>);
/// (개수, RSSI 합, 경로 A 합, 경로 B 합)
type RssiSum = (usize, i64, i64, i64);

#[derive(Default)]
struct Stats {
    kinds: BTreeMap<u8, usize>,
    c2hs: BTreeMap<(u8, u8, u8), usize>,
    /// (802.11 종류, 하위 종류) 별 개수
    frames: BTreeMap<(u8, u8), usize>,
    /// 방식별 (속도 부호, 대역폭)
    rates: BTreeMap<(u16, u8), usize>,
    crc: usize,
    /// 데이터 프레임: (보낸 쪽 A2, 받는 쪽 A1)
    pairs: BTreeMap<([u8; 6], [u8; 6]), Pair>,
    /// Block Ack: (보낸 쪽, 받는 쪽) 별 (개수, 속도 부호)
    bas: BTreeMap<Link, RateCount>,
    ssids: BTreeMap<String, usize>,
    bytes: usize,
    /// PPDU 상태: (PHY 상태 종류, 속도 부호, 대역폭) 별 (개수, RSSI 합, 경로 A 합, 경로 B 합)
    ppdus: BTreeMap<(u8, u16, u8), RssiSum>,
    ppdu_invalid: usize,
    awdl: BTreeMap<[u8; 6], Awdl>,
    events: Vec<String>,
    /// 모든 데이터 프레임의 초당 바이트
    data_per_sec: BTreeMap<u64, usize>,
    /// TLV 32가 가리키는 6 GHz 채널
    six_ghz: Option<Chan>,
}

/// rtw89_phy_status_bitmap
fn physts_name(t: u8) -> &'static str {
    const N: [&str; 17] = [
        "td-fail",
        "brk-tx",
        "cca-spoof",
        "ofdm-brk",
        "cck-brk",
        "dl-mu-spoof",
        "HE-MU",
        "VHT-MU",
        "ul-tb-spoof",
        "rsvd9",
        "trig-base",
        "CCK",
        "OFDM",
        "HT",
        "VHT",
        "HE",
        "EHT",
    ];
    N.get(t as usize).copied().unwrap_or("?")
}

fn main() -> std::io::Result<()> {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let chan = std::env::args()
        .nth(2)
        .filter(|s| s.contains(':'))
        .map(|s| parse_chan(&s).expect("channel: band:center:primary:bw"))
        .unwrap_or(Chan::new(Band::G5, 155, 149, Bw::B80));
    let t = Instant::now();
    let mut chip = Rtl8922a::open()?;
    println!(
        "open: {} (cv {}) at {} ms",
        if chip.usb3 { "USB 3" } else { "USB 2" },
        chip.cv,
        t.elapsed().as_millis()
    );
    chip.power_on()?;
    let file = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/firmware/rtw8922a_fw-4.bin"))?;
    let v = chip.download_firmware(&file)?;
    println!("firmware {}.{}.{}.{} running at {} ms", v[0], v[1], v[2], v[3], t.elapsed().as_millis());
    chip.mac_init()?;
    chip.read_efuse_rf()?;
    chip.phy_init(&file)?;
    chip.dm_init(&file)?;
    chip.set_channel(&chan)?;
    chip.set_promisc(std::env::args().any(|a| a == "crc"))?;
    println!(
        "RFE {}, channel {chan:?} ({} MHz) at {} ms",
        chip.rfe,
        chan.freq(),
        t.elapsed().as_millis()
    );

    let stop = AtomicBool::new(false);
    let mut st = Stats::default();
    let calibrate = !std::env::args().any(|a| a == "nok");
    let follow = std::env::args().any(|a| a == "follow");
    let start = std::thread::scope(|s| {
        let mut reader = match chip.usb.reader(8, 64 * 1024) {
            Ok(r) => r,
            Err(e) => {
                println!("reader: {e}");
                return Instant::now();
            }
        };
        if calibrate {
            match chip.calibrate(&mut reader, &chan, true) {
                Ok(rs) => {
                    let ok = rs.iter().filter(|r| r.ok()).count();
                    let all: Vec<String> = rs.iter().map(|r| format!("{} {:?} {} ms", r.name, r.state, r.ms)).collect();
                    println!("RFK {ok}/{} ok: {}", rs.len(), all.join(", "));
                }
                Err(e) => println!("RFK: {e}"),
            }
        }
        println!("listening {secs} s from {} ms", t.elapsed().as_millis());
        let start = Instant::now();
        s.spawn(|| {
            std::thread::sleep(Duration::from_secs(secs));
            stop.store(true, Ordering::Relaxed);
            chip.usb.abort_in();
        });
        while !stop.load(Ordering::Relaxed) {
            let sec = start.elapsed().as_secs();
            let at = start.elapsed().as_secs_f64();
            let r = reader.next(|buf| {
                st.bytes += buf.len();
                rx::packets(buf, |d, data| count(&mut st, sec, at, d, data))
            });
            if r.is_err() {
                break;
            }
            if follow && let Some(c) = st.six_ghz.take() {
                let t = Instant::now();
                let r = chip.set_channel(&c).and_then(|_| chip.calibrate(&mut reader, &c, false));
                let ok = r.as_ref().map(|rs| rs.iter().filter(|r| r.ok()).count()).unwrap_or(0);
                st.events.push(format!(
                    "{at:.1}s -> {c:?} in {} ms, RFK ok {ok} ({:?})",
                    t.elapsed().as_millis(),
                    r.err()
                ));
                println!("{}", st.events.last().unwrap());
            }
        }
        start
    });
    let dur = start.elapsed().as_secs_f64();
    println!("{:.1} MB over USB in {dur:.1} s", st.bytes as f64 / 1e6);
    println!("packets by type: {:?}, C2H {:?}", st.kinds, st.c2hs);
    println!("802.11 (type, subtype): {:?}, CRC errors {}", st.frames, st.crc);
    let rates: Vec<String> = st.rates.iter().map(|((r, bw), n)| format!("{} bw{bw}: {n}", rate_name(*r))).collect();
    println!("rates: {}", rates.join(", "));
    let mut pairs: Vec<_> = st.pairs.iter().collect();
    pairs.sort_by_key(|a| std::cmp::Reverse(a.1.bytes));
    for ((a2, a1), p) in pairs.iter().take(6) {
        let rates: Vec<String> = p.rates.iter().map(|((r, bw), n)| format!("{} bw{bw} x{n}", rate_name(*r))).collect();
        println!(
            "data {} -> {}: {} frames, {:.2} MB ({:.1} MB/s avg), A-MPDU {}, {}",
            mac(a2),
            mac(a1),
            p.frames,
            p.bytes as f64 / 1e6,
            p.bytes as f64 / 1e6 / dur,
            p.ampdu,
            rates.join(", ")
        );
        if p.bytes > 1_000_000 {
            let line: Vec<String> = p.per_sec.iter().map(|(s, b)| format!("{s}s {:.1}", *b as f64 / 1e6)).collect();
            println!("  MB per second: {}", line.join(", "));
        }
    }
    let mut bas: Vec<_> = st.bas.iter().collect();
    bas.sort_by_key(|a| std::cmp::Reverse(a.1.0));
    for ((ta, ra), (n, rates)) in bas.iter().take(4) {
        let rates: Vec<String> = rates.iter().map(|(r, n)| format!("{} x{n}", rate_name(*r))).collect();
        println!("BA {} -> {}: {n} ({})", mac(ta), mac(ra), rates.join(", "));
    }
    println!("beacon SSIDs: {:?}", st.ssids);
    println!("AWDL peers:");
    for (a, p) in &st.awdl {
        println!("  {}: {} actions, TLVs {:?}", mac(a), p.actions, p.tlvs);
    }
    for e in st.events.iter().take(60) {
        println!("  {e}");
    }
    let line: Vec<String> = st
        .data_per_sec
        .iter()
        .filter(|(_, b)| **b > 100_000)
        .map(|(s, b)| format!("{s}s {:.1}", *b as f64 / 1e6))
        .collect();
    println!("all data MB per second (>0.1): {}", line.join(", "));
    let mut ppdus: Vec<_> = st.ppdus.iter().collect();
    ppdus.sort_by_key(|a| std::cmp::Reverse(a.1.0));
    println!(
        "PPDU status ({} invalid): kind, rate, bw: count, avg RSSI dBm (path A, path B)",
        st.ppdu_invalid
    );
    for ((k, r, bw), (n, rssi, a, b)) in ppdus.iter().take(16) {
        let n = *n as f64;
        println!(
            "  {:11} {:16} bw{bw}: {n:6}  {:6.1} ({:6.1}, {:6.1})",
            physts_name(*k),
            rate_name(*r),
            *rssi as f64 / n,
            *a as f64 / n,
            *b as f64 / n
        );
    }
    Ok(())
}

fn count(st: &mut Stats, sec: u64, at: f64, d: &rx::RxDesc, data: &[u8]) {
    *st.kinds.entry(d.kind).or_default() += 1;
    if d.kind == rx::RX_TYPE_C2H {
        if let Some(c) = rx::c2h(data) {
            *st.c2hs.entry((c.cat, c.class, c.func)).or_default() += 1;
        }
        return;
    }
    if d.kind == rx::RX_TYPE_PPDU_STAT {
        ppdu_status(st, d, data);
        return;
    }
    if d.crc_err {
        st.crc += 1;
        return;
    }
    if d.kind != rx::RX_TYPE_WIFI || data.len() < 10 {
        return;
    }
    *st.rates.entry((d.rate, d.bw)).or_default() += 1;
    let (ft, sub) = ((data[0] >> 2) & 3, data[0] >> 4);
    *st.frames.entry((ft, sub)).or_default() += 1;
    let a1: [u8; 6] = data[4..10].try_into().unwrap();
    if ft == 1 && sub == 9 && data.len() >= 16 {
        let e = st.bas.entry((data[10..16].try_into().unwrap(), a1)).or_default();
        e.0 += 1;
        *e.1.entry(d.rate).or_default() += 1;
    }
    if ft == 2 && data.len() >= 24 {
        *st.data_per_sec.entry(sec).or_default() += data.len();
        let p = st.pairs.entry((data[10..16].try_into().unwrap(), a1)).or_default();
        p.frames += 1;
        p.bytes += data.len();
        p.ampdu += d.ampdu as usize;
        *p.rates.entry((d.rate, d.bw)).or_default() += 1;
        *p.per_sec.entry(sec).or_default() += data.len();
    }
    if ft == 0 && sub == 13 {
        awdl(st, at, data);
    }
    // 비콘의 SSID (24 머리말 + 12 고정 필드 다음 첫 요소)
    if ft == 0 && sub == 8 && data.len() > 38 && data[36] == 0 {
        let n = data[37] as usize;
        if let Some(s) = data.get(38..38 + n) {
            *st.ssids.entry(String::from_utf8_lossy(s).into_owned()).or_default() += 1;
        }
    }
}

fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// AWDL 액션 프레임(분류 127, OUI 00:17:f2, 종류 8)의 TLV들. 고정 머리 16바이트 다음부터 (종류 1, 길이 2, 값).
fn awdl(st: &mut Stats, at: f64, data: &[u8]) {
    if data.len() < 40 || data[24] != 127 || data[25..28] != [0x00, 0x17, 0xf2] || data[28] != 8 {
        return;
    }
    let a2: [u8; 6] = data[10..16].try_into().unwrap();
    let mut events = Vec::new();
    let mut want = None;
    let e = st.awdl.entry(a2).or_default();
    e.actions += 1;
    let mut i = 40;
    while i + 3 <= data.len() {
        let (t, n) = (data[i], u16::from_le_bytes([data[i + 1], data[i + 2]]) as usize);
        let Some(v) = data.get(i + 3..i + 3 + n) else { break };
        e.tlvs.insert(t);
        if (32..=35).contains(&t) {
            let slot = &mut e.last[(t - 32) as usize];
            if slot.as_deref() != Some(v) {
                events.push(format!("{at:.1}s {} TLV {t}: {}", mac(&a2), hex(v)));
                *slot = Some(v.to_vec());
            }
            if t == 32 && v.len() >= 5 {
                want = six_ghz_chan(v[2], v[4]);
            }
        }
        i += 3 + n;
    }
    st.events.extend(events);
    if want.is_some() && !st.events.iter().any(|e| e.contains("-> Chan")) && st.six_ghz.is_none() {
        st.six_ghz = want;
    }
}

/// 운용 등급(131..134)과 주 채널로 6 GHz 채널.
fn six_ghz_chan(class: u8, ch: u8) -> Option<Chan> {
    let (bw, span) = match class {
        131 => (Bw::B20, 4),
        132 => (Bw::B40, 8),
        133 => (Bw::B80, 16),
        134 => (Bw::B160, 32),
        _ => return None,
    };
    let center = ((ch.checked_sub(1)?) / span) * span + 1 + (span - 4) / 2;
    Some(Chan::new(Band::G6, center, ch, bw))
}

/// PPDU 상태 (rtw89_core_rx_process_mac_ppdu, rtw89_core_rx_process_phy_ppdu): MAC 정보, 사용자들,
/// 수신 개수, PLCP 다음에 PHY 상태 머리말(종류, RSSI).
fn ppdu_status(st: &mut Stats, d: &rx::RxDesc, data: &[u8]) {
    let Some(w0) = le32(data, 0) else { return };
    if w0 & (1 << 27) != 0 {
        st.ppdu_invalid += 1;
        return;
    }
    let users = (w0 & 0x1f) as usize;
    let plcp = (((w0 >> 16) & 0xff) << 3) as usize;
    let rx_cnt = if w0 & (1 << 29) != 0 { 128 } else { 0 };
    let off = 8 + users * 4 + (users & 1) * 4 + rx_cnt + plcp;
    let (Some(h), Some(h1)) = (le32(data, off), le32(data, off + 4)) else {
        st.ppdu_invalid += 1;
        return;
    };
    if h & (1 << 7) == 0 {
        st.ppdu_invalid += 1;
        return;
    }
    let kind = (h & 0x1f) as u8;
    let dbm = |v: u32| ((v & 0xff) as i64 >> 1) - 110;
    let e = st.ppdus.entry((kind, d.rate, d.bw)).or_default();
    e.0 += 1;
    e.1 += dbm(h >> 24);
    e.2 += dbm(h1);
    e.3 += dbm(h1 >> 8);
}
