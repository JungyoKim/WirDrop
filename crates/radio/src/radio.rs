//! 중계기가 쓰는 무선: 올리기, 프레임 받기, 프레임 보내기. 어댑터는 두 가지입니다: RTL8812BU/
//! RTL8822BU(TP-Link Archer T3U Nano, rtw88에서 옮김)와 RTL8912AU(ipTIME BE6500UA, rtw89에서 옮김).
use std::io;
use std::sync::Mutex;
use std::time::Instant;

use crate::chip::err;
use crate::generated::regs::*;
use crate::rtl8822b::phy::{Bw, Channel};
use crate::rtl8822b::{RX_DESC_SIZE, Rtl8822b, TX_DESC_SIZE, checksum};
use crate::rtl8922a::{self, HwRate, Rtl8922a, rx as rx89};
use crate::usb::{Reader, Usb, UsbId};

/// Realtek의 RTL8822B 펌웨어 (linux-firmware rtw88/rtw8822b_fw.bin, 수정 없이 재배포 가능).
pub static FIRMWARE: &[u8] = include_bytes!("../firmware/rtw8822b_fw.bin");

/// AWDL BSSID: AWDL 프레임의 주소 3.
pub const AWDL_BSSID: [u8; 6] = [0x00, 0x25, 0x00, 0xff, 0x94, 0x73];

/// 속도 부호의 수: 0-3 CCK, 4-11 OFDM, 12-43 HT MCS0-31, 44-83 VHT 1-4스트림 MCS0-9,
/// 84-107 HE 1-2스트림 MCS0-11 ([rate_name]).
pub const RATE_CODES: usize = 108;
const HT_BASE: u8 = 12;
const VHT_BASE: u8 = 44;
const HE_BASE: u8 = 84;

/// 속도 부호의 이름.
pub fn rate_name(code: u8) -> String {
    const LEGACY: [&str; 12] = ["1M", "2M", "5.5M", "11M", "6M", "9M", "12M", "18M", "24M", "36M", "48M", "54M"];
    match code {
        0..=11 => LEGACY[code as usize].to_string(),
        HT_BASE..=43 => format!("HT_MCS{}", code - HT_BASE),
        VHT_BASE..=83 => format!("VHT{}SS_MCS{}", (code - VHT_BASE) / 10 + 1, (code - VHT_BASE) % 10),
        HE_BASE..=107 => format!("HE{}SS_MCS{}", (code - HE_BASE) / 12 + 1, (code - HE_BASE) % 12),
        _ => format!("code{code}"),
    }
}

/// 속도 부호의 공간 스트림 수 (HT·VHT·HE만).
pub fn rate_streams(code: u8) -> Option<u8> {
    match code {
        HT_BASE..=43 => Some((code - HT_BASE) / 8 + 1),
        VHT_BASE..=83 => Some((code - VHT_BASE) / 10 + 1),
        HE_BASE..=107 => Some((code - HE_BASE) / 12 + 1),
        _ => None,
    }
}

/// RTL8922A 수신 서술자의 속도(rtw89 v1: 10..8 방식, 7..5 NSS-1, 4..0 MCS) → 속도 부호.
fn rate_from_v1(v: u16) -> u8 {
    let (mode, nss, mcs) = ((v >> 8) & 7, ((v >> 5) & 7) as u8, (v & 0x1f) as u8);
    match mode {
        0 => (v & 0xf).min(11) as u8,
        1 => HT_BASE + (v & 0x1f) as u8,
        2 => VHT_BASE + 10 * nss.min(3) + mcs.min(9),
        // HE와 EHT(같은 MCS 표의 앞부분)는 2스트림까지.
        _ => HE_BASE + 12 * nss.min(1) + mcs.min(11),
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub usb: UsbId,
    pub channel: u8,
    pub bw_mhz: u16,
    /// 우리 주소: 여기로 온 프레임에 하드웨어가 ACK와 Block Ack을 보냅니다.
    pub mac: [u8; 6],
    pub bssid: [u8; 6],
    /// RTL8822B: 모든 속도에 쓰는 고정 TXAGC 지수 (0-63), 아니면 PHY 표의 기본값.
    pub tx_power: Option<u8>,
    /// 유니캐스트 프레임의 하드웨어 재전송 횟수.
    pub retry_limit: u8,
    pub igi: Option<u8>,
    /// A-MPDU 하나의 최대 MPDU 수 (RTL8822B 서술자는 둘씩 세어 62까지). 16이면 3.9 KB A-MSDU로 된
    /// A-MPDU가 HT 수신기가 받는 64 KB 안에 듭니다.
    pub ampdu_max: u8,
    /// 우리가 보내는 A-MPDU의 최소 MPDU 시작 간격 (802.11 코드: 6 = 8 us, Apple 기기가 알리는 값).
    pub ampdu_density: u8,
    /// RTL8912AU: 송신 출력 한도를 정할 나라 (ISO 3166 두 글자). 없으면 세계 공통 한도.
    pub country: Option<[u8; 2]>,
    /// RTL8912AU: USB 전송 하나에 담는 프레임 한도 ([rtl8922a::Rtl8922a::send_many]).
    pub usb_tx_agg: rtl8922a::TxAgg,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            usb: UsbId::T3U_NANO,
            channel: 149,
            bw_mhz: 80,
            mac: [0x02, 0, 0, 0, 0, 1],
            bssid: AWDL_BSSID,
            tx_power: None,
            retry_limit: 8,
            igi: None,
            ampdu_max: 16,
            ampdu_density: 6,
            country: None,
            usb_tx_agg: rtl8922a::TxAgg::new(16, 20 << 10),
        }
    }
}

