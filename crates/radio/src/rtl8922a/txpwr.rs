//! 송신 출력: 펌웨어 파일의 속도별 출력 표와 지역별 한도 표를 채널에 맞춰 MAC에 씁니다
//! (rtw89_fw_recognize_txpwr_from_elm, rtw89_phy_set_txpwr_byrate/offset/limit/limit_ru_be,
//! rtw8922a_set_txpwr_diff/ref/sar_diff, rtw8922a_set_tx_shape). 표 값은 0.25 dB(RF 단위), MAC은
//! 0.5 dB 단위입니다.
use std::collections::HashMap;
use std::io;

use super::chan::{Band, Bw, Chan};
use super::fw;
use super::generated::regs::*;
use super::{Rtl8922a, put};

const ELM_TXPWR_BYRATE: u32 = 9;
const ELM_LMT: [u32; 3] = [10, 11, 12];
const ELM_LMT_RU: [u32; 3] = [13, 14, 15];
const ELM_TX_SHAPE_LMT: u32 = 16;
const ELM_REGD: u32 = 20;
/// RTW89_TXPWR_CONF_DFLT_RFE_TYPE
const DFLT_RFE: u8 = 0;
const RS_CCK: u8 = 0;
const RS_OFDM: u8 = 1;
const RS_MCS: u8 = 2;
const RS_HEDCM: u8 = 3;
const RS_OFFSET: u8 = 4;
const REGD_WW: u8 = 0;
const REGD_NUM: u8 = 16;
/// RTW89_REG_6GHZ_POWER_VLP: 공유기 없이 기기끼리 쓰는 가장 낮은 등급.
const REG6_VLP: u8 = 0;
/// RTW89_SAR_TXPWR_MAC_MAX: SAR 설정이 없을 때.
const SAR_MAC_MAX: i8 = 63;
/// rtw8922a: 송신 출력 기준과 TSSI 기본값.
const TSSI_K_BASE: u32 = 0x12;

/// 속도별 출력 표 한 칸의 자리: (대역, 폭, 속도 묶음, OFDMA, NSS, 색인).
type ByrKey = (u8, u8, u8, u8, u8, u8);
/// 한도 표 한 칸: (대역, 폭, 송신 경로 수, 속도 묶음, 빔포밍, 규제, 6 GHz 등급, 채널 색인).
type LmtKey = (u8, u8, u8, u8, u8, u8, u8, u8);
/// RU 한도 표 한 칸: (대역, RU, 송신 경로 수, 규제, 6 GHz 등급, 채널 색인).
type RuKey = (u8, u8, u8, u8, u8, u8);

/// 펌웨어에서 읽은 송신 출력 표들.
#[derive(Default)]
pub struct TxPowerTables {
    byr: HashMap<ByrKey, i8>,
    lmt: HashMap<LmtKey, i8>,
    lmt_ru: HashMap<RuKey, i8>,
    tx_shape: HashMap<(u8, u8, u8, u8), u8>,
    /// 나라 (ISO 3166 두 글자) → 대역별 규제 (2.4, 5, 6 GHz).
    regd: HashMap<[u8; 2], [u8; 3]>,
}

/// rtw89_fw_recognize_txpwr_from_elm: 우리 A-die에 맞고 RFE 종류가 같은 마지막 요소, 없으면 기본(RFE 0).
fn pick<'a>(elements: &'a [fw::Element<'a>], id: u32, aid: u16, rfe: u8) -> Option<&'a fw::Element<'a>> {
    let mut chosen: Option<&fw::Element> = None;
    for e in elements.iter().filter(|e| e.id == id && (e.aid == 0 || e.aid == aid)) {
        let e_rfe = e.head[2];
        if e_rfe == rfe || (e_rfe == DFLT_RFE && chosen.is_none_or(|c| c.head[2] == DFLT_RFE)) {
            chosen = Some(e);
        }
    }
    chosen
}

