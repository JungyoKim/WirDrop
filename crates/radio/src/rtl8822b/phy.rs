//! RTL8822B 베이스밴드와 RF: 표, 채널 전환, 안테나 경로, 보정 (rtw88 phy.c, rtw8822b.c, coex.c,
//! bf.c; BSD-3-Clause).
use std::io;

use super::Rtl8822b;
use crate::chip::{RFREG_MASK, delay_ms, delay_us, err};
use crate::generated::regs::*;
use crate::generated::tables;

const MASKDWORD: u32 = 0xffff_ffff;
const MASKLWORD: u32 = 0x0000_ffff;
const MASKBYTE0: u32 = 0xff;
const MASKBYTE1: u32 = 0xff00;
const BB_PATH_A: u8 = 1;
const BB_PATH_B: u8 = 2;
const BB_PATH_AB: u8 = 3;

/// 두 번 읽는 사이에 PHY가 센 것: 오경보, 그리고 (정상, CRC 실패) MPDU.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhyCounters {
    pub false_alarms: u32,
    pub ofdm: (u32, u32),
    pub ht: (u32, u32),
    pub vht: (u32, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bw {
    B20 = 0,
    B40 = 1,
    B80 = 2,
}

impl Bw {
    pub fn mhz(self) -> u16 {
        match self {
            Bw::B20 => 20,
            Bw::B40 => 40,
            Bw::B80 => 80,
        }
    }
}

/// 채널: 주 20 MHz 채널, 폭, 그 폭의 중심 채널.
#[derive(Debug, Clone, Copy)]
pub struct Channel {
    pub primary: u8,
    pub bw: Bw,
    pub center: u8,
    /// rtw88의 primary_ch_idx (RTW_SC_*): 0 상관없음, 1 위, 2 아래, 3 맨 위, 4 맨 아래.
    pub sc: u8,
}

impl Channel {
    /// `primary`를 담는 이 폭의 채널 (5 GHz 40/80 MHz 블록은 36/149에서 시작).
    pub fn new(primary: u8, mhz: u16) -> Channel {
        let bw = match mhz {
            80 if primary > 14 => Bw::B80,
            40 | 80 if primary > 14 => Bw::B40,
            _ => Bw::B20,
        };
        let (center, sc) = match bw {
            Bw::B20 => (primary, 0),
            Bw::B40 => {
                let lower = ((primary as i32 - 36).rem_euclid(8)) < 4;
                if lower { (primary + 2, 2) } else { (primary - 2, 1) }
            }
            Bw::B80 => {
                let pos = (primary as i32 - 36).rem_euclid(16) / 4; // 가장 낮은 것부터 0..3
                let center = (primary as i32 + 6 - 4 * pos) as u8;
                // rtw_update_channel: 중심보다 아래면 10 MHz 떨어진 것이 LOWER, 아니면 LOWEST.
                let sc = match pos {
                    0 => 4,
                    1 => 2,
                    2 => 1,
                    _ => 3,
                };
                (center, sc)
            }
        };
        Channel { primary, bw, center, sc }
    }
}

fn is_2g(ch: u8) -> bool {
    ch <= 14
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_blocks() {
        // 80 MHz 블록 149-161 (중심 155): 가장 낮은 것부터 매긴 rtw88의 서브채널 번호.
        let sc: Vec<(u8, u8)> = [149, 153, 157, 161]
            .iter()
            .map(|&p| (Channel::new(p, 80).center, Channel::new(p, 80).sc))
            .collect();
        assert_eq!(sc, [(155, 4), (155, 2), (155, 1), (155, 3)]);
        let c = Channel::new(36, 40);
        assert_eq!((c.center, c.sc, c.bw), (38, 2, Bw::B40));
        let c = Channel::new(40, 40);
        assert_eq!((c.center, c.sc), (38, 1));
        let c = Channel::new(6, 80);
        assert_eq!((c.center, c.bw), (6, Bw::B20));
    }
}
fn band_5g_1(ch: u8) -> bool {
    (36..=48).contains(&ch)
}
fn band_5g_2(ch: u8) -> bool {
    (52..=64).contains(&ch)
}
fn band_5g_3(ch: u8) -> bool {
    (100..=144).contains(&ch)
}
fn band_5g_4(ch: u8) -> bool {
    (149..=177).contains(&ch)
}

#[derive(Clone, Copy)]
enum Table {
    Mac,
    Bb,
    Agc,
    Rf(usize),
}

impl Rtl8822b {
    /// 이 칩의 조건(컷, USB, efuse RFE 형식)으로 하는 rtw_parse_tbl_phy_cond.
    fn load_table(&self, data: &[u32], kind: Table) -> io::Result<usize> {
        let r = &self.regs;
        let drv_cut = if self.cut != 0 { self.cut as u32 } else { 15 };
        let drv_rfe = self.efuse.rfe_option as u32;
        let positive = |c: u32| -> bool {
            let rfe = c & 0xff;
            let intf = (c >> 8) & 0xf;
            let pkg = (c >> 12) & 0xf;
            let cut = (c >> 24) & 0xf;
            !(cut != 0 && cut != drv_cut) && !(pkg != 0 && pkg != 15) && !(intf != 0 && intf != 2) && rfe == drv_rfe
        };
        let (mut matched, mut skipped) = (true, false);
        let mut pos_cond = 0u32;
        let mut writes = 0;
        for &[a, d] in data.as_chunks::<2>().0 {
            if a & (1 << 31) != 0 {
                match (a >> 28) & 3 {
                    3 => {
                        matched = true;
                        skipped = false;
                    }
                    2 => matched = !skipped,
                    _ => pos_cond = a,
                }
            } else if a & (1 << 30) != 0 {
                if !skipped {
                    if positive(pos_cond) {
                        matched = true;
                        skipped = true;
                    } else {
                        matched = false;
                        skipped = false;
                    }
                } else {
                    matched = false;
                }
            } else if matched {
                writes += 1;
                match kind {
                    Table::Mac => r.w8(a, d as u8)?,
                    Table::Agc => r.w32(a, d)?,
                    Table::Bb => match a {
                        0xfe => delay_ms(50),
                        0xfd => delay_ms(5),
                        0xfc => delay_ms(1),
                        0xfb => delay_us(50),
                        0xfa => delay_us(5),
                        0xf9 => delay_us(1),
                        _ => r.w32(a, d)?,
                    },
                    Table::Rf(path) => match a {
                        0xffe => delay_ms(50),
                        0xfe => delay_us(100),
                        _ => r.rf_write(path, a, RFREG_MASK, d)?,
                    },
                }
            }
        }
        Ok(writes)
    }

    /// rtw8822b_phy_set_param: BB/RF 전원을 올리고 표를 불러옵니다.
    pub fn phy_set_param(&self) -> io::Result<()> {
        let r = &self.regs;
        r.set8(REG_SYS_FUNC_EN, BIT_FEN_BB_RSTB | BIT_FEN_BB_GLB_RST)?;
        r.set8(REG_RF_CTRL, BIT_RF_EN | BIT_RF_RSTB | BIT_RF_SDM_RSTB)?;
        r.set32(REG_WLRF1, BIT_WLRF1_BBRF_EN)?;
        r.clr32(REG_RXPSEL, BIT_RX_PSEL_RST)?;
        let t = std::time::Instant::now();
        let mut n = self.load_table(&tables::MAC, Table::Mac)?;
        n += self.load_table(&tables::BB, Table::Bb)?;
        n += self.load_table(&tables::AGC, Table::Agc)?;
        n += self.load_table(&tables::RF_A, Table::Rf(0))?;
        if self.rf_paths > 1 {
            n += self.load_table(&tables::RF_B, Table::Rf(1))?;
        }
        tracing::info!(
            "PHY tables: {n} writes in {} ms (RFE type {})",
            t.elapsed().as_millis(),
            self.efuse.rfe_option
        );
        let cap = (self.efuse.crystal_cap & 0x3f) as u32;
        r.w32m(0x24, 0x7e00_0000, cap)?;
        r.w32m(0x28, 0x7e, cap)?;
        r.set32(REG_RXPSEL, BIT_RX_PSEL_RST)?;
        let paths = if self.rf_paths > 1 { BB_PATH_AB } else { BB_PATH_A };
        self.config_trx_mode(paths, paths, 149)?;
        // rtw_phy_init -> rtw8822b_adaptivity_init: EDCCA 문턱을 최대로.
        r.w32m(0x8a4, MASKBYTE0, 0x7f)?;
        r.w32m(0x8a4, MASKBYTE1, 0x7f)?;
        r.clr32(REG_TX_PTCL_CTRL, BIT_DIS_EDCCA)?;
        r.set32(REG_RD_CTRL, BIT_EDCCA_MSK_CNTDOWN_EN)?;
        r.w32m(REG_EDCCA_SOURCE, BIT_SOURCE_OPTION, 1)?;
        r.w32m(REG_EDCCA_POW_MA, BIT_MA_LEVEL, 0)?;
        r.set32(REG_EDCCA_DECISION, BIT_EDCCA_OPTION)?;
        // rtw8822b_phy_rfe_init: 칩 최상위 mux, s0/s1의 RFE를 출력으로.
        r.w32m(0x64, (1 << 29) | (1 << 28), 0x3)?;
        r.w32m(0x4c, (1 << 26) | (1 << 25), 0x0)?;
        r.w32m(0x40, 1 << 2, 0x1)?;
        r.w32m(0x1990, 0x3f, 0x30)?;
        r.w32m(0x1990, (1 << 11) | (1 << 10), 0x3)?;
        r.w32m(0x974, 0x3f, 0x3f)?;
        r.w32m(0x974, (1 << 11) | (1 << 10), 0x3)?;
        // rtw8822b_phy_bf_init -> rtw_bf_phy_init
        let mut v = r.r32(REG_MU_TX_CTL)?;
        v |= BIT_MU_P1_WAIT_STATE_EN;
        v = (v & !BIT_MASK_R_MU_RL) | ((0xa << BIT_SHIFT_R_MU_RL) & BIT_MASK_R_MU_RL);
        v &= !BIT_EN_MU_MIMO;
        v &= !BIT_MASK_R_MU_TABLE_VALID;
        r.w32(REG_MU_TX_CTL, v)?;
        r.w8(
            REG_WMAC_MU_BF_OPTION,
            ((3 << BIT_SHIFT_WMAC_TXMU_ACKPOLICY) | BIT_WMAC_TXMU_ACKPOLICY_EN) as u8,
        )?;
        r.w16(REG_WMAC_MU_BF_CTL, 0)?;
        r.set32(REG_TXBF_CTRL, BIT_USE_NDPA_PARAMETER)?;
        r.w8(REG_NDPA_OPT_CTRL, 0x10)?;
        r.w32m(REG_BBPSF_CTRL, BIT_MASK_CSI_RATE, 4)?; // DESC_RATE6M
        r.w32(0x1c94, 0xafff_afff)
    }

    fn rfe_ifem(&self) -> bool {
        // rtw8822b_rfe_info: 형식 2는 iFEM 2.4 GHz / eFEM 5 GHz, 3과 5는 iFEM.
        self.efuse.rfe_option != 2
    }

    fn set_channel_rfe(&self, ch: u8, rx: u8, tx: u8) -> io::Result<()> {
        let r = &self.regs;
        let trsw_2g = if rx == BB_PATH_AB || tx == BB_PATH_AB {
            0xa501
        } else if rx == tx {
            0xa500
        } else {
            0xa005
        };
        if self.rfe_ifem() {
            if is_2g(ch) {
                r.w32sm(REG_RFESEL0, 0xffffff, 0x745774)?;
                r.w32sm(REG_RFESEL8, MASKBYTE1, 0x57)?;
            } else {
                r.w32sm(REG_RFESEL0, 0xffffff, 0x477547)?;
                r.w32sm(REG_RFESEL8, MASKBYTE1, 0x75)?;
            }
            r.w32sm(REG_RFEINV, (1 << 11) | (1 << 10) | 0x3f, 0)?;
            r.w32sm(REG_TRSW, MASKLWORD, if is_2g(ch) { trsw_2g } else { 0xa5a5 })
        } else {
            if is_2g(ch) {
                r.w32sm(REG_RFESEL0, 0xffffff, 0x705770)?;
                r.w32sm(REG_RFESEL8, MASKBYTE1, 0x57)?;
                r.w32sm(REG_RFECTL, 1 << 4, 0)?;
            } else {
                r.w32sm(REG_RFESEL0, 0xffffff, 0x177517)?;
                r.w32sm(REG_RFESEL8, MASKBYTE1, 0x75)?;
                r.w32sm(REG_RFECTL, 1 << 5, 0)?;
            }
            r.w32sm(REG_RFEINV, (1 << 11) | (1 << 10) | 0x3f, 0)?;
            r.w32sm(REG_TRSW, MASKLWORD, trsw_2g)
        }
    }

    fn set_channel_cca(&self, ch: u8, bw: Bw, rx: u8) -> io::Result<()> {
        let r = &self.regs;
        let one_r = rx == BB_PATH_A || rx == BB_PATH_B;
        let col = match (is_2g(ch), one_r) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        };
        // 열마다 (reg82c, reg830, reg838): iFEM-ext (형식 3, 5); iFEM 2G / eFEM 5G (형식 2).
        const IFEM_EXT: [[u32; 4]; 3] = [
            [0x75da8010, 0x75da8010, 0x75da8010, 0x75da8010],
            [0x79a0eaaa, 0x97a0eaac, 0x79a0eaaa, 0x79a0eaaa],
            [0x87765541, 0x86666341, 0x87765561, 0x86666361],
        ];
        const IFEM: [[u32; 4]; 3] = [
            [0x75c97010, 0x75c97010, 0x75c97010, 0x75c97010],
            [0x79a0eaaa, 0x79a0eaac, 0x79a0eaaa, 0x79a0eaaa],
            [0x87765541, 0x87746341, 0x87765541, 0x87746341],
        ];
        const EFEM: [[u32; 4]; 3] = [
            [0x75b86010, 0x75b76010, 0x75b86010, 0x75b76010],
            [0x79a0eaa8, 0x79a0eaac, 0x79a0eaa8, 0x79a0eaaa],
            [0x87766451, 0x87766431, 0x87766451, 0x87766431],
        ];
        let (tbl, ifem_cca, ext) = if self.rfe_ifem() {
            (&IFEM_EXT, true, true)
        } else if is_2g(ch) {
            (&IFEM, true, false)
        } else {
            (&EFEM, false, false)
        };
        let (r82c, mut r830, r838) = (tbl[0][col], tbl[1][col], tbl[2][col]);
        if ifem_cca
            && ((self.cut == 1 && (col == 1 || col == 3) && bw == Bw::B40)
                || (!ext && col == 3 && bw == Bw::B40)
                || (self.efuse.rfe_option == 5 && col == 3))
        {
            r830 = 0x79a0ea28;
        }
        r.w32m(REG_CCASEL, MASKDWORD, r82c)?;
        r.w32m(REG_PDMFTH, MASKDWORD, r830)?;
        r.w32m(REG_CCA2ND, MASKDWORD, r838)?;
        if !ifem_cca && self.cut != 1 {
            r.w32m(REG_L1WT, MASKDWORD, 0x9194b2b9)?;
        }
        if bw == Bw::B20 && (36..=144).contains(&ch) && !band_5g_1(ch) && !band_5g_2(ch) {
            r.w32m(REG_CCA2ND, 0xf0, 0x4)?;
        }
        Ok(())
    }

    fn toggle_igi(&self, rx: u8) -> io::Result<()> {
        let r = &self.regs;
        let igi = r.r32m(REG_RXIGI_A, 0x7f)?;
        r.w32m(REG_RXIGI_A, 0x7f, igi.wrapping_sub(2))?;
        r.w32m(REG_RXIGI_A, 0x7f, igi)?;
        r.w32m(REG_RXIGI_B, 0x7f, igi.wrapping_sub(2))?;
        r.w32m(REG_RXIGI_B, 0x7f, igi)?;
        r.w32m(REG_RXPSEL, MASKBYTE0, 0)?;
        r.w32m(REG_RXPSEL, MASKBYTE0, (rx | (rx << 4)) as u32)
    }

    /// rtw8822b_config_trx_mode.
    fn config_trx_mode(&self, tx: u8, rx: u8, ch: u8) -> io::Result<()> {
        let r = &self.regs;
        r.w32m(REG_AGCTR_A, MASKLWORD, if (tx | rx) & BB_PATH_A != 0 { 0x3231 } else { 0x1111 })?;
        r.w32m(REG_AGCTR_B, MASKLWORD, if (tx | rx) & BB_PATH_B != 0 { 0x3231 } else { 0x1111 })?;
        r.w32m(REG_CDDTXP, (1 << 19) | (1 << 18), 0x3)?;
        r.w32m(REG_TXPSEL, (1 << 29) | (1 << 28), 0x1)?;
        r.w32m(REG_TXPSEL, 1 << 30, 0x1)?;
        if tx & BB_PATH_A != 0 {
            r.w32m(REG_CDDTXP, 0xfff0_0000, 0x001)?;
            r.w32m(REG_ADCINI, 0xf000_0000, 0x8)?;
        } else if tx & BB_PATH_B != 0 {
            r.w32m(REG_CDDTXP, 0xfff0_0000, 0x002)?;
            r.w32m(REG_ADCINI, 0xf000_0000, 0x4)?;
        }
        r.w32m(REG_TXPSEL1, 0xfff0, if tx == BB_PATH_A || tx == BB_PATH_B { 0x01 } else { 0x43 })?;
        r.w32m(REG_TXPSEL, MASKBYTE0, ((tx << 4) | tx) as u32)?;
        r.w32m(REG_RXDESC, 1 << 22, 0)?;
        r.w32m(REG_RXDESC, 1 << 18, 0)?;
        if rx & BB_PATH_A != 0 {
            r.w32m(REG_ADCINI, 0x0f00_0000, 0x0)?;
        } else if rx & BB_PATH_B != 0 {
            r.w32m(REG_ADCINI, 0x0f00_0000, 0x5)?;
        }
        r.w32m(REG_RXPSEL, MASKBYTE0, ((rx << 4) | rx) as u32)?;
        let two = (rx != BB_PATH_A && rx != BB_PATH_B) as u32;
        r.w32m(REG_ANTWT, 1 << 16, two)?;
        r.w32m(REG_HTSTFWT, 1 << 28, two)?;
        r.w32m(REG_MRC, 1 << 23, two)?;
        let mut ok = false;
        for _ in 0..100 {
            r.rf_write(0, RF_LUTWE, RFREG_MASK, 0x80000)?;
            r.rf_write(0, RF_LUTWA, RFREG_MASK, 0x00001)?;
            delay_us(2);
            if r.rf_read(0, 0x33, RFREG_MASK)? == 0x00001 {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(err("could not write the RF mode table"));
        }
        r.rf_write(0, RF_LUTWE, RFREG_MASK, 0x80000)?;
        r.rf_write(0, RF_LUTWA, RFREG_MASK, 0x00001)?;
        r.rf_write(0, RF_LUTWD1, RFREG_MASK, 0x00034)?;
        r.rf_write(0, RF_LUTWD0, RFREG_MASK, 0x4080c)?;
        r.rf_write(0, RF_LUTWE, RFREG_MASK, 0x00000)?;
        r.rf_write(0, RF_LUTWE, RFREG_MASK, 0x00000)?;
        self.toggle_igi(rx)?;
        self.set_channel_cca(1, Bw::B20, rx)?;
        self.set_channel_rfe(ch, rx, tx)
    }

    /// 같은 중심과 폭에서 주 20 MHz 채널만 바꿉니다: BB가 받는 주 채널(REG_ADCCLK)과 MAC이 보내는
    /// 부대역(REG_DATA_SC). RF와 이득은 그대로라 USB 전송 몇 번이면 됩니다 (AWDL 상대의 슬롯을 따라
    /// 80 MHz 안에서 옮길 때).
    pub fn set_primary(&self, c: Channel) -> io::Result<()> {
        self.set_bb_primary(c)?;
        self.set_mac_primary(c)
    }

    /// rtw8822b_set_channel_bb의 폭과 주 채널 부분.
    fn set_bb_primary(&self, c: Channel) -> io::Result<()> {
        let r = &self.regs;
        let mut v = r.r32(REG_ADCCLK)?;
        match c.bw {
            Bw::B20 => v &= 0xffcf_fc00,
            Bw::B40 => {
                if c.sc == 1 {
                    r.set32(REG_RXSB, 1 << 4)?;
                } else {
                    r.clr32(REG_RXSB, 1 << 4)?;
                }
                v = (v & 0xff3f_f300) | (((c.sc as u32 & 0xf) << 2) | 1);
            }
            Bw::B80 => v = (v & 0xfcef_cf00) | (((c.sc as u32 & 0xf) << 2) | 2),
        }
        r.w32(REG_ADCCLK, v)
    }

    /// rtw_set_channel_mac의 송신 부대역.
    fn set_mac_primary(&self, c: Channel) -> io::Result<()> {
        let txsc20 = c.sc as u32;
        let txsc40 = if c.bw == Bw::B80 {
            if txsc20 == 1 || txsc20 == 3 { 9 } else { 10 }
        } else {
            0
        };
        self.regs.w8(REG_DATA_SC, ((txsc20 & 0xf) | ((txsc40 & 0xf) << 4)) as u8)
    }

    /// rtw8822b_set_channel과 rtw_set_channel_mac.
    pub fn set_channel(&self, c: Channel) -> io::Result<()> {
        let r = &self.regs;
        let ch = c.center;
        let paths = if self.rf_paths > 1 { BB_PATH_AB } else { BB_PATH_A };
        // --- BB
        if is_2g(ch) {
            r.w32m(REG_RXPSEL, 1 << 28, 0x1)?;
            r.w32m(REG_CCK_CHECK, 1 << 7, 0x0)?;
            r.w32m(REG_ENTXCCK, 1 << 18, 0x0)?;
            r.w32m(REG_RXCCAMSK, 0x0000_fc00, 15)?;
            r.w32m(REG_ACGG2TBL, 0x1f, 0x0)?;
            r.w32m(REG_CLKTRK, 0x1ffe_0000, 0x96a)?;
            if ch == 14 {
                r.w32m(REG_TXSF2, MASKDWORD, 0x0000_6577)?;
                r.w32m(REG_TXSF6, MASKLWORD, 0x0000)?;
            } else {
                r.w32m(REG_TXSF2, MASKDWORD, 0x384f_6577)?;
                r.w32m(REG_TXSF6, MASKLWORD, 0x1525)?;
            }
            r.w32m(REG_RFEINV, 0x300, 0x2)?;
        } else {
            r.w32m(REG_ENTXCCK, 1 << 18, 0x1)?;
            r.w32m(REG_CCK_CHECK, 1 << 7, 0x1)?;
            r.w32m(REG_RXPSEL, 1 << 28, 0x0)?;
            r.w32m(REG_RXCCAMSK, 0x0000_fc00, 34)?;
            let g = if band_5g_1(ch) || band_5g_2(ch) {
                1
            } else if band_5g_3(ch) {
                2
            } else {
                3
            };
            r.w32m(REG_ACGG2TBL, 0x1f, g)?;
            let clk = if band_5g_1(ch) {
                0x494
            } else if band_5g_2(ch) {
                0x453
            } else if (100..=116).contains(&ch) {
                0x452
            } else {
                0x412
            };
            r.w32m(REG_CLKTRK, 0x1ffe_0000, clk)?;
            r.w32m(0xcbc, 0x300, 0x1)?;
        }
        self.set_bb_primary(c)?;
        r.w32m(REG_ADC160, 1 << 30, 0x1)?;
        if c.bw == Bw::B80 && (self.efuse.rfe_option == 2 || self.efuse.rfe_option == 3) {
            r.w32m(REG_L1PKWT, 0x0000_f000, 0x6)?;
            r.w32m(REG_ADC40, 1 << 10, 0x1)?;
        }
        // --- MAC (rtw_set_channel_mac)
        self.set_mac_primary(c)?;
        let mut v = r.r32(REG_WMAC_TRXPTCL_CTL)? & !BIT_RFMOD;
        v |= match c.bw {
            Bw::B80 => BIT_RFMOD_80M,
            Bw::B40 => BIT_RFMOD_40M,
            Bw::B20 => 0,
        };
        r.w32(REG_WMAC_TRXPTCL_CTL, v)?;
        let v = (r.r32(REG_AFE_CTRL1)? & !BIT_MAC_CLK_SEL) | (MAC_CLK_HW_DEF_80M << BIT_SHIFT_MAC_CLK_SEL);
        r.w32(REG_AFE_CTRL1, v)?;
        r.w8(REG_USTIME_TSF, MAC_CLK_SPEED as u8)?;
        r.w8(REG_USTIME_EDCA, MAC_CLK_SPEED as u8)?;
        let mut v = r.r8(REG_CCK_CHECK)? & !(BIT_CHECK_CCK_EN as u8);
        if !is_2g(ch) {
            v |= BIT_CHECK_CCK_EN as u8;
        }
        r.w8(REG_CCK_CHECK, v)?;
        // --- RF
        let mut rf18 = r.rf_read(0, 0x18, RFREG_MASK)?;
        rf18 &= !(((1 << 16) | (1 << 9) | (1 << 8)) | 0xff | ((1 << 18) | (1 << 17)) | ((1 << 11) | (1 << 10)));
        if !is_2g(ch) {
            rf18 |= (1 << 16) | (1 << 8);
        }
        rf18 |= ch as u32;
        if ch > 144 {
            rf18 |= 1 << 18;
        } else if ch >= 80 {
            rf18 |= 1 << 17;
        }
        rf18 |= match c.bw {
            Bw::B20 => (1 << 11) | (1 << 10),
            Bw::B40 => 1 << 11,
            Bw::B80 => 1 << 10,
        };
        const LOW: [u8; 15] = [0x7, 0x6, 0x6, 0x5, 0x0, 0x0, 0x7, 0xff, 0x6, 0x5, 0x0, 0x0, 0x7, 0x6, 0x6];
        const MIDDLE: [u8; 23] = [
            0x6, 0x5, 0x0, 0x0, 0x7, 0x6, 0x6, 0xff, 0x0, 0x0, 0x7, 0x6, 0x6, 0x5, 0x0, 0xff, 0x7, 0x6, 0x6, 0x5, 0x0, 0x0, 0x7,
        ];
        const HIGH: [u8; 15] = [0x5, 0x5, 0x0, 0x7, 0x7, 0x6, 0x5, 0xff, 0x0, 0x7, 0x7, 0x6, 0x5, 0x5, 0x0];
        let be = if is_2g(ch) {
            0
        } else if band_5g_1(ch) || band_5g_2(ch) {
            LOW[((ch - 36) >> 1) as usize]
        } else if band_5g_3(ch) {
            MIDDLE[((ch - 100) >> 1) as usize]
        } else if band_5g_4(ch) {
            HIGH[((ch - 149) >> 1) as usize]
        } else {
            return Err(err(format!("channel {ch} is not supported")));
        };
        r.rf_write(0, RF_MALSEL, (1 << 17) | (1 << 16) | (1 << 15), be as u32)?;
        r.rf_write(0, RF_LUTDBG, 1 << 18, (ch == 144) as u32)?;
        r.rf_write(0, 0x18, RFREG_MASK, rf18)?;
        if self.rf_paths > 1 {
            r.rf_write(1, 0x18, RFREG_MASK, rf18)?;
        }
        r.rf_write(0, RF_XTALX2, 1 << 19, 0)?;
        r.rf_write(0, RF_XTALX2, 1 << 19, 1)?;
        // --- RX DFIR
        match c.bw {
            Bw::B40 => {
                r.w32m(REG_ACBB0, (1 << 29) | (1 << 28), 0x1)?;
                r.w32m(REG_ACBBRXFIR, (1 << 29) | (1 << 28), 0x0)?;
                r.w32sm(REG_TXDFIR, 1 << 31, 0x0)?;
            }
            Bw::B80 => {
                r.w32m(REG_ACBB0, (1 << 29) | (1 << 28), 0x2)?;
                r.w32m(REG_ACBBRXFIR, (1 << 29) | (1 << 28), 0x1)?;
                r.w32sm(REG_TXDFIR, 1 << 31, 0x0)?;
            }
            Bw::B20 => {
                r.w32m(REG_ACBB0, (1 << 29) | (1 << 28), 0x2)?;
                r.w32m(REG_ACBBRXFIR, (1 << 29) | (1 << 28), 0x2)?;
                r.w32sm(REG_TXDFIR, 1 << 31, 0x1)?;
            }
        }
        self.toggle_igi(paths)?;
        self.set_channel_cca(ch, c.bw, paths)?;
        self.set_channel_rfe(ch, paths, paths)
    }

    /// 모든 경로의 모든 속도에 같은 송신 출력 지수 (TXAGC, 0-63).
    pub fn set_tx_power(&self, index: u8) -> io::Result<()> {
        let v = u32::from_le_bytes([index & 0x3f; 4]);
        for path in 0..self.rf_paths {
            let base = [0x1d00u32, 0x1d80][path];
            // 속도 0x00-0x53 (CCK, OFDM, HT MCS0-15, VHT 1SS/2SS), 레지스터마다 넷.
            for rate in (0..0x54u32).step_by(4) {
                self.regs.w32(base + rate, v)?;
            }
        }
        Ok(())
    }

    /// rtw8822b_do_iqk: 펌웨어가 이 채널에서 IQ 불균형과 LO 누설을 보정합니다.
    pub fn iqk(&self) -> io::Result<u32> {
        let r = &self.regs;
        self.h2c_pkt(0x0e, &[0])?;
        let mut n = 0;
        while n < 300 {
            if r.rf_read(0, RF_DTXLOK, RFREG_MASK)? == 0xabcde {
                break;
            }
            delay_ms(20);
            n += 1;
        }
        r.rf_write(0, RF_DTXLOK, RFREG_MASK, 0)?;
        let fail = r.r32m(REG_IQKFAILMSK, 0xff)?;
        tracing::info!("IQK done after {} ms, fail mask 0x{fail:02x}", n * 20);
        Ok(fail)
    }

    /// rtw_coex_power_on_setting + rtw_coex_init_hw_config(wifi_only): 안테나는 Wi-Fi 것입니다.
    pub fn coex_wifi_only(&self) -> io::Result<()> {
        let r = &self.regs;
        r.set8(REG_SYS_FUNC_EN, BIT_FEN_BB_GLB_RST | BIT_FEN_BB_RSTB)?;
        let ind = |addr: u32, mask: u32, val: u32| -> io::Result<()> {
            let t = self.ltecoex_read(addr)?;
            self.ltecoex_write(addr, (t & !mask) | ((val << mask.trailing_zeros()) & mask))
        };
        // rtw8822b_coex_cfg_rfe_type
        r.w8(REG_RFE_CTRL_E, 0xff)?;
        r.w8m(REG_RFESEL_CTRL + 1, 0x3, 0)?;
        r.w8m(REG_RFE_INV16, BIT_RFE_BUF_EN, 0)?;
        ind(LTE_COEX_CTRL, BIT_LTE_COEX_EN, 0)?;
        ind(LTE_WL_TRX_CTRL, MASKLWORD, 0xffff)?;
        ind(LTE_BT_TRX_CTRL, MASKLWORD, 0xffff)?;
        // rtw8822b_coex_cfg_init
        r.set8(REG_BCN_CTRL, BIT_EN_BCN_FUNCTION)?;
        r.w8m(REG_BT_TDMA_TIME, BIT_MASK_SAMPLE_RATE, 0x5)?;
        r.w8(REG_BT_STAT_CTRL, 0x1)?;
        r.set32(REG_GPIO_MUXCFG, BIT_BT_PTA_EN)?;
        r.set32(REG_GPIO_MUXCFG, BIT_PO_BT_PTA_PINS)?;
        r.set8(REG_QUEUE_CTRL, BIT_PTA_WL_TX_EN)?;
        r.clr8(REG_QUEUE_CTRL, BIT_PTA_EDCCA_EN)?;
        r.set16(REG_BT_COEX_V2, BIT_GNT_BT_POLARITY)?;
        // 송신 응답(ACK, BA, CTS)과 비콘을 Wi-Fi 높은 우선순위로
        for bit in [3u32, 4, 27] {
            r.w8m(REG_BT_COEX_TABLE_H + bit / 8, 1 << (bit % 8), 1)?;
        }
        // COEX_SET_ANT_WONLY: GNT_BT 소프트웨어 low, GNT_WL 소프트웨어 high, 경로는 Wi-Fi 것,
        // 안테나 스위치(RFE ctrl 8/9의 DPDT)는 Wi-Fi 쪽으로.
        ind(LTE_COEX_CTRL, 0xc000, 1)?;
        ind(LTE_COEX_CTRL, 0x0c00, 1)?;
        ind(LTE_COEX_CTRL, 0x3000, 3)?;
        ind(LTE_COEX_CTRL, 0x0300, 3)?;
        r.set8(REG_SYS_SDIO_CTRL + 3, BIT_LTE_MUX_CTRL_PATH >> 24)?;
        if !matches!(self.efuse.rfe_option, 0x12 | 0x15 | 0x16) {
            r.w8m(REG_LED_CFG + 2, BIT_DPDT_SEL_EN >> 16, 0)?;
            r.w8m(REG_LED_CFG + 3, BIT_DPDT_WL_SEL >> 24, 1)?;
            r.w8m(REG_RFE_CTRL8, BIT_MASK_RFE_SEL89, 0x77)?;
            r.w8m(REG_RFE_INV8, BIT_MASK_RFE_INV89, 0x2)?;
        }
        r.w8(0xff1a, 0)?;
        // rtw8822b_coex_cfg_gnt_debug
        r.w8m(REG_PAD_CTRL1 + 2, BIT_BTGP_SPI_EN >> 16, 0)?;
        r.w8m(REG_PAD_CTRL1 + 3, BIT_BTGP_JTAG_EN >> 24, 0)?;
        r.w8m(REG_GPIO_MUXCFG + 2, BIT_FSPI_EN >> 16, 0)?;
        r.w8m(REG_PAD_CTRL1 + 1, BIT_LED1DIS >> 8, 0)?;
        r.w8m(REG_SYS_SDIO_CTRL + 3, BIT_DBG_GNT_WL_BT >> 24, 0)
    }

    /// 모든 경로의 초기 이득 (IGI).
    pub fn set_igi(&self, igi: u8) -> io::Result<()> {
        self.regs.w32m(0xc50, 0x7f, igi as u32)?;
        if self.rf_paths > 1 {
            self.regs.w32m(0xe50, 0x7f, igi as u32)?;
        }
        Ok(())
    }

    pub fn igi(&self) -> io::Result<u8> {
        Ok(self.regs.r32m(0xc50, 0x7f)? as u8)
    }

    /// rtw8822b_false_alarm_statistics: 지난 호출 뒤 OFDM (켜져 있으면 CCK도) 오경보.
    pub fn false_alarms(&self) -> io::Result<u32> {
        Ok(self.counters()?.false_alarms)
    }

    /// rtw8822b_false_alarm_statistics: 지난 호출 뒤 채널의 모든 송신자에 대한 오경보와 MPDU CRC
    /// 결과. 그다음 카운터를 리셋합니다 (REG_CNTRST).
    pub fn counters(&self) -> io::Result<PhyCounters> {
        let r = &self.regs;
        let cck_on = r.r32(0x808)? & (1 << 28) != 0;
        let cck = r.r16(0xa5c)? as u32;
        let ofdm = r.r16(0xf48)? as u32;
        let crc = |reg: u32| -> io::Result<(u32, u32)> {
            let v = r.r32(reg)?;
            Ok((v & 0xffff, v >> 16))
        };
        let c = PhyCounters {
            false_alarms: ofdm + if cck_on { cck } else { 0 },
            ofdm: crc(REG_CRC_OFDM)?,
            ht: crc(REG_CRC_HT)?,
            vht: crc(REG_CRC_VHT)?,
        };
        r.set32(0x9a4, 1 << 17)?;
        r.clr32(0x9a4, 1 << 17)?;
        r.clr32(0xa2c, 1 << 15)?;
        r.set32(0xa2c, 1 << 15)?;
        r.set32(0xb58, 1 << 0)?;
        r.clr32(0xb58, 1 << 0)?;
        Ok(c)
    }

    /// rtw_bf_enable_bfee_su: `bfer`의 측정(NDPA + NDP)에 압축 빔포밍 보고로 답해, 상대가 우리
    /// 쪽으로 빔을 맞추게 합니다. `sound_dim`은 측정 차원 - 1.
    pub fn bfee_su(&self, bfer: [u8; 6], sound_dim: u8) -> io::Result<()> {
        let r = &self.regs;
        let nc = (self.rf_paths as u32) - 1;
        let csi = (3u32 << 10) | (1 << 8) | ((sound_dim as u32 & 7) << 3) | nc;
        r.w8m(REG_SND_PTCL_CTRL, BIT_MASK_BEAMFORM, RTW_SND_CTRL_SOUNDING as u8)?;
        for (i, b) in bfer.iter().enumerate() {
            r.w8(REG_ASSOCIATED_BFMER0_INFO + i as u32, *b)?;
        }
        r.w16(REG_TX_CSI_RPT_PARAM_BW20, csi as u16)?;
        r.w8(REG_SND_PTCL_CTRL + 3, RTW_NDP_RX_STANDBY_TIME as u8)
    }
}