/// 프레임 하나를 보내는 방법.
#[derive(Debug, Clone, Copy, Default)]
pub struct TxRate {
    /// 속도 부호 ([rate_name]).
    pub rate: u8,
    /// 0 = 20 MHz, 1 = 40, 2 = 80.
    pub bw: u8,
    /// 짧은 보호 구간 (HE는 0.8 us, 아니면 1.6 us; 둘 다 2배 HE-LTF).
    pub sgi: bool,
    pub ldpc: bool,
}

impl TxRate {
    pub const OFDM6: TxRate = TxRate {
        rate: 4,
        bw: 0,
        sgi: false,
        ldpc: false,
    };

    /// 500 kb/s 단위(2 = 1 Mb/s ... 108 = 54 Mb/s)에서, 또는 0x80 | HT MCS에서.
    pub fn legacy(code: u8) -> TxRate {
        let rate = if code & 0x80 != 0 {
            HT_BASE + (code & 0x1f)
        } else {
            match code {
                2 => 0,
                4 => 1,
                11 => 2,
                22 => 3,
                12 => 4,
                18 => 5,
                24 => 6,
                36 => 7,
                48 => 8,
                72 => 9,
                96 => 10,
                108 => 11,
                _ => 4,
            }
        };
        TxRate { rate, ..TxRate::OFDM6 }
    }

    pub fn vht(nss: u8, mcs: u8, bw: u8, sgi: bool, ldpc: bool) -> TxRate {
        TxRate {
            rate: VHT_BASE + 10 * (nss.clamp(1, 2) - 1) + mcs.min(9),
            bw,
            sgi,
            ldpc,
        }
    }

    /// HE SU (RTL8912AU만).
    pub fn he(nss: u8, mcs: u8, bw: u8, sgi: bool, ldpc: bool) -> TxRate {
        TxRate {
            rate: HE_BASE + 12 * (nss.clamp(1, 2) - 1) + mcs.min(11),
            bw,
            sgi,
            ldpc,
        }
    }

    /// RTL8822B 서술자 속도: HE는 같은 스트림 수의 VHT로.
    fn rtw88_code(&self) -> u8 {
        match self.rate {
            HE_BASE..=107 => {
                let (nss, mcs) = ((self.rate - HE_BASE) / 12, (self.rate - HE_BASE) % 12);
                VHT_BASE + 10 * nss + mcs.min(9)
            }
            r => r,
        }
    }

    fn rate_id(&self) -> u32 {
        match self.rtw88_code() {
            0..=3 => 8,        // B
            4..=11 => 7,       // G
            0x0c..=0x13 => 5,  // N 1SS
            0x14..=0x2b => 4,  // N 2SS
            0x2c..=0x35 => 10, // AC 1SS
            _ => 9,            // AC 2SS
        }
    }

    /// RTL8922A 하드웨어 속도. HE는 20 MHz를 넘거나 MCS10 이상이면 LDPC가 필수입니다.
    fn hw_rate(&self) -> HwRate {
        let c = self.rate;
        match c {
            0..=11 => HwRate::legacy(c),
            HT_BASE..=43 => HwRate::ht(c - HT_BASE, self.bw, self.sgi),
            VHT_BASE..=83 => HwRate::vht((c - VHT_BASE) / 10 + 1, (c - VHT_BASE) % 10, self.bw, self.sgi, self.ldpc),
            _ => {
                let (nss, mcs) = ((c - HE_BASE) / 12 + 1, (c - HE_BASE) % 12);
                let ldpc = self.ldpc || self.bw > 0 || mcs >= 10;
                // RTW89_GILTF_2XHE08 / _2XHE16
                HwRate::he(nss, mcs, self.bw, if self.sgi { 3 } else { 2 }, ldpc)
            }
        }
    }
}

/// 내보낼 프레임 (802.11 헤더부터, FCS 없이).
pub struct TxFrame<'a> {
    pub data: &'a [u8],
    pub rate: TxRate,
    /// A-MPDU에 실어도 됩니다 (받는 쪽과 Block Ack 합의가 열려 있음).
    pub ampdu: bool,
    /// ACK를 기대하지 않습니다 (그룹 프레임처럼 보냄).
    pub no_ack: bool,
    /// 이 6비트 번호를 단 송신 보고를 펌웨어에 청합니다 ([Radio::parse_with_c2h] 참고).
    pub report: Option<u8>,
    /// 최선 노력 대신 음성 큐로 채널을 다툽니다 ([Radio::set_voice_access] 참고).
    pub voice: bool,
}

/// 받은 프레임.
#[derive(Debug)]
pub struct RxFrame<'a> {
    /// FCS를 뺀 MPDU.
    pub data: &'a [u8],
    pub crc_err: bool,
    pub icv_err: bool,
    /// 속도 부호 ([rate_name]).
    pub rate: u8,
    /// PPDU 카운터: 한 A-MPDU의 MPDU들은 같은 값입니다.
    pub ppdu_cnt: u8,
    /// PPDU의 마지막 MPDU: RTL8922A는 PPDU를 다 받은 뒤 그 상태 보고를 보내므로 그때 표시합니다. 그
    /// PPDU의 MPDU가 앞선 USB 전송에 있었으면 data가 빈 표시만 넘깁니다 (RTL8822B는 늘 false).
    pub ampdu_end: bool,
    /// PHY 상태 (PPDU의 첫 MPDU): 경로별 출력(dBm), SNR과 EVM (반 dB 단위).
    pub phy: Option<PhyStatus>,
    pub tsf_low: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PhyStatus {
    pub power: [i8; 2],
    pub snr: [i8; 2],
    pub evm: [i8; 2],
    /// 0 = 20 MHz, 1 = 40, 2 = 80, 3 = 160.
    pub bw: u8,
}