/// 요소의 항목들 (ent_sz 바이트씩). 우리가 아는 크기보다 긴 항목은 남은 바이트가 0일 때만 받습니다
/// (__fw_txpwr_entry_acceptable).
fn entries<'a>(e: &'a fw::Element<'a>, size: usize) -> impl Iterator<Item = Vec<u8>> + 'a {
    let ent_sz = e.head[3] as usize;
    let n = u32::from_le_bytes(e.head[4..8].try_into().unwrap()) as usize;
    (0..n).filter_map(move |i| {
        let raw = e.body.get(i * ent_sz..(i + 1) * ent_sz)?;
        if raw.len() > size && raw[size..].iter().any(|b| *b != 0) {
            return None;
        }
        let mut v = raw[..raw.len().min(size)].to_vec();
        v.resize(size, 0);
        Some(v)
    })
}

/// rtw89_channel_to_idx
fn ch_idx(band: Band, ch: u8) -> u8 {
    match band {
        Band::G6 => match ch {
            1..=29 => (ch - 1) / 2,
            33..=61 => (ch - 3) / 2,
            65..=93 => (ch - 5) / 2,
            97..=125 => (ch - 7) / 2,
            129..=157 => (ch - 9) / 2,
            161..=189 => (ch - 11) / 2,
            193..=221 => (ch - 13) / 2,
            225..=253 => (ch - 15) / 2,
            _ => 0,
        },
        _ => match ch {
            1..=14 => ch - 1,
            36..=64 => (ch - 36) / 2,
            100..=144 => (ch - 100) / 2 + 15,
            149..=177 => (ch - 149) / 2 + 38,
            _ => 0,
        },
    }
}

/// RF 단위(0.25 dB) → MAC 단위(0.5 dB) (rtw89_phy_txpwr_rf_to_mac).
fn rf_to_mac(v: i8) -> i8 {
    v >> 1
}

fn pack(v: &[i8]) -> u32 {
    u32::from_le_bytes([v[0] as u8, v[1] as u8, v[2] as u8, v[3] as u8])
}

impl TxPowerTables {
    /// 펌웨어 파일에서 이 칩(aid, RFE 종류)의 표를 읽습니다.
    pub fn load(file: &[u8], aid: u16, rfe: u8) -> io::Result<TxPowerTables> {
        let elements = fw::elements(file)?;
        let mut t = TxPowerTables::default();
        if let Some(e) = pick(&elements, ELM_TXPWR_BYRATE, aid, rfe) {
            // rtw89_fw_txpwr_byrate_entry: band, nss, rs, shf, len, data(4), bw, ofdma
            for v in entries(e, 11) {
                let (band, nss, rs, shf, len, bw, ofdma) = (v[0], v[1], v[2], v[3], v[4], v[9], v[10]);
                if band >= 3 || bw >= 5 || rs > RS_OFFSET {
                    continue;
                }
                let data = u32::from_le_bytes(v[5..9].try_into().unwrap());
                for i in 0..len.min(4) {
                    let (o, n) = if matches!(rs, RS_MCS | RS_HEDCM) { (ofdma, nss) } else { (0, 0) };
                    t.byr.insert((band, bw, rs, o, n, shf + i), (data >> (8 * i)) as u8 as i8);
                }
            }
        }
        for (band, id) in ELM_LMT.iter().enumerate() {
            let Some(e) = pick(&elements, *id, aid, rfe) else { continue };
            let six = band == 2;
            // 2.4/5 GHz: bw, nt, rs, bf, regd, ch_idx, v. 6 GHz: bw, nt, rs, bf, regd, reg6, ch_idx, v.
            for v in entries(e, if six { 8 } else { 7 }) {
                let (reg6, ch, val) = if six { (v[5], v[6], v[7]) } else { (REG6_VLP, v[5], v[6]) };
                if v[4] < REGD_NUM {
                    t.lmt.insert((band as u8, v[0], v[1], v[2], v[3], v[4], reg6, ch), val as i8);
                }
            }
        }
        for (band, id) in ELM_LMT_RU.iter().enumerate() {
            let Some(e) = pick(&elements, *id, aid, rfe) else { continue };
            let six = band == 2;
            // 2.4/5 GHz: ru, nt, regd, ch_idx, v. 6 GHz: ru, nt, regd, reg6, ch_idx, v.
            for v in entries(e, if six { 6 } else { 5 }) {
                let (reg6, ch, val) = if six { (v[3], v[4], v[5]) } else { (REG6_VLP, v[3], v[4]) };
                t.lmt_ru.insert((band as u8, v[0], v[1], v[2], reg6, ch), val as i8);
            }
        }
        if let Some(e) = pick(&elements, ELM_TX_SHAPE_LMT, aid, rfe) {
            // rtw89_fw_tx_shape_lmt_entry: band, tx_shape_rs, regd, v, reg6_pwr
            for v in entries(e, 5) {
                t.tx_shape.insert((v[0], v[1], v[2], v[4]), v[3]);
            }
        }
        // REGD 요소는 머리말이 rsvd0..2, ent_sz, num_ents라 같은 자리에서 읽힙니다.
        if let Some(e) = elements.iter().rfind(|e| e.id == ELM_REGD) {
            // rtw89_fw_regd_entry: alpha2[2], rule_2ghz, rule_5ghz, rule_6ghz, fmap(4)
            for v in entries(e, 9) {
                let rule = |r: u8| if r < REGD_NUM { r } else { REGD_WW };
                t.regd.insert([v[0], v[1]], [rule(v[2]), rule(v[3]), rule(v[4])]);
            }
        }
        Ok(t)
    }

