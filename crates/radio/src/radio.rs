//! 중계기가 쓰는 무선: 올리기, 프레임 받기, 프레임 보내기.
use std::io;
use std::time::Instant;

use crate::chip::err;
use crate::generated::regs::*;
use crate::rtl8822b::phy::{Bw, Channel};
use crate::rtl8822b::{RX_DESC_SIZE, Rtl8822b, TX_DESC_SIZE, checksum};
use crate::usb::{Reader, Usb, UsbId};

/// Realtek의 RTL8822B 펌웨어 (linux-firmware rtw88/rtw8822b_fw.bin, 수정 없이 재배포 가능).
pub static FIRMWARE: &[u8] = include_bytes!("../firmware/rtw8822b_fw.bin");

/// AWDL BSSID: AWDL 프레임의 주소 3.
pub const AWDL_BSSID: [u8; 6] = [0x00, 0x25, 0x00, 0xff, 0x94, 0x73];

#[derive(Debug, Clone)]
pub struct Config {
    pub usb: UsbId,
    pub channel: u8,
    pub bw_mhz: u16,
    /// 우리 주소: 여기로 온 프레임에 하드웨어가 ACK와 Block Ack을 보냅니다.
    pub mac: [u8; 6],
    pub bssid: [u8; 6],
    /// 모든 속도에 쓰는 고정 TXAGC 지수 (0-63), 아니면 PHY 표의 기본값.
    pub tx_power: Option<u8>,
    /// 유니캐스트 프레임의 하드웨어 재전송 횟수.
    pub retry_limit: u8,
    pub igi: Option<u8>,
    /// A-MPDU 하나의 최대 MPDU 수 (서술자는 둘씩 세어 62까지). 16이면 3.9 KB A-MSDU로 된 A-MPDU가
    /// HT 수신기가 받는 64 KB 안에 듭니다.
    pub ampdu_max: u8,
    /// 우리가 보내는 A-MPDU의 최소 MPDU 시작 간격 (802.11 코드: 6 = 8 us, Apple 기기가 알리는 값).
    pub ampdu_density: u8,
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
        }
    }
}

/// 프레임 하나를 보내는 방법.
#[derive(Debug, Clone, Copy, Default)]
pub struct TxRate {
    /// 서술자 속도: 0-3 CCK, 4-11 OFDM, 0x0c+ HT MCS, 0x2c + 10*(nss-1) + mcs VHT.
    pub rate: u8,
    /// 0 = 20 MHz, 1 = 40, 2 = 80.
    pub bw: u8,
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
            0x0c + (code & 0x1f)
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
            rate: 0x2c + 10 * (nss.clamp(1, 2) - 1) + mcs.min(9),
            bw,
            sgi,
            ldpc,
        }
    }

    fn rate_id(&self) -> u32 {
        match self.rate {
            0..=3 => 8,        // B
            4..=11 => 7,       // G
            0x0c..=0x13 => 5,  // N 1SS
            0x14..=0x2b => 4,  // N 2SS
            0x2c..=0x35 => 10, // AC 1SS
            _ => 9,            // AC 2SS
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
    /// 서술자 속도 ([TxRate::rate] 참고).
    pub rate: u8,
    /// 2비트 PPDU 카운터: 한 A-MPDU의 MPDU들은 같은 값입니다.
    pub ppdu_cnt: u8,
    /// PHY 상태 (PPDU의 첫 MPDU): 경로별 출력(dBm), SNR과 EVM (반 dB 단위).
    pub phy: Option<PhyStatus>,
    pub tsf_low: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PhyStatus {
    pub power: [i8; 2],
    pub snr: [i8; 2],
    pub evm: [i8; 2],
    /// 0 = 20 MHz, 1 = 40, 2 = 80.
    pub bw: u8,
}

pub struct Radio {
    pub chip: Rtl8822b,
    pub cfg: Config,
}

impl Radio {
    /// 어댑터를 열어 cfg의 채널에 올리고 보내고 받을 준비를 합니다.
    pub fn open(cfg: Config) -> io::Result<Radio> {
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
        let radio = Radio { chip, cfg };
        radio.configure()?;
        tracing::info!("radio up at {} ms", t.elapsed().as_millis());
        Ok(radio)
    }

    fn configure(&self) -> io::Result<()> {
        let c = &self.chip;
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

    pub fn bw(&self) -> Bw {
        Channel::new(self.cfg.channel, self.cfg.bw_mhz).bw
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
        w[4] = f.rate.rate as u32 & 0x7f;
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

    /// 프레임을 보냅니다. 같은 큐면 USB 전송 하나에 셋까지 묶습니다 (칩의 송신 묶음). 나간 수를
    /// 돌려줍니다.
    pub fn send(&self, frames: &[TxFrame]) -> io::Result<usize> {
        let mut sent = 0;
        let mut buf: Vec<u8> = Vec::with_capacity(20 * 1024);
        let mut i = 0;
        while i < frames.len() {
            buf.clear();
            let ep = self.ep_for(&frames[i]);
            let mut n = 0;
            while i < frames.len() && n < 3 && self.ep_for(&frames[i]) == ep {
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
            self.chip.regs.usb.bulk_write(ep, &buf)?;
            sent += n;
        }
        Ok(sent)
    }

    fn ep_for(&self, f: &TxFrame) -> u8 {
        self.chip.ep_for_qsel(self.qsel(f))
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

    /// 32 KB 벌크 읽기 `count`개를 동시에 걸어 두는 수신기.
    pub fn receiver(&self, count: usize) -> io::Result<Reader<'_>> {
        self.chip.regs.usb.reader(count, 32 * 1024)
    }

    /// USB 수신 전송 하나를 프레임들로 나눕니다.
    pub fn parse<'a>(buf: &'a [u8], f: impl FnMut(RxFrame<'a>)) {
        Self::parse_with_c2h(buf, f, |_| {})
    }

    /// USB 수신 전송 하나를 프레임과 펌웨어 메시지(C2H)로 나눕니다.
    pub fn parse_with_c2h<'a>(buf: &'a [u8], mut f: impl FnMut(RxFrame<'a>), mut on_c2h: impl FnMut(C2h<'a>)) {
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
                    phy,
                    tsf_low: w5,
                });
            }
            at += total.div_ceil(8) * 8;
        }
    }

    /// `bfer`의 채널 측정에 하드웨어로 답합니다 (SU 빔포밍 수신).
    pub fn arm_beamformee(&self, bfer: [u8; 6]) -> io::Result<()> {
        self.chip.bfee_su(bfer, 1)
    }

    /// 음성 큐가 채널에 닿는 방식: 매체가 `aifs_us`만큼 비어 있은 뒤 (SIFS 포함) 2^ecw_min - 1
    /// 슬롯의 백오프 창, 실패하면 2^ecw_max - 1까지 두 배씩.
    pub fn set_voice_access(&self, aifs_us: u8, ecw_min: u8, ecw_max: u8) -> io::Result<()> {
        let v = aifs_us as u16 | (ecw_min as u16 & 0xf) << 8 | (ecw_max as u16 & 0xf) << 12;
        self.chip.regs.w16(crate::generated::regs::REG_EDCA_VO_PARAM, v)
    }

    /// 칩 전원을 내립니다 (어댑터는 다음에 열 수 있게 둡니다).
    pub fn close(self) {
        let _ = self.chip.power_off();
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