enum Chip {
    Rtw88(Rtl8822b),
    Rtw89(Box<Rtl8922a>),
}

pub struct Radio {
    chip: Chip,
    pub cfg: Config,
    /// RTL8922A: 우리에게 보내는 상대들의 MACID와 받는 쪽 Block Ack 합의.
    peers: Mutex<Peers>,
}

/// RTL8922A가 MACID를 주는 상대 수 (MACID 1..=PEER_SLOTS).
const PEER_SLOTS: usize = 16;

/// RTL8922A의 주소 CAM에 둔 상대들 ([Rtl8922a::add_peer]).
#[derive(Default, Clone)]
pub struct Peers {
    /// MACID i + 1 자리의 상대 주소와 마지막으로 들린 때.
    slots: [Option<([u8; 6], Instant)>; PEER_SLOTS],
    /// 정적 Block Ack CAM 항목마다 (MACID, TID, 시작 순서 번호, 창).
    ba: [Option<(u8, u8, u16, u16)>; rtl8922a::BACAM_STATIC_NUM as usize],
}

impl Radio {
    /// 어댑터를 열어 cfg의 채널에 올리고 보내고 받을 준비를 합니다. cfg.usb가 RTL8912AU 계열이면
    /// rtw89 쪽 드라이버를 씁니다.
    pub fn open(cfg: Config) -> io::Result<Radio> {
        if rtl8922a::USB_IDS.contains(&cfg.usb) {
            return Self::open_rtw89(cfg);
        }
        let t = Instant::now();
        // 막 꽂은 어댑터는 WinUSB가 잡기 전에 목록에 나올 수 있습니다.
        let usb = loop {
            match Usb::open(cfg.usb) {
                Err(e) if e.kind() == io::ErrorKind::NotFound && t.elapsed() < std::time::Duration::from_secs(3) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                r => break r?,
            }
        };
        if t.elapsed().as_millis() >= 100 {
            tracing::info!("adapter there after {} ms", t.elapsed().as_millis());
        }
        let mut chip = Rtl8822b::new(usb)?;
        chip.power_on()?;
        chip.read_efuse()?;
        tracing::info!(
            "efuse: RFE type {}, crystal 0x{:02x}, thermal {}, MAC {}",
            chip.efuse.rfe_option,
            chip.efuse.crystal_cap,
            chip.efuse.thermal_meter,
            mac_str(&chip.efuse.mac)
        );
        if !matches!(chip.efuse.rfe_option, 2 | 3 | 5) {
            return Err(err(format!("RFE type {} is not supported", chip.efuse.rfe_option)));
        }
        chip.download_firmware(FIRMWARE)?;
        tracing::info!("firmware up at {} ms", t.elapsed().as_millis());
        chip.mac_init()?;
        chip.phy_set_param()?;
        chip.rx_agg(true)?;
        chip.send_fw_info()?;
        chip.coex_wifi_only()?;
        let radio = Radio {
            chip: Chip::Rtw88(chip),
            cfg,
            peers: Mutex::default(),
        };
        radio.configure_rtw88()?;
        tracing::info!("radio up at {} ms", t.elapsed().as_millis());
        Ok(radio)
    }

    fn open_rtw89(cfg: Config) -> io::Result<Radio> {
        let t = Instant::now();
        let chan = rtl8922a::Chan::around(cfg.channel, rtw89_bw(cfg.bw_mhz));
        let (chip, rfk) = Rtl8922a::start_with(cfg.usb, &chan, cfg.country)?;
        let ok = rfk.iter().filter(|r| r.ok()).count();
        tracing::debug!(
            "RF calibration: {}",
            rfk.iter()
                .map(|r| format!("{} {:?} {} ms", r.name, r.state, r.ms))
                .collect::<Vec<_>>()
                .join(", ")
        );
        tracing::info!(
            "RTL8912AU {} cv {} RFE {}: firmware, MAC, BB/RF up, RF calibration {ok}/{} at {} ms",
            if chip.usb3 { "USB 3" } else { "USB 2" },
            chip.cv,
            chip.rfe,
            rfk.len(),
            t.elapsed().as_millis()
        );
        if let Some(s) = chip.txpwr_summary(&chan) {
            tracing::info!("tx power {s}");
        }
        chip.vif_init(cfg.mac, cfg.bssid)?;
        // 채널의 모든 것을 받습니다 (거르는 것은 중계기).
        chip.set_promisc(false)?;
        tracing::info!(
            "on channel {} ({} MHz, center {}) at {} ms",
            chan.primary,
            cfg.bw_mhz,
            chan.center,
            t.elapsed().as_millis()
        );
        Ok(Radio {
            chip: Chip::Rtw89(Box::new(chip)),
            cfg,
            peers: Mutex::default(),
        })
    }