    /// 나라에 맞는 대역별 규제 (모르는 나라는 세계 공통).
    pub fn regd_of(&self, country: Option<[u8; 2]>) -> [u8; 3] {
        country
            .and_then(|c| self.regd.get(&c.map(|b| b.to_ascii_uppercase())).copied())
            .unwrap_or([REGD_WW; 3])
    }

    /// rtw89_phy_read_txpwr_byrate (MAC 단위).
    fn byrate(&self, band: Band, bw: u8, rs: u8, ofdma: u8, nss: u8, idx: u8) -> i8 {
        let band = if rs == RS_CCK { Band::G2 } else { band };
        let (o, n) = if matches!(rs, RS_MCS | RS_HEDCM) { (ofdma, nss) } else { (0, 0) };
        rf_to_mac(self.byr.get(&(band as u8, bw, rs, o, n, idx)).copied().unwrap_or(0))
    }

    /// rtw89_phy_read_txpwr_limit (MAC 단위, SAR 없음): 그 나라 값, 없으면 세계 공통 값.
    #[allow(clippy::too_many_arguments)] // 펌웨어 표의 열쇠 그대로
    fn limit(&self, regd: &[u8; 3], band: Band, bw: u8, ntx: u8, rs: u8, bf: u8, ch: u8) -> i8 {
        let b = band as u8;
        let (ci, reg6) = (ch_idx(band, ch), REG6_VLP);
        let get = |r: u8| self.lmt.get(&(b, bw, ntx, rs, bf, r, reg6, ci)).copied().unwrap_or(0);
        let v = match get(regd[b as usize]) {
            0 => get(REGD_WW),
            v => v,
        };
        rf_to_mac(v).min(SAR_MAC_MAX)
    }

    /// rtw89_phy_read_txpwr_limit_ru (MAC 단위).
    fn limit_ru(&self, regd: &[u8; 3], band: Band, ru: u8, ntx: u8, ch: u8) -> i8 {
        let b = band as u8;
        let ci = ch_idx(band, ch);
        let get = |r: u8| self.lmt_ru.get(&(b, ru, ntx, r, REG6_VLP, ci)).copied().unwrap_or(0);
        let v = match get(regd[b as usize]) {
            0 => get(REGD_WW),
            v => v,
        };
        rf_to_mac(v).min(SAR_MAC_MAX)
    }
}

/// rtw89_byr_spec_be: (속도 묶음, OFDMA, 첫 색인, 개수, 40 MHz까지만, 1 스트림만).
const BYR_SPEC: [(u8, u8, u8, u8, bool, bool); 8] = [
    (RS_CCK, 0, 0, 4, true, true),
    (RS_OFDM, 0, 0, 8, false, true),
    (RS_MCS, 0, 14, 2, false, true),
    (RS_MCS, 1, 14, 2, false, true),
    (RS_MCS, 0, 0, 14, false, false),
    (RS_HEDCM, 0, 0, 4, false, false),
    (RS_MCS, 1, 0, 14, false, false),
    (RS_HEDCM, 1, 0, 4, false, false),
];

impl Rtl8922a {
    /// 시험용: 이 채널에서 고른 속도들의 출력과 한도 (dBm).
    pub fn txpwr_summary(&self, c: &Chan) -> Option<String> {
        let (t, regd) = self.txpwr.as_ref()?;
        let bw = c.bw as u8;
        let dbm = |v: i8| v as f32 / 2.0;
        let mut s = format!("regd {regd:?}:");
        for (name, rs, nss, idx) in [
            ("OFDM6", RS_OFDM, 0, 0),
            ("OFDM54", RS_OFDM, 0, 7),
            ("MCS0", RS_MCS, 0, 0),
            ("MCS7", RS_MCS, 0, 7),
            ("MCS9 2SS", RS_MCS, 1, 9),
            ("MCS11 2SS", RS_MCS, 1, 11),
        ] {
            s += &format!(" {name} {}", dbm(t.byrate(c.band, bw, rs, 0, nss, idx)));
        }
        for ntx in 0..2 {
            s += &format!(
                " | limit {}TX: OFDM {} MCS {}",
                ntx + 1,
                dbm(t.limit(regd, c.band, 0, ntx, RS_OFDM, 0, c.primary)),
                dbm(t.limit(regd, c.band, bw, ntx, RS_MCS, 0, c.center))
            );
        }
        Some(s)
    }