    fn configure_rtw88(&self) -> io::Result<()> {
        let Chip::Rtw88(c) = &self.chip else { return Ok(()) };
        let r = &c.regs;
        let cfg = &self.cfg;
        // 포트 0: 우리 주소, BSSID, 애드혹 (AWDL은 IBSS 비슷합니다), TSF 동작.
        for i in 0..6 {
            r.w8(0x0610 + i, cfg.mac[i as usize])?;
            r.w8(0x0618 + i, cfg.bssid[i as usize])?;
        }
        r.w32m(REG_CR, 0x30000, 1)?; // 포트 0 네트워크 형식: 애드혹
        r.w8(REG_BCN_CTRL, (BIT_EN_BCN_FUNCTION | BIT_DIS_TSF_UDT) as u8)?;
        // 채널의 모든 것을 FCS와 PHY 상태와 함께 받습니다 (거르는 것은 중계기).
        let rcr = BIT_APP_FCS | BIT_APP_MIC | BIT_APP_ICV | BIT_APP_PHYSTS | BIT_VHT_DACK | BIT_HTC_LOC_CTRL | BIT_AAP | BIT_APM | BIT_AM | BIT_AB;
        r.w32(REG_RCR, rcr)?;
        r.w16(REG_RXFLTMAP0, 0xffff)?;
        r.w16(REG_RXFLTMAP1, 0xffff)?;
        r.w16(REG_RXFLTMAP2, 0xffff)?;
        r.w16(REG_RETRY_LIMIT, ((cfg.retry_limit as u16 & 0x3f) << 8) | (cfg.retry_limit as u16 & 0x3f))?;
        let ch = Channel::new(cfg.channel, cfg.bw_mhz);
        c.set_channel(ch)?;
        if let Some(p) = cfg.tx_power {
            c.set_tx_power(p)?;
        }
        c.iqk()?;
        if let Some(igi) = cfg.igi {
            c.set_igi(igi)?;
        }
        tracing::info!(
            "on channel {} ({} MHz, center {}), IGI 0x{:02x}",
            ch.primary,
            ch.bw.mhz(),
            ch.center,
            c.igi()?
        );
        Ok(())
    }

    /// RTL8822B 드라이버 (동적 이득, 카운터처럼 그 칩에만 있는 것).
    pub fn rtw88(&self) -> Option<&Rtl8822b> {
        match &self.chip {
            Chip::Rtw88(c) => Some(c),
            Chip::Rtw89(_) => None,
        }
    }

    /// RTL8922A 드라이버.
    pub fn rtw89(&self) -> Option<&Rtl8922a> {
        match &self.chip {
            Chip::Rtw89(c) => Some(c),
            Chip::Rtw88(_) => None,
        }
    }

    /// 어댑터 이름 (로그용).
    pub fn name(&self) -> &'static str {
        match &self.chip {
            Chip::Rtw88(_) => "RTL8822BU",
            Chip::Rtw89(_) => "RTL8912AU",
        }
    }

    /// 펌웨어가 [TxFrame::report]에 답합니다.
    pub fn has_tx_reports(&self) -> bool {
        matches!(self.chip, Chip::Rtw88(_))
    }

    fn usb(&self) -> &Usb {
        match &self.chip {
            Chip::Rtw88(c) => &c.regs.usb,
            Chip::Rtw89(c) => &c.usb,
        }
    }

    /// 기다리는 수신을 멈춥니다 (받는 스레드가 끝날 수 있게).
    pub fn abort_rx(&self) {
        self.usb().abort_in();
    }

    pub fn bw(&self) -> Bw {
        Channel::new(self.cfg.channel, self.cfg.bw_mhz).bw
    }

    /// 처음 채널과 같은 묶음(중심과 폭) 안의 주 채널 primary로 옮깁니다: 레지스터 몇 개만 바뀝니다
    /// (AWDL 상대의 슬롯을 따라갈 때). 묶음 밖의 채널은 거절합니다.
    pub fn set_primary(&self, primary: u8) -> io::Result<()> {
        let outside = || {
            err(format!(
                "channel {primary} is outside channel {}'s {} MHz",
                self.cfg.channel, self.cfg.bw_mhz
            ))
        };
        match &self.chip {
            Chip::Rtw88(c) => {
                let (home, to) = (Channel::new(self.cfg.channel, self.cfg.bw_mhz), Channel::new(primary, self.cfg.bw_mhz));
                if to.center != home.center || to.bw != home.bw {
                    return Err(outside());
                }
                c.set_primary(to)
            }
            Chip::Rtw89(c) => {
                let bw = rtw89_bw(self.cfg.bw_mhz);
                let (home, to) = (rtl8922a::Chan::around(self.cfg.channel, bw), rtl8922a::Chan::around(primary, bw));
                if to.center != home.center || to.bw != home.bw || to.band != home.band {
                    return Err(outside());
                }
                c.set_primary(&to)
            }
        }
    }

    fn fill_desc(&self, d: &mut [u8], f: &TxFrame) {
        let (bmc, qsel, mgmt) = {
            let fc0 = f.data[0];
            let group = f.data.len() > 4 && f.data[4] & 1 != 0;
            let mgmt = fc0 & 0x0c == 0x00 || fc0 & 0x0c == 0x04;
            // 데이터는 TID 0(최선 노력)으로, 관리와 제어는 관리 큐로.
            (group || f.no_ack, self.qsel(f), mgmt)
        };
        let mut w = [0u32; 10];
        w[0] = (f.data.len() as u32 & 0xffff) | ((TX_DESC_SIZE as u32) << 16) | ((bmc as u32) << 24) | (1 << 26);
        if mgmt {
            w[0] |= 1 << 31; // DISQSELSEQ
        }
        w[1] = ((qsel as u32) << 8) | (f.rate.rate_id() << 16);
        if f.ampdu {
            w[2] = (1 << 12) | ((self.cfg.ampdu_density as u32 & 7) << 20); // AGG_EN, AMPDU_DEN
            w[3] |= ((self.cfg.ampdu_max as u32 / 2).clamp(1, 0x1f)) << 17; // MAX_AGG_NUM, 둘씩
        }
        if let Some(sn) = f.report {
            w[2] |= 1 << 19; // SPE_RPT
            w[6] = ((sn as u32) << 2) & 0xfc; // SW_DEFINE[7:2]: 보고의 번호
        }
        w[3] |= (1 << 8) | (1 << 10); // USE_RATE, DISDATAFB
        w[4] = f.rate.rtw88_code() as u32 & 0x7f;
        w[5] = ((f.rate.sgi as u32) << 4) | ((f.rate.bw as u32 & 3) << 5) | ((f.rate.ldpc as u32) << 7);
        let seq = if f.data.len() >= 24 {
            (u16::from_le_bytes([f.data[22], f.data[23]]) >> 4) as u32
        } else {
            0
        };
        w[9] = seq << 12;
        for (i, v) in w.iter().enumerate() {
            d[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
        checksum(d);
    }

    /// 프레임을 보냅니다. RTL8822B는 같은 큐면 USB 전송 하나에 셋까지 묶습니다 (칩의 송신 묶음).
    /// 나간 수를 돌려줍니다.
    pub fn send(&self, frames: &[TxFrame]) -> io::Result<usize> {
        if let Chip::Rtw89(c) = &self.chip {
            return self.send_rtw89(c, frames);
        }
        let Chip::Rtw88(chip) = &self.chip else { unreachable!() };
        let mut sent = 0;
        let mut buf: Vec<u8> = Vec::with_capacity(20 * 1024);
        let mut i = 0;
        while i < frames.len() {
            buf.clear();
            let ep = self.ep_for(chip, &frames[i]);
            let mut n = 0;
            while i < frames.len() && n < 3 && self.ep_for(chip, &frames[i]) == ep {
                let f = &frames[i];
                if f.data.len() < 10 || f.data.len() > 11000 {
                    i += 1;
                    continue;
                }
                if buf.len() + TX_DESC_SIZE + f.data.len() > 20 * 1024 {
                    break;
                }
                while !buf.len().is_multiple_of(8) {
                    buf.push(0);
                }
                let at = buf.len();
                buf.resize(at + TX_DESC_SIZE, 0);
                self.fill_desc(&mut buf[at..at + TX_DESC_SIZE], f);
                buf.extend_from_slice(f.data);
                n += 1;
                i += 1;
            }
            if n == 0 {
                continue;
            }
            if n > 1 {
                let mut w7 = u32::from_le_bytes(buf[28..32].try_into().unwrap());
                w7 = (w7 & 0x00ff_ffff) | ((n as u32) << 24);
                buf[28..32].copy_from_slice(&w7.to_le_bytes());
                checksum(&mut buf[..TX_DESC_SIZE]);
            }
            chip.regs.usb.bulk_write(ep, &buf)?;
            sent += n;
        }
        Ok(sent)
    }

    fn send_rtw89(&self, c: &Rtl8922a, frames: &[TxFrame]) -> io::Result<usize> {
        let hw: Vec<rtl8922a::TxFrame> = frames
            .iter()
            .filter(|f| (10..=11454).contains(&f.data.len()))
            .map(|f| {
                let mut t = rtl8922a::TxFrame::new(f.data, f.rate.hw_rate());
                t.ampdu = f.ampdu;
                t.ampdu_max = self.cfg.ampdu_max;
                t.ampdu_density = self.cfg.ampdu_density;
                t.no_ack = f.no_ack;
                t.voice = f.voice;
                t.retry = Some(self.cfg.retry_limit);
                t
            })
            .collect();
        c.send_many(&hw, &self.cfg.usb_tx_agg)
    }

    fn ep_for(&self, chip: &Rtl8822b, f: &TxFrame) -> u8 {
        chip.ep_for_qsel(self.qsel(f))
    }

    /// 하드웨어 큐: 관리, 음성(6), 최선 노력(0).
    fn qsel(&self, f: &TxFrame) -> u8 {
        let fc0 = f.data.first().copied().unwrap_or(0);
        if fc0 & 0x0c != 0x08 {
            super::rtl8822b::QSEL_MGMT
        } else if f.voice {
            6
        } else {
            0
        }
    }

    /// 벌크 읽기 count개를 동시에 걸어 두는 수신기.
    pub fn receiver(&self, count: usize) -> io::Result<Reader<'_>> {
        match &self.chip {
            Chip::Rtw88(c) => c.regs.usb.reader(count, 32 * 1024),
            Chip::Rtw89(c) => c.usb.reader(count, 64 * 1024),
        }
    }

    /// USB 수신 전송 하나를 프레임들로 나눕니다.
    pub fn parse<'a>(&self, buf: &'a [u8], f: impl FnMut(RxFrame<'a>)) {
        self.parse_with_c2h(buf, f, |_| {})
    }

    /// USB 수신 전송 하나를 프레임과 펌웨어 메시지(C2H)로 나눕니다.
    pub fn parse_with_c2h<'a>(&self, buf: &'a [u8], mut f: impl FnMut(RxFrame<'a>), on_c2h: impl FnMut(C2h<'a>)) {
        match &self.chip {
            Chip::Rtw88(_) => parse_rtw88(buf, f, on_c2h),
            Chip::Rtw89(c) => parse_rtw89(buf, |r| {
                if !r.crc_err {
                    self.track_peer(c, r.data);
                }
                f(r)
            }),
        }
    }

    /// RTL8922A: 우리 주소로 온 프레임의 보낸 이를 주소 CAM에 두고 (그래야 칩이 그 상대의 A-MPDU에
    /// Block Ack을, RTS에 CTS를 보냅니다), 상대의 ADDBA 요청과 DELBA를 따라 받는 쪽 Block Ack 항목을
    /// 고칩니다. 우리에게 온 관리, 데이터, 제어 프레임(RTS, BAR)과 AWDL 방송 프레임의 보낸 이를 둡니다:
    /// 상대가 RTS로 시작하면 CTS를 받기 전에는 데이터를 보내지 않으므로, 데이터만 보고 두면 서로
    /// 기다리기만 합니다. 받는 스레드가 프레임마다 부르고, 새 상대나 합의가 바뀔 때만 펌웨어에 씁니다.
    fn track_peer(&self, c: &Rtl8922a, d: &[u8]) {
        if d.len() < 16 || d[10] & 1 != 0 {
            return;
        }
        let control = d[0] & 0x0c == 0x04;
        let to_us = d[4..10] == self.cfg.mac;
        // 보낸 이(주소 2)가 있는 제어 프레임: RTS, BAR, Block Ack, PS-Poll.
        let control_with_ta = control && matches!(d[0], 0xb4 | 0x84 | 0x94 | 0xa4);
        let awdl_broadcast = !control && d.len() >= 24 && d[4] & 1 != 0 && d[16..22] == self.cfg.bssid;
        if !(to_us && (!control && d.len() >= 24 || control_with_ta) || awdl_broadcast) {
            return;
        }
        let ta: [u8; 6] = d[10..16].try_into().unwrap();
        let now = Instant::now();
        let mut p = self.peers.lock().unwrap();
        let macid = match p.slots.iter().position(|s| s.is_some_and(|(m, _)| m == ta)) {
            Some(i) => {
                p.slots[i] = Some((ta, now));
                i as u8 + 1
            }
            None => {
                // 빈 자리, 없으면 가장 오래 들리지 않은 상대의 자리.
                let i = p
                    .slots
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or_else(|| (0..PEER_SLOTS).min_by_key(|&i| p.slots[i].map(|(_, t)| t)).unwrap_or(0));
                let macid = i as u8 + 1;
                for e in 0..p.ba.len() {
                    if let Some((m, tid, _, _)) = p.ba[e].filter(|b| b.0 == macid) {
                        let _ = c.ba_cam(e as u8, m, tid, 0, 64, false);
                        p.ba[e] = None;
                    }
                }
                if let Err(e) = c.add_peer(macid, &ta, &self.cfg.mac, &self.cfg.bssid) {
                    tracing::warn!("can't give {} a MACID: {e}", mac_str(&ta));
                    return;
                }
                tracing::info!("{} gets MACID {macid}", mac_str(&ta));
                p.slots[i] = Some((ta, now));
                macid
            }
        };
        // Block Ack 분류(3)의 ADDBA 요청(0)과 DELBA(2).
        if !to_us || d[0] != 0xd0 || d.len() < 28 || d[24] != 3 {
            return;
        }
        match d[25] {
            0 if d.len() >= 33 => {
                let param = u16::from_le_bytes([d[27], d[28]]);
                let (tid, size) = (((param >> 2) & 0xf) as u8, param >> 6);
                let ssn = u16::from_le_bytes([d[31], d[32]]) >> 4;
                let free =
                    p.ba.iter()
                        .position(|b| b.is_some_and(|b| (b.0, b.1) == (macid, tid)))
                        .or_else(|| p.ba.iter().position(Option::is_none));
                let Some(e) = free else {
                    tracing::info!("Block Ack from {} TID {tid}: no static entry left, the chip makes one", mac_str(&ta));
                    return;
                };
                match c.ba_cam(e as u8, macid, tid, ssn, size, true) {
                    Ok(()) => {
                        p.ba[e] = Some((macid, tid, ssn, size));
                        tracing::info!("Block Ack from {} TID {tid}: window {size} from {ssn}", mac_str(&ta));
                    }
                    Err(err) => tracing::warn!("Block Ack entry for {}: {err}", mac_str(&ta)),
                }
            }
            // 보내는 쪽(상대)이 끝낸 합의만: 우리가 보내는 쪽인 합의에는 받는 쪽 항목이 없습니다.
            2 if u16::from_le_bytes([d[26], d[27]]) & (1 << 11) != 0 => {
                let tid = (u16::from_le_bytes([d[26], d[27]]) >> 12) as u8;
                if let Some(e) = p.ba.iter().position(|b| b.is_some_and(|b| (b.0, b.1) == (macid, tid))) {
                    let _ = c.ba_cam(e as u8, macid, tid, 0, 64, false);
                    p.ba[e] = None;
                }
            }
            _ => {}
        }
    }

    /// RTL8922A: 지금 주소 CAM과 Block Ack CAM에 둔 상대들 (어댑터를 다시 올릴 때 [Radio::restore_peers]로).
    pub fn peers(&self) -> Peers {
        self.peers.lock().unwrap().clone()
    }

    /// 다시 올린 칩에 앞의 상대들과 받는 쪽 Block Ack 합의를 그대로 씁니다: 상대는 합의가 살아 있다고
    /// 보고 곧바로 A-MPDU나 RTS로 보냅니다.
    pub fn restore_peers(&self, old: Peers) {
        let Chip::Rtw89(c) = &self.chip else { return };
        let mut p = self.peers.lock().unwrap();
        for (i, slot) in old.slots.iter().enumerate() {
            if let Some((ta, _)) = slot
                && let Err(e) = c.add_peer(i as u8 + 1, ta, &self.cfg.mac, &self.cfg.bssid)
            {
                tracing::warn!("can't give {} its MACID back: {e}", mac_str(ta));
            }
        }
        for (e, b) in old.ba.iter().enumerate() {
            if let Some((macid, tid, ssn, size)) = *b
                && let Err(err) = c.ba_cam(e as u8, macid, tid, ssn, size, true)
            {
                tracing::warn!("Block Ack entry {e} back: {err}");
            }
        }
        *p = old;
    }

    /// bfer의 채널 측정에 하드웨어로 답합니다 (SU 빔포밍 수신). RTL8912AU는 아직 아무것도 하지 않습니다.
    pub fn arm_beamformee(&self, bfer: [u8; 6]) -> io::Result<()> {
        match &self.chip {
            Chip::Rtw88(c) => c.bfee_su(bfer, 1),
            Chip::Rtw89(_) => Ok(()),
        }
    }

    /// 음성 큐가 채널에 닿는 방식: 매체가 aifs_us만큼 비어 있은 뒤 (SIFS 포함) 2^ecw_min - 1
    /// 슬롯의 백오프 창, 실패하면 2^ecw_max - 1까지 두 배씩.
    pub fn set_voice_access(&self, aifs_us: u8, ecw_min: u8, ecw_max: u8) -> io::Result<()> {
        match &self.chip {
            Chip::Rtw88(c) => {
                let v = aifs_us as u16 | (ecw_min as u16 & 0xf) << 8 | (ecw_max as u16 & 0xf) << 12;
                c.regs.w16(crate::generated::regs::REG_EDCA_VO_PARAM, v)
            }
            Chip::Rtw89(c) => c.set_edca_vo(aifs_us, ecw_min, ecw_max),
        }
    }

    /// 칩 전원을 내립니다 (어댑터는 다음에 열 수 있게 둡니다).
    pub fn close(self) {
        match self.chip {
            Chip::Rtw88(c) => {
                let _ = c.power_off();
            }
            Chip::Rtw89(c) => c.power_off(),
        }
    }
}