    /// 채널 c의 송신 출력을 씁니다 (rtw8922a_set_txpwr). regd는 [TxPowerTables::regd_of]에서.
    pub fn set_txpwr(&self, t: &TxPowerTables, c: &Chan, regd: &[u8; 3]) -> io::Result<()> {
        let band = c.band;
        // rtw89_phy_set_txpwr_byrate_be: 폭 20..320, 스트림 1..2마다 정해진 순서로 4개씩.
        let mut addr = R_BE_PWR_BY_RATE;
        for bw in 0..=4u8 {
            for nss in 0..=1u8 {
                let mut v = Vec::new();
                for (rs, ofdma, first, num, only_40, only_1ss) in BYR_SPEC {
                    if (bw > 1 && only_40) || (nss > 0 && only_1ss) {
                        continue;
                    }
                    v.extend((first..first + num).map(|i| t.byrate(band, bw, rs, ofdma, nss, i)));
                }
                for q in v.as_chunks::<4>().0 {
                    self.w32(addr, pack(q))?;
                    addr += 4;
                }
            }
        }
        // rtw89_phy_set_txpwr_offset_be: 방식마다 4비트 차이.
        let off = |i: u8| (t.byrate(band, 0, RS_OFFSET, 0, 0, i) as u32) & 0xf;
        let ofst = off(4) | (off(3) << 4) | (off(2) << 8) | (off(1) << 12) | (off(0) << 16) | (off(7) << 20) | (off(6) << 24) | (off(5) << 28);
        self.w32(R_BE_PWR_RATE_OFST_CTRL, ofst)?;
        // rtw8922a_set_tx_shape: 그 나라의 OFDM 모양이 0이 아니면 삼각 모양.
        let shape = t
            .tx_shape
            .get(&(band as u8, RS_OFDM, regd[band as usize], REG6_VLP))
            .copied()
            .unwrap_or(0);
        self.pw32_mask(R_BEDGE3, B_BEDGE_CFG, (shape != 0) as u32)?;
        // rtw89_phy_set_txpwr_limit_be / _limit_ru_be: 송신 경로 수 1, 2마다 한 쪽.
        let mut addr = R_BE_PWR_LMT;
        for ntx in 0..=1u8 {
            for q in self.limit_page(t, regd, c, ntx).as_chunks::<4>().0 {
                self.w32(addr, pack(q))?;
                addr += 4;
            }
        }
        let mut addr = R_BE_PWR_RU_LMT;
        for ntx in 0..=1u8 {
            for q in Self::limit_ru_page(t, regd, c, ntx).as_chunks::<4>().0 {
                self.w32(addr, pack(q))?;
                addr += 4;
            }
        }
        // rtw8922a_set_txpwr_diff (안테나 이득 보정 없음): 기준 0, TSSI 기본값.
        for path in 0..2u32 {
            let o = path * 0x100;
            self.pw32_mask(R_TXAGC_REF_DBM_P0 + o, B_TXAGC_OFDM_REF_DBM_P0, 0)?;
            self.pw32_mask(R_TXAGC_REF_DBM_P0 + o, B_TXAGC_CCK_REF_DBM_P0, 0)?;
            self.pw32_mask(R_TSSI_K_P0 + o, B_TSSI_K_OFDM_P0, TSSI_K_BASE)?;
        }
        self.set_txpwr_ref()?;
        // rtw8922a_set_txpwr_sar_diff: SAR 없음.
        let sar_rf = (SAR_MAC_MAX as u32) << 1;
        self.pw32_mask(R_P0_TXPWRB_BE, B_TXPWRB_MAX_BE, sar_rf)?;
        self.pw32_mask(R_P1_TXPWRB_BE, B_TXPWRB_MAX_BE, sar_rf)
    }

    /// rtw8922a_set_txpwr_ref: MAC 쪽 기준 0 dBm.
    pub(super) fn set_txpwr_ref(&self) -> io::Result<()> {
        let v = put(put(self.r32(R_BE_PWR_REF_CTRL)?, B_BE_PWR_REF_CTRL_OFDM, 0), B_BE_PWR_REF_CTRL_CCK, 0);
        self.w32(R_BE_PWR_REF_CTRL, v)
    }