/// MHz를 RTL8922A의 채널 폭으로.
fn rtw89_bw(mhz: u16) -> rtl8922a::Bw {
    match mhz {
        160 => rtl8922a::Bw::B160,
        80 => rtl8922a::Bw::B80,
        40 => rtl8922a::Bw::B40,
        _ => rtl8922a::Bw::B20,
    }
}

/// RTL8822B 수신 전송 하나.
fn parse_rtw88<'a>(buf: &'a [u8], mut f: impl FnMut(RxFrame<'a>), mut on_c2h: impl FnMut(C2h<'a>)) {
    let mut at = 0usize;
    while at + RX_DESC_SIZE <= buf.len() {
        let d = &buf[at..];
        let w = |i: usize| u32::from_le_bytes(d[4 * i..4 * i + 4].try_into().unwrap());
        let (w0, w2, w3, w5) = (w(0), w(2), w(3), w(5));
        let pkt_len = (w0 & 0x3fff) as usize;
        let drv_info = (((w0 >> 16) & 0xf) * 8) as usize;
        let shift = ((w0 >> 24) & 3) as usize;
        let physt = w0 & (1 << 26) != 0;
        let is_c2h = w2 & (1 << 28) != 0;
        let off = RX_DESC_SIZE + drv_info + shift;
        let total = off + pkt_len;
        if pkt_len == 0 || at + total > buf.len() {
            break;
        }
        if is_c2h && pkt_len >= 2 {
            let m = &d[off..off + pkt_len];
            on_c2h(C2h::parse(m));
        }
        if !is_c2h && pkt_len > 4 {
            let phy = if physt && drv_info >= 32 {
                let p = &d[RX_DESC_SIZE + shift..];
                let page = p[0] & 0xf;
                if page == 1 {
                    let pw = |b: u8| (b as i16 - 110).clamp(-128, 127) as i8;
                    let rxsc = if (4..12).contains(&(w3 & 0x7f)) { p[5] & 0xf } else { p[5] >> 4 };
                    let bw = match rxsc {
                        1..=8 => 0,
                        9..=12 => 1,
                        13.. => 2,
                        _ => (p[15] >> 4) & 3,
                    };
                    Some(PhyStatus {
                        power: [pw(p[1]), pw(p[2])],
                        snr: [p[24] as i8, p[25] as i8],
                        evm: [p[16] as i8, p[17] as i8],
                        bw,
                    })
                } else {
                    Some(PhyStatus {
                        power: [(p[1] as i16 - 110) as i8, -128],
                        snr: [0; 2],
                        evm: [0; 2],
                        bw: 0,
                    })
                }
            } else {
                None
            };
            f(RxFrame {
                data: &d[off..off + pkt_len - 4],
                crc_err: w0 & (1 << 14) != 0,
                icv_err: w0 & (1 << 15) != 0,
                rate: (w3 & 0x7f) as u8,
                ppdu_cnt: ((w2 >> 29) & 3) as u8,
                ampdu_end: false,
                phy,
                tsf_low: w5,
            });
        }
        at += total.div_ceil(8) * 8;
    }
}

/// RTL8922A 수신 전송 하나. MPDU의 PHY 상태(경로별 세기)는 그 PPDU의 PPDU 상태 보고에 따로 오므로,
/// 같은 전송 안에서 PPDU 카운터로 짝지어 (RTL8822B처럼 A-MPDU의 첫 MPDU에만) 붙입니다. 짝이 없으면
/// 없는 채로, 모두 받은 순서대로 넘깁니다.
fn parse_rtw89<'a>(buf: &'a [u8], mut f: impl FnMut(RxFrame<'a>)) {
    let mut held: Vec<RxFrame<'a>> = Vec::new();
    // 아직 PPDU 상태를 기다리는 MPDU가 시작하는 자리.
    let mut open = 0;
    rx89::packets(buf, |d, data| {
        if d.kind == rx89::RX_TYPE_PPDU_STAT {
            if let Some(r) = held[open..].iter_mut().rev().find(|r| r.ppdu_cnt == d.ppdu_cnt) {
                r.ampdu_end = true;
            } else {
                held.push(RxFrame {
                    data: &[],
                    crc_err: false,
                    icv_err: false,
                    rate: 0,
                    ppdu_cnt: d.ppdu_cnt,
                    ampdu_end: true,
                    phy: None,
                    tsf_low: d.free_run,
                });
            }
            let Some(phy) = rx89::ppdu_power(data) else { return };
            let phy = PhyStatus {
                power: phy,
                snr: [0; 2],
                evm: [0; 2],
                bw: d.bw.min(3),
            };
            if let Some(r) = held[open..].iter_mut().find(|r| r.ppdu_cnt == d.ppdu_cnt) {
                r.phy = Some(phy);
            }
            open = held.len();
            return;
        }
        // 서술자 종류 63: CRC가 깨진 프레임 (rtw89에는 없는 값이지만 이 칩은 이렇게 보냅니다).
        let crc_err = d.crc_err || d.kind == 63;
        // 끝의 FCS 4바이트는 떼고 넘깁니다 (RTL8822B와 같이).
        if (d.kind != rx89::RX_TYPE_WIFI && d.kind != 63) || data.len() < 10 + 4 {
            return;
        }
        let data = &data[..data.len() - 4];
        held.push(RxFrame {
            data,
            crc_err,
            icv_err: d.icv_err,
            rate: rate_from_v1(d.rate),
            ppdu_cnt: d.ppdu_cnt,
            ampdu_end: false,
            phy: None,
            tsf_low: d.free_run,
        });
    });
    // 같은 PPDU가 짝을 찾은 순서대로 나가도록, 남은 것과 함께 받은 순서를 지킵니다.
    for r in held {
        f(r);
    }
}