    /// rtw89_phy_fill_limit_be: struct rtw89_txpwr_limit_be (76바이트) 한 쪽.
    fn limit_page(&self, t: &TxPowerTables, regd: &[u8; 3], c: &Chan, ntx: u8) -> Vec<i8> {
        let (band, ch, pri) = (c.band, c.center, c.primary);
        let bf2 = |bw: u8, rs: u8, ch: u8| [t.limit(regd, band, bw, ntx, rs, 0, ch), t.limit(regd, band, bw, ntx, rs, 1, ch)];
        let min2 = |a: [i8; 2], b: [i8; 2]| [a[0].min(b[0]), a[1].min(b[1])];
        let mut cck20 = [0i8; 2];
        let mut cck40 = [0i8; 2];
        let mut mcs20 = [[0i8; 2]; 16];
        let mut mcs40 = [[0i8; 2]; 8];
        let mut mcs80 = [[0i8; 2]; 4];
        let mut mcs160 = [[0i8; 2]; 2];
        let mut half = [[0i8; 2]; 4];
        // 하위 20/40/80 MHz 채널들의 중심: 폭의 절반에서 시작해 4·8·16씩.
        let subs = |n: usize, step: i32| -> Vec<u8> { (0..n).map(|i| (ch as i32 - step * (n as i32 - 1) / 2 + step * i as i32) as u8).collect() };
        let ofdm = match c.bw {
            Bw::B20 => {
                cck20 = bf2(0, RS_CCK, ch);
                cck40 = bf2(1, RS_CCK, ch);
                mcs20[0] = bf2(0, RS_MCS, ch);
                bf2(0, RS_OFDM, ch)
            }
            Bw::B40 => {
                cck20 = bf2(0, RS_CCK, ch.wrapping_sub(2));
                cck40 = bf2(1, RS_CCK, ch);
                for (i, s) in subs(2, 4).into_iter().enumerate() {
                    mcs20[i] = bf2(0, RS_MCS, s);
                }
                mcs40[0] = bf2(1, RS_MCS, ch);
                bf2(0, RS_OFDM, pri)
            }
            Bw::B80 => {
                for (i, s) in subs(4, 4).into_iter().enumerate() {
                    mcs20[i] = bf2(0, RS_MCS, s);
                }
                let s40 = subs(2, 8);
                for (i, s) in s40.iter().enumerate() {
                    mcs40[i] = bf2(1, RS_MCS, *s);
                }
                mcs80[0] = bf2(2, RS_MCS, ch);
                half[0] = min2(mcs40[0], mcs40[1]);
                bf2(0, RS_OFDM, pri)
            }
            Bw::B160 => {
                for (i, s) in subs(8, 4).into_iter().enumerate() {
                    mcs20[i] = bf2(0, RS_MCS, s);
                }
                for (i, s) in subs(4, 8).into_iter().enumerate() {
                    mcs40[i] = bf2(1, RS_MCS, s);
                }
                for (i, s) in subs(2, 16).into_iter().enumerate() {
                    mcs80[i] = bf2(2, RS_MCS, s);
                }
                mcs160[0] = bf2(3, RS_MCS, ch);
                half[0] = min2(mcs40[0], mcs40[1]);
                half[1] = min2(mcs40[2], mcs40[3]);
                bf2(0, RS_OFDM, pri)
            }
        };
        let mut v = Vec::with_capacity(76);
        v.extend(cck20);
        v.extend(cck40);
        v.extend(ofdm);
        mcs20.iter().for_each(|x| v.extend(x));
        mcs40.iter().for_each(|x| v.extend(x));
        mcs80.iter().for_each(|x| v.extend(x));
        mcs160.iter().for_each(|x| v.extend(x));
        v.extend([0i8; 2]); // 320 MHz
        half.iter().for_each(|x| v.extend(x));
        v
    }

    /// rtw89_phy_fill_limit_ru_be: struct rtw89_txpwr_limit_ru_be (80바이트) 한 쪽.
    fn limit_ru_page(t: &TxPowerTables, regd: &[u8; 3], c: &Chan, ntx: u8) -> Vec<i8> {
        let n = match c.bw {
            Bw::B20 => 1,
            Bw::B40 => 2,
            Bw::B80 => 4,
            Bw::B160 => 8,
        };
        let mut rows = [[0i8; 16]; 5];
        for i in 0..n {
            let ch = (c.center as i32 - 4 * (n as i32 - 1) / 2 + 4 * i as i32) as u8;
            for (ru, row) in rows.iter_mut().enumerate() {
                row[i] = t.limit_ru(regd, c.band, ru as u8, ntx, ch);
            }
        }
        rows.iter().flatten().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_index() {
        assert_eq!(ch_idx(Band::G5, 149), 38);
        assert_eq!(ch_idx(Band::G5, 155), 41);
        assert_eq!(ch_idx(Band::G5, 36), 0);
        assert_eq!(ch_idx(Band::G6, 17), 8);
        assert_eq!(ch_idx(Band::G6, 15), 7);
    }

    #[test]
    fn firmware_tables() {
        let t = TxPowerTables::load(super::super::FIRMWARE, 0, 1).unwrap();
        assert!(!t.byr.is_empty() && !t.lmt.is_empty() && !t.lmt_ru.is_empty());
        assert_eq!(t.regd_of(Some(*b"KR")), [6, 6, 6]);
    }
}