pub fn mac_str(m: &[u8]) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// 펌웨어가 보낸 메시지.
#[derive(Debug)]
pub enum C2h<'a> {
    /// [TxFrame::report]로 보낸 프레임의 결과: 확인되었는지 아닌지.
    TxReport {
        sn: u8,
        ok: bool,
        raw: &'a [u8],
    },
    Other {
        id: u8,
        payload: &'a [u8],
    },
}

impl<'a> C2h<'a> {
    fn parse(m: &'a [u8]) -> C2h<'a> {
        let (id, p) = (m[0], &m[2..]);
        match id {
            // C2H_CCX_TX_RPT (v0): 상태는 payload[0]의 7:6비트, 번호는 payload[6]의 7:2비트
            0x03 if p.len() >= 7 => C2h::TxReport {
                sn: p[6] >> 2,
                ok: p[0] & 0xc0 == 0,
                raw: p,
            },
            // C2H_HALMAC / C2H_CCX_RPT (v1)
            0xff if p.len() >= 10 && p[0] == 0x0f => C2h::TxReport {
                sn: p[8] >> 2,
                ok: p[9] & 0xc0 == 0,
                raw: p,
            },
            _ => C2h::Other { id, payload: p },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_codes() {
        assert_eq!(rate_name(TxRate::he(2, 11, 2, true, true).rate), "HE2SS_MCS11");
        assert_eq!(rate_name(TxRate::vht(2, 7, 2, true, true).rate), "VHT2SS_MCS7");
        assert_eq!(rate_from_v1(0x309), HE_BASE + 9);
        assert_eq!(rate_from_v1(0x32b), HE_BASE + 12 + 11);
        assert_eq!(rate_from_v1(0x228), VHT_BASE + 10 + 8);
        assert_eq!(rate_from_v1(8), 8);
        assert_eq!(rate_streams(HE_BASE + 12 + 3), Some(2));
        let hw = TxRate::he(2, 9, 2, true, false).hw_rate();
        assert_eq!((hw.code, hw.bw, hw.gi_ltf, hw.ldpc), (0x329, 2, 3, true));
        assert_eq!(TxRate::he(1, 7, 2, false, false).rtw88_code(), VHT_BASE + 7);
    }
}
