//! 채널 설정과 그 전의 DM 초기화 (rtw8922a_set_channel, rtw8922a_set_channel_help, rtw8922a_ctrl_mlo,
//! rtw8922a_chlk_reload, rtw89_phy_dm_init의 하드웨어 부분). 두 RF 경로를 PHY 0 하나에 묶는 rtw89의 단일
//! 링크 방식(MLO_2_PLUS_0_1RF)을 따릅니다: 경로 B는 경로 A의 합성기를 같이 씁니다.
use std::io;
use std::thread::sleep;
use std::time::Duration;

use super::generated::regs::*;
use super::phy::{INV_RF_DATA, RFREG_MASK, phy1};
use super::{CHIP_CAV, Rtl8922a, put};

/// RF 보정 결과를 채널별로 담아 두는 표의 수 (__RTW89_RFK_CHS_NR_V1).
pub(super) const RFK_TABLES: usize = 3;
const HE_N_USER_MAX: u32 = 4;
/// rtw89_ch_base_table: 채널 번호를 BB의 채널 색인으로 바꿀 때의 기준 채널.
const CH_BASE: [u8; 16] = [1, 0xff, 36, 100, 132, 149, 0xff, 1, 33, 65, 97, 129, 161, 193, 225, 0xff];
const XTAL_SI_XTAL_PLL: u8 = 0x16;
const XTAL_SI_APBT: u8 = 0xd1;
const XTAL_SI_PLL_1: u8 = 0xe1;
/// rtw89_mac_cfg_phy_rpt_be: MAC_AX_PHY_RPT_SIZE_8.
const PHY_RPT_SIZE_8: u32 = 1;
/// 블루투스 공존 (coex.h): 지그비 공존 표와 끊기 표.
const R_BTC_ZB_COEX_TBL_0: u32 = 0xe328;
const R_BTC_ZB_COEX_TBL_1: u32 = 0xe32c;
const R_BTC_ZB_BREAK_TBL: u32 = 0xe350;
/// RF 송수신 마스크 표의 묶음 (BTC_BT_SS/TX/RX_GROUP).
const BTC_BT_SS_GROUP: u32 = 0x0;
const BTC_BT_TX_GROUP: u32 = 0x2;
const BTC_BT_RX_GROUP: u32 = 0x3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    G2,
    G5,
    G6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bw {
    B20,
    B40,
    B80,
    B160,
}

/// 채널: 중심 채널 번호, 주 20 MHz 채널 번호, 대역폭 (rtw89_chan).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chan {
    pub band: Band,
    pub center: u8,
    pub primary: u8,
    pub bw: Bw,
}

fn mhz(band: Band, ch: u8) -> u32 {
    let ch = ch as u32;
    match band {
        Band::G2 if ch == 14 => 2484,
        Band::G2 => 2407 + 5 * ch,
        Band::G5 => 5000 + 5 * ch,
        Band::G6 if ch == 2 => 5935,
        Band::G6 => 5950 + 5 * ch,
    }
}

impl Chan {
    pub fn new(band: Band, center: u8, primary: u8, bw: Bw) -> Self {
        Chan { band, center, primary, bw }
    }

    /// 20 MHz 채널.
    pub fn narrow(band: Band, ch: u8) -> Self {
        Chan::new(band, ch, ch, Bw::B20)
    }

    /// 2.4/5 GHz 주 채널 primary와 폭으로 채널 (중심은 그 폭의 표준 묶음에서).
    pub fn around(primary: u8, bw: Bw) -> Self {
        let band = if primary <= 14 { Band::G2 } else { Band::G5 };
        if band == Band::G2 || bw == Bw::B20 {
            return Chan::new(band, primary, primary, Bw::B20);
        }
        // 5 GHz 묶음의 첫 채널들 (40/80/160 MHz).
        let starts: &[u8] = match bw {
            Bw::B40 => &[36, 44, 52, 60, 100, 108, 116, 124, 132, 140, 149, 157, 165, 173],
            Bw::B80 => &[36, 52, 100, 116, 132, 149, 165],
            _ => &[36, 100, 149],
        };
        let span = 4 * match bw {
            Bw::B40 => 2,
            Bw::B80 => 4,
            _ => 8,
        };
        match starts.iter().find(|&&s| primary >= s && primary < s + span) {
            Some(&s) => Chan::new(band, s + (span - 4) / 2, primary, bw),
            None => Chan::new(band, primary, primary, Bw::B20),
        }
    }

    /// 중심 주파수 (MHz).
    pub fn freq(&self) -> u32 {
        mhz(self.band, self.center)
    }

    /// BB 이득 표의 대역 (rtw89_get_subband_type, rtw89_subband_to_gain_band_be).
    fn gain_band(&self) -> usize {
        match (self.band, self.center) {
            (Band::G2, _) => 0,
            (Band::G5, 100..=144) => 2,
            (Band::G5, 149..=177) => 3,
            (Band::G5, _) => 1,
            (Band::G6, c) => 4 + (c.saturating_sub(1) as usize / 32).min(7),
        }
    }

    /// 주 채널이 몇 번째 20 MHz인지 (rtw89_get_primary_sb_idx).
    fn pri_sb(&self) -> u32 {
        let ofst = [0, 2, 6, 14][self.bw as usize];
        ((ofst + self.primary as i32 - self.center as i32) / 4) as u32
    }

    /// dbw 폭으로 보낼 때의 부반송 대역 자리 (rtw89_phy_get_txsb).
    fn txsb(&self, dbw: Bw) -> u32 {
        let (pri, c) = (self.primary as i32, self.center as i32);
        if self.bw == dbw || self.bw == Bw::B20 {
            return 0;
        }
        let v = match (self.bw, dbw) {
            (Bw::B80, Bw::B20) => (pri - c + 6) / 4,
            (Bw::B160, Bw::B20) => (pri - c + 14) / 4,
            (Bw::B160, Bw::B40) => (pri - c + 12) / 8,
            _ => (pri > c) as i32,
        };
        v as u32
    }

    /// RF 0x18 (rtw8922a_chan_to_rf18_val).
    pub(super) fn rf18(&self) -> u32 {
        let mut v = put(0, RR_CFGCH_CH, self.center as u32);
        match self.band {
            Band::G2 => {}
            Band::G5 => v |= put(0, RR_CFGCH_BAND1, CFGCH_BAND1_5G) | put(0, RR_CFGCH_BAND0, CFGCH_BAND0_5G),
            Band::G6 => v |= put(0, RR_CFGCH_BAND1, CFGCH_BAND1_6G) | put(0, RR_CFGCH_BAND0, CFGCH_BAND0_6G),
        }
        let bw = match self.bw {
            Bw::B20 => CFGCH_BW_V2_20M,
            Bw::B40 => CFGCH_BW_V2_40M,
            Bw::B80 => CFGCH_BW_V2_80M,
            Bw::B160 => CFGCH_BW_V2_160M,
        };
        v | put(0, RR_CFGCH_BW_V2, bw)
    }

    /// BB의 채널 색인 (rtw89_encode_chan_idx, 주 채널로).
    fn chan_idx(&self) -> u32 {
        let ch = self.primary;
        let range = match self.band {
            Band::G2 => return ch as u32,
            Band::G5 => 2..=5,
            Band::G6 => 7..=14,
        };
        let Some(idx) = range.rev().find(|&i| ch >= CH_BASE[i]) else {
            return 0;
        };
        ((idx as u32) << 4) | ((ch - CH_BASE[idx]) as u32 >> 1)
    }
}

/// R_BE_TX_SUB_BAND_VALUE: 좁은 폭으로 보낼 때의 부대역 자리와 주 20 MHz 비트맵
/// (rtw8922a_set_channel_mac).
fn tx_sub_band(c: &Chan) -> u32 {
    let (sb20, sb40, sb80) = match c.bw {
        Bw::B160 => (c.txsb(Bw::B20), c.txsb(Bw::B40), c.txsb(Bw::B80)),
        Bw::B80 => (c.txsb(Bw::B20), c.txsb(Bw::B40), 0),
        Bw::B40 => (c.txsb(Bw::B20), 0, 0),
        Bw::B20 => (0, 0, 0),
    };
    let mut txsb = match c.bw {
        Bw::B160 => put(0, B_BE_TXSB_20M_MASK, sb20) | put(0, B_BE_TXSB_40M_MASK, sb40) | put(0, B_BE_TXSB_80M_MASK, sb80),
        Bw::B80 => put(0, B_BE_TXSB_20M_MASK, sb20) | put(0, B_BE_TXSB_40M_MASK, sb40),
        Bw::B40 => put(0, B_BE_TXSB_20M_MASK, sb20),
        Bw::B20 => 0,
    };
    if sb20 <= BE_PRI20_BITMAP_MAX {
        txsb |= put(0, B_BE_PRI20_BITMAP_MASK, 1 << sb20);
    }
    txsb
}

/// 처음 켤 때의 채널 (rtw89가 채널을 정하기 전에 쓰는 2.4 GHz 1번).
const BOOT_CHAN: Chan = Chan {
    band: Band::G2,
    center: 1,
    primary: 1,
    bw: Bw::B20,
};

/// 이득 보정을 쓰는 BB 자리: (2.4 GHz [경로 A, B], 5/6 GHz [경로 A, B], 2.4 GHz mask, 5/6 GHz mask).
type GainReg = ([u32; 2], [u32; 2], u32, u32);
const BB_GAIN_LNA: [GainReg; 7] = [
    ([0x409c, 0x449c], [0x406c, 0x446c], 0xff00, 0xff),
    ([0x409c, 0x449c], [0x406c, 0x446c], 0xff00_0000, 0xff_0000),
    ([0x40a0, 0x44a0], [0x4070, 0x4470], 0xff00, 0xff),
    ([0x40a0, 0x44a0], [0x4070, 0x4470], 0xff00_0000, 0xff_0000),
    ([0x40a4, 0x44a4], [0x4074, 0x4474], 0xff00, 0xff),
    ([0x40a4, 0x44a4], [0x4074, 0x4474], 0xff00_0000, 0xff_0000),
    ([0x40a8, 0x44a8], [0x4078, 0x4478], 0xff00, 0xff),
];
const BB_GAIN_TIA: [GainReg; 2] = [
    ([0x4054, 0x4454], [0x4054, 0x4454], 0x7fc_0000, 0x1ff),
    ([0x4058, 0x4458], [0x4054, 0x4454], 0x1ff, 0x3_fe00),
];
const BB_OP1DB_LNA: [GainReg; 7] = [
    ([0x40ac, 0x44ac], [0x4078, 0x4478], 0xff00, 0xff00_0000),
    ([0x40ac, 0x44ac], [0x407c, 0x447c], 0xff_0000, 0xff),
    ([0x40ac, 0x44ac], [0x407c, 0x447c], 0xff00_0000, 0xff00),
    ([0x40b0, 0x44b0], [0x407c, 0x447c], 0xff, 0xff_0000),
    ([0x40b0, 0x44b0], [0x407c, 0x447c], 0xff00, 0xff00_0000),
    ([0x40b0, 0x44b0], [0x4080, 0x4480], 0xff_0000, 0xff),
    ([0x40b0, 0x44b0], [0x4080, 0x4480], 0xff00_0000, 0xff00),
];
const BB_OP1DB_TIA_LNA: [GainReg; 8] = [
    ([0x40b4, 0x44b4], [0x4080, 0x4480], 0xff_0000, 0xff00_0000),
    ([0x40b4, 0x44b4], [0x4084, 0x4484], 0xff00_0000, 0xff),
    ([0x40b8, 0x44b8], [0x4084, 0x4484], 0xff, 0xff00),
    ([0x40b8, 0x44b8], [0x4084, 0x4484], 0xff00, 0xff_0000),
    ([0x40b8, 0x44b8], [0x4084, 0x4484], 0xff_0000, 0xff00_0000),
    ([0x40b8, 0x44b8], [0x4088, 0x4488], 0xff00_0000, 0xff),
    ([0x40bc, 0x44bc], [0x4088, 0x4488], 0xff, 0xff00),
    ([0x40bc, 0x44bc], [0x4088, 0x4488], 0xff00, 0xff_0000),
];
/// 수신 세기 보정 자리 (rpl_comp_bw160/80/40/20; 경로 B는 주소에 0x400).
const RPL_160: [(u32, u32); 8] = [
    (0x41e8, 0xff00),
    (0x41e8, 0xff_0000),
    (0x41e8, 0xff00_0000),
    (0x41ec, 0xff),
    (0x41ec, 0xff00),
    (0x41ec, 0xff_0000),
    (0x41ec, 0xff00_0000),
    (0x41f0, 0xff),
];
const RPL_80: [(u32, u32); 4] = [(0x41f4, 0xff), (0x41f4, 0xff00), (0x41f4, 0xff_0000), (0x41f4, 0xff00_0000)];
const RPL_40: [(u32, u32); 2] = [(0x41f0, 0xff_0000), (0x41f0, 0xff00_0000)];
const RPL_20: [(u32, u32); 1] = [(0x41f0, 0xff00)];
const SCO_BARKER: [u32; 14] = [
    0x1fe4f, 0x1ff5e, 0x2006c, 0x2017b, 0x2028a, 0x20399, 0x204a8, 0x205b6, 0x206c5, 0x207d4, 0x208e3, 0x209f2, 0x20b00, 0x20d8a,
];
const SCO_CCK: [u32; 14] = [
    0x2bdac, 0x2bf21, 0x2c095, 0x2c209, 0x2c37e, 0x2c4f2, 0x2c666, 0x2c7db, 0x2c94f, 0x2cac3, 0x2cc38, 0x2cdac, 0x2cf21, 0x2d29e,
];
/// 송신 경로 A+B의 TMAC 설정 (rtw8922a_ctrl_tx_path_tmac).
const TX_PATH_AB: [(u32, u32); 7] = [
    (0x11a00, 0x21c8_6900),
    (0x11a04, 0x00e4_e433),
    (0x11a08, 0x3939_0cc9),
    (0x11a0c, 0x4e43_3240),
    (0x11a10, 0x90cc_900e),
    (0x11a14, 0x0024_0393),
    (0x11a18, 0x201c_8600),
];
/// 두 경로, 2개 공간 스트림의 디지털 전력 보정 (rtw8922a_digital_pwr_comp_*_val[1]).
const PWR_COMP_2G_S0: [u32; 22] = [
    0x012C0064, 0x04B00258, 0x00432710, 0x019000A7, 0x06400320, 0x0D05091D, 0x14D50FA0, 0x00000000, 0x01010100, 0x00000101, 0x01000000, 0x01010101,
    0x01010000, 0x02020202, 0x00000404, 0x03020101, 0x04040303, 0x02010000, 0x03030303, 0x00000505, 0x03030201, 0x05050303,
];
const PWR_COMP_2G_S1: [u32; 22] = [
    0x012C0064, 0x04B00258, 0x00432710, 0x019000A7, 0x06400320, 0x0D05091D, 0x14D50FA0, 0x00000000, 0x01010100, 0x00000101, 0x01010000, 0x02020201,
    0x02010100, 0x03030202, 0x01000404, 0x04030201, 0x05050404, 0x01010100, 0x04030303, 0x01000505, 0x03030101, 0x05050404,
];
const PWR_COMP_5G: [u32; 22] = [
    0x012C0096, 0x044C02BC, 0x00322710, 0x015E0096, 0x03C8028A, 0x0BB80708, 0x17701194, 0x04030201, 0x05050505, 0x01000505, 0x07060504, 0x09090908,
    0x09070400, 0x0E0D0C0B, 0x03000E0E, 0x0D0B0907, 0x1010100F, 0x0B080500, 0x1512100D, 0x05001515, 0x100D0B08, 0x15151512,
];

impl Rtl8922a {
    fn w8_mask(&self, a: u32, mask: u32, x: u32) -> io::Result<()> {
        self.w8(a, put(self.r8(a)? as u32, mask, x) as u8)
    }

    /// rtw89_phy_dm_init의 하드웨어 부분과 rtw89_core_start의 나머지 (PHY 상태 보고 켜기).
    /// [Rtl8922a::phy_init] 다음에.
    pub fn dm_init(&mut self, file: &[u8]) -> io::Result<()> {
        // rtw8922a_bb_sethw
        self.pclr32(R_EN_SND_WO_NDP, B_EN_SND_WO_NDP)?;
        self.pclr32(R_EN_SND_WO_NDP_C1, B_EN_SND_WO_NDP)?;
        self.w32_mask(R_BE_PWR_BOOST, B_BE_PWR_CTRL_SEL, 0)?;
        self.ctrl_mlo(&BOOT_CHAN)?;
        // rtw89_btc_ntfy_init: rtw8922a_btc_init_cfg, 그리고 블루투스가 없으니 Wi-Fi 전용 안테나.
        self.btc_init_cfg()?;
        self.btc_wl_only()?;
        self.cfo_init()?;
        self.bb_wrap_init()?;
        // rtw8922a_rfk_hw_init: rfk_mlo_ctrl, 그리고 PLL
        self.set_syn_a_only()?;
        self.chlk_reload(&BOOT_CHAN)?;
        self.rfk_pll_init()?;
        self.init_rf_nctl(file)?;
        // rtw8922a_bb_cfg_txrx_path: 송수신 모두 경로 A+B, 2 스트림.
        self.hal_reset(BOOT_CHAN.band, true)?;
        self.ctrl_trx_path()?;
        self.hal_reset(BOOT_CHAN.band, false)?;
        // rtw89_core_start: PPDU 상태와 PHY 보고 (8922A는 EDCCA 모드를 바꾸지 않음).
        self.cfg_ppdu_status(true)?;
        self.cfg_phy_rpt(true)
    }

    /// rtw89_phy_cfo_init과 rtw89_dcfo_comp_init: 공장에서 eFuse에 적어 둔 크리스털 보정값으로 주파수를
    /// 맞추고, 디지털 CFO 보정을 켭니다. 이 칩은 전원이 들어오면 보정값 대신 0x40으로 돌아서 (이
    /// 어댑터의 eFuse 값은 0x59) 주파수가 정확한 iPhone과는 수십 kHz가 어긋나고, 그러면 칩이 상대의
    /// ACK부터 알아듣지 못해 같은 프레임을 한도까지 다시 보냅니다.
    fn cfo_init(&self) -> io::Result<()> {
        let cap = self.xtal_k & B_AX_XTAL_SC_MASK as u8;
        // 비어 있는 eFuse(0xff)는 기본값 그대로 둡니다.
        if self.xtal_k != 0xff {
            self.set_xtal_cap(cap)?;
        }
        for a in [R_DCFO_OPT_BE, phy1(R_DCFO_OPT_BE)] {
            self.pw32_mask(a, B_DCFO_OPT_EN_BE, 1)?;
        }
        for a in [R_DCFO_WEIGHT_BE, phy1(R_DCFO_WEIGHT_BE)] {
            self.pw32_mask(a, B_DCFO_WEIGHT_MSK_BE, 8)?;
        }
        tracing::debug!("crystal cap {:02x?} (eFuse {:02x})", self.xtal_cap()?, self.xtal_k);
        Ok(())
    }

    /// rtw89_phy_bb_wrap_init_be (MAC 0): MACID마다의 송신 경로와 출력 한도 표를 비우고, 송신 출력
    /// 표와 강제 설정을 처음 값으로 둡니다. 이 표들은 전원을 켤 때 아무 값이나 들어 있어서, 비우지
    /// 않으면 MACID마다의 송신 출력이 올릴 때마다 달라집니다.
    fn bb_wrap_init(&self) -> io::Result<()> {
        const MACIDS: u32 = 32;
        for i in 0..MACIDS {
            self.w32(R_BE_PWR_MACID_PATH_BASE + 4 * i, 0)?;
        }
        for i in 0..MACIDS {
            self.w32(R_BE_PWR_MACID_LMT_BASE + 4 * i, 0)?;
        }
        // rtw89_phy_bb_wrap_tpu_set_all
        self.w32_mask(R_BE_PWR_FTM_SS, B_BE_PWR_BY_RATE_DBW_ON, 0x3)?;
        for range in [
            R_BE_PWR_BY_RATE..=R_BE_PWR_BY_RATE_END,
            R_BE_PWR_RULMT_START..=R_BE_PWR_RULMT_END,
            R_BE_PWR_RATE_OFST_CTRL..=R_BE_PWR_RATE_OFST_END,
        ] {
            for a in range.step_by(4) {
                self.w32(a, 0)?;
            }
        }
        self.w32_mask(R_BE_PWR_REF_CTRL, B_BE_PWR_OFST_LMT_DB, 0)?;
        self.w32_mask(R_BE_PWR_OFST_LMTBF, B_BE_PWR_OFST_LMTBF_DB, 0)?;
        self.w32_mask(R_BE_PWR_RATE_CTRL, B_BE_PWR_OFST_BYRATE_DB, 0)?;
        self.w32_mask(R_BE_PWR_OFST_RULMT, B_BE_PWR_OFST_RULMT_DB, 0)?;
        self.w32_mask(R_BE_PWR_OFST_SW, B_BE_PWR_OFST_SW_DB, 0)?;
        // rtw89_phy_bb_wrap_force_cr_init
        self.w32_mask(R_BE_PWR_FORCE_LMT, B_BE_PWR_FORCE_LMT_ON, 0)?;
        self.w32_mask(R_BE_PWR_RATE_CTRL, B_BE_FORCE_PWR_BY_RATE_EN, 0)?;
        self.w32_mask(R_BE_PWR_OFST_RULMT, B_BE_PWR_FORCE_RU_ENON, 0)?;
        self.w32_mask(R_BE_PWR_OFST_RULMT, B_BE_PWR_FORCE_RU_ON, 0)?;
        self.w32_mask(R_BE_PWR_FORCE_MACID, B_BE_PWR_FORCE_MACID_ALL, 0)?;
        self.w32_mask(R_BE_PWR_COEX_CTRL, B_BE_PWR_FORCE_COEX_ON, 0)?;
        self.w32_mask(R_BE_PWR_BOOST, B_BE_PWR_FORCE_RATE_ON, 0)?;
        // rtw89_phy_bb_wrap_ftm_init
        self.w32(R_BE_PWR_FTM, 0x00e4_e431)?;
        self.w32_mask(R_BE_PWR_FTM_SS, 0x7, 0)?;
        // rtw89_phy_bb_wrap_ul_pwr (8922A). rtw89는 MAC 1에도 쓰지만 우리는 CMAC 1을 켜지 않습니다.
        self.w32(R_BE_PWR_RSSI_TARGET_LMT, 0x0201_fe00)?;
        self.w32(R_BE_PWR_TH, 0x00ff_ec7e)
    }

    /// 채널을 바꿉니다 (__rtw89_set_channel의 PHY 0 부분; 송신 전력과 RF 보정은 아직).
    pub fn set_channel(&self, c: &Chan) -> io::Result<()> {
        // set_channel_help(enter): rtw8922a_pre_set_channel_bb/rf, hal_reset
        self.pw32_mask(R_DBCC, B_DBCC_EN, 0)?;
        self.emlsr_2_plus_0()?;
        self.set_syn_a_only()?;
        sleep(Duration::from_millis(1));
        self.hal_reset(c.band, true)?;

        self.set_channel_mac(c)?;
        self.set_channel_bb(c)?;
        self.set_channel_rf(c)?;
        if let Some((t, regd)) = &self.txpwr {
            self.set_txpwr(t, c, regd)?;
        }

        // set_channel_help(exit): hal_reset, rtw8922a_post_set_channel_bb/rf
        self.hal_reset(c.band, false)?;
        self.digital_pwr_comp(c.band)?;
        self.ctrl_mlo(c)?;
        self.set_syn_a_only()?;
        self.chlk_reload(c)
    }

    /// 모든 프레임을 받게 합니다: 주소·BSSID·비콘 검사를 끄고 스니퍼 모드 (rtw89_ops_configure_filter의
    /// FIF_OTHER_BSS, FIF_BCN_PRBRESP_PROMISC, FIF_PROBE_REQ, FIF_ALLMULTI와 모니터 모드).
    pub fn set_promisc(&self, crc_errors: bool) -> io::Result<()> {
        let mut f = super::mac::default_rx_fltr()
            & !(B_BE_A_MC | B_BE_A_A1_MATCH | B_BE_A_BCN_CHK_EN | B_BE_A_BC | B_BE_A_BC_CAM_MATCH | B_BE_A_UC_CAM_MATCH);
        f |= B_BE_SNIFFER_MODE;
        if crc_errors {
            f |= B_BE_A_CRC32_ERR;
        }
        let v = self.r32(R_BE_RX_FLTR_OPT)?;
        self.w32(R_BE_RX_FLTR_OPT, (v & B_BE_RX_MPDU_MAX_LEN_MASK) | (f & !B_BE_RX_MPDU_MAX_LEN_MASK))
    }

    /// rtw89_mac_cfg_ppdu_status_be
    pub(super) fn cfg_ppdu_status(&self, en: bool) -> io::Result<()> {
        if !en {
            return self.clr32(R_BE_PPDU_STAT, B_BE_PPDU_STAT_RPT_EN);
        }
        self.w32_mask(R_BE_HW_PPDU_STATUS, B_BE_FWD_PPDU_STAT_MASK, 3)?;
        self.w32(
            R_BE_PPDU_STAT,
            B_BE_PPDU_STAT_RPT_EN | B_BE_APP_RX_CNT_RPT | B_BE_APP_PLCP_HDR_RPT | B_BE_PPDU_STAT_RPT_CRC32 | B_BE_PPDU_STAT_RPT_DMA,
        )
    }

    /// rtw89_mac_cfg_phy_rpt_be
    fn cfg_phy_rpt(&self, en: bool) -> io::Result<()> {
        self.w32_mask(R_BE_RCR, B_BE_PHY_RPT_SZ_MASK, if en { PHY_RPT_SIZE_8 } else { 0 })?;
        self.w32_mask(R_BE_RCR, B_BE_HDR_CNV_SZ_MASK, 0)?;
        self.w32_mask(R_BE_DRV_INFO_OPTION, B_BE_DRV_INFO_PHYRPT_EN, en as u32)
    }

    /// rtw8922a_btc_init_cfg (안테나 2개, 경로 B를 블루투스와 나눠 씀): GNT_WL이 내려가도 Wi-Fi 수신은
    /// 통과하게 하는 RF 송수신 마스크, Wi-Fi 높은 우선순위 프레임, 끊기 표.
    fn btc_init_cfg(&self) -> io::Result<()> {
        for path in 0..2 {
            self.write_rf(path, RR_LUTWE, RFREG_MASK, 1 << 17)?;
            let tx = if path == 1 { 0x55f } else { 0x5ff };
            for (group, val) in [(BTC_BT_SS_GROUP, 0x5ff), (BTC_BT_RX_GROUP, 0x5df), (BTC_BT_TX_GROUP, tx)] {
                self.write_rf(path, RR_LUTWA, RFREG_MASK, group)?;
                self.write_rf(path, RR_LUTWD0, RFREG_MASK, val)?;
            }
            self.write_rf(path, RR_LUTWE, RFREG_MASK, 0)?;
        }
        self.w32(
            R_BTC_COEX_WL_REQ_BE,
            B_BTC_RSP_ACK_HI | B_BTC_TX_BCN_HI | B_BTC_TX_TRI_HI | B_BTC_TX_NULL_HI,
        )?;
        self.w32(R_BE_BT_BREAK_TABLE, BTC_BREAK_PARAM)?;
        self.w32(R_BTC_ZB_COEX_TBL_0, 0xda5a_5a5a)?;
        self.w32(R_BTC_ZB_COEX_TBL_1, 0xda5a_5a5a)?;
        self.w32(R_BTC_ZB_BREAK_TBL, 0xf0ff_ffff)
    }

    /// rtw89 coex의 BTC_ANT_WONLY: 두 대역 모두 GNT_WL을 소프트웨어로 1, GNT_BT를 0, WL_ACT를 1
    /// (_set_gnt(BTC_GNT_SW_HI, BTC_GNT_SW_LO, BTC_WLACT_SW_HI) → rtw89_mac_cfg_gnt_v2).
    fn btc_wl_only(&self) -> io::Result<()> {
        let bt_sw = B_BE_GNT_BT_BB0_SWCTRL
            | B_BE_GNT_BT_RX_BB0_SWCTRL
            | B_BE_GNT_BT_TX_BB0_SWCTRL
            | B_BE_GNT_BT_BB1_SWCTRL
            | B_BE_GNT_BT_RX_BB1_SWCTRL
            | B_BE_GNT_BT_TX_BB1_SWCTRL;
        let wl = B_BE_GNT_WL_BB0_VAL
            | B_BE_GNT_WL_BB1_VAL
            | B_BE_GNT_WL_RX_VAL
            | B_BE_GNT_WL_TX_VAL
            | B_BE_GNT_WL_BB_PWR_VAL
            | B_BE_GNT_WL_BB0_SWCTRL
            | B_BE_GNT_WL_BB1_SWCTRL
            | B_BE_GNT_WL_RX_SWCTRL
            | B_BE_GNT_WL_TX_SWCTRL
            | B_BE_GNT_WL_BB_PWR_SWCTRL;
        self.w32(R_BE_GNT_SW_CTRL, bt_sw | wl | B_BE_WL_ACT_SWCTRL | B_BE_WL_ACT_VAL)
    }

    /// rtw8922a_ctrl_mlo(MLO_2_PLUS_0_1RF)
    fn ctrl_mlo(&self, c: &Chan) -> io::Result<()> {
        self.pw32_mask(R_DBCC, B_DBCC_EN, 0)?;
        self.pw32_mask(R_DBCC_FA, B_DBCC_FA, 1)?;
        self.ctrl_afe_dac(c.bw, 0)?;
        self.ctrl_afe_dac(c.bw, 1)?;
        self.emlsr_2_plus_0()
    }

    fn emlsr_2_plus_0(&self) -> io::Result<()> {
        for v in [0x6180, 0xbbab, 0xaba9, 0xeba9, 0xeaa9] {
            self.pw32_mask(R_EMLSR, B_EMLSR_PARM, v)?;
        }
        Ok(())
    }

    fn ctrl_afe_dac(&self, bw: Bw, path: u32) -> io::Result<()> {
        let o = path * 0x100;
        let (d0, d1) = if bw == Bw::B160 { (0xd, 0x6) } else { (0xe, 0x7) };
        self.pw32_mask(R_AFEDAC0 + o, B_AFEDAC0, d0)?;
        self.pw32_mask(R_AFEDAC1 + o, B_AFEDAC1, d1)
    }

    /// rtw8922a_set_syn01(RF_SYN_ON_OFF): 합성기는 경로 A 것만 켭니다.
    fn set_syn_a_only(&self) -> io::Result<()> {
        if self.cv == CHIP_CAV {
            for v in [3, 2, 3] {
                self.write_rf(0, RR_POW, RR_POW_SYN, v)?;
            }
            return self.write_rf(1, RR_POW, RR_POW_SYN, 0);
        }
        self.write_rf(0, RR_POW, RR_POW_SYN_V1, 0xf)?;
        self.write_rf(1, RR_POW, RR_POW_SYN_V1, 0)
    }

    /// rtw8922a_chlk_reload: 이 채널의 RF 보정 표를 고르고 BB/RF가 그 표를 쓰게 합니다.
    fn chlk_reload(&self, c: &Chan) -> io::Result<()> {
        // rtw89_rfk_chan_lookup: 같은 채널이 있으면 그것, 없으면 맨 앞.
        let mut tbl = self.rfk_tbl.lock().unwrap();
        let idx = tbl.iter().position(|t| t.as_ref() == Some(c)).unwrap_or(0);
        tbl[idx] = Some(*c);
        drop(tbl);
        for (coef, cfir, path) in [(R_COEF_SEL, R_CFIR_LUT, 0), (R_COEF_SEL_C1, R_CFIR_LUT_C1, 1)] {
            self.pw32_mask(coef, B_COEF_SEL_EN, 1)?;
            self.pw32_mask(coef, B_COEF_SEL_IQC_V1, idx as u32)?;
            self.pw32_mask(coef, B_COEF_SEL_MDPD_V1, idx as u32)?;
            self.write_rf(path, RR_MODOPT, RR_TXG_SEL, 0x4 | idx as u32)?;
            let sel = self.pr32(coef)?;
            self.pw32_mask(cfir, B_CFIR_LUT_G3, sel & 1)?;
            self.pw32_mask(cfir, B_CFIR_LUT_G5, (sel >> 1) & 1)?;
        }
        Ok(())
    }

    /// RF 보정 표의 채널들 ([Rtl8922a::chlk_reload]가 고른 자리).
    pub(super) fn rfk_table(&self) -> [Option<Chan>; RFK_TABLES] {
        *self.rfk_tbl.lock().unwrap()
    }

    /// rtw8922a_rfk_pll_init
    fn rfk_pll_init(&self) -> io::Result<()> {
        let v = self.xtal_si_read(XTAL_SI_PLL_1)?;
        self.xtal_si_write(XTAL_SI_PLL_1, v | 0xf8, 0xff)?;
        let v = self.xtal_si_read(XTAL_SI_APBT)?;
        self.xtal_si_write(XTAL_SI_APBT, v & !0x60, 0xff)?;
        let v = self.xtal_si_read(XTAL_SI_XTAL_PLL)?;
        self.xtal_si_write(XTAL_SI_XTAL_PLL, v | 0x38, 0xff)
    }

    /// rtw8922a_hal_reset (송신 스케줄러 멈춤은 아직 송신이 없어 뺐습니다).
    fn hal_reset(&self, band: Band, enter: bool) -> io::Result<()> {
        if enter {
            self.cfg_ppdu_status(false)?;
            self.dfs_en(false)?;
            self.tssi_cont_en(false)?;
            self.adc_en(false)?;
            sleep(Duration::from_micros(40));
            self.bb_reset_en(band, false)
        } else {
            self.cfg_ppdu_status(true)?;
            self.adc_en(true)?;
            self.dfs_en(true)?;
            self.tssi_cont_en(true)?;
            self.bb_reset_en(band, true)
        }
    }

    fn dfs_en(&self, en: bool) -> io::Result<()> {
        self.pw32_mask(0x2800, 1 << 1, en as u32)?;
        self.pw32_mask(0x2900, 1 << 1, en as u32)
    }

    fn tssi_cont_en(&self, en: bool) -> io::Result<()> {
        self.pw32_mask(R_TSSI_PWR_P0, B_TSSI_CONT_EN, !en as u32)?;
        self.pw32_mask(R_TSSI_PWR_P1, B_TSSI_CONT_EN, !en as u32)
    }

    fn adc_en(&self, en: bool) -> io::Result<()> {
        let v = self.pr32(R_ADC_FIFO_V1)?;
        let mut f = (v & B_ADC_FIFO_EN_V1) >> B_ADC_FIFO_EN_V1.trailing_zeros();
        if en {
            f &= !0x3;
        } else {
            f |= 0x3;
        }
        self.pw32_mask(R_ADC_FIFO_V1, B_ADC_FIFO_EN_V1, f)
    }

    /// rtw8922a_bb_reset_en
    fn bb_reset_en(&self, band: Band, en: bool) -> io::Result<()> {
        if en {
            self.pw32_mask(R_RSTB_ASYNC, B_RSTB_ASYNC_ALL, 1)?;
            if band == Band::G2 {
                self.pw32_mask(R_RXCCA_BE1, B_RXCCA_BE1_DIS, 0)?;
            }
            self.pw32_mask(R_PD_CTRL, B_PD_HIT_DIS, 0)
        } else {
            self.pw32_mask(R_RXCCA_BE1, B_RXCCA_BE1_DIS, 1)?;
            self.pw32_mask(R_PD_CTRL, B_PD_HIT_DIS, 1)?;
            sleep(Duration::from_micros(1));
            self.pw32_mask(R_RSTB_ASYNC, B_RSTB_ASYNC_ALL, 0)
        }
    }

    fn tssi_reset(&self) -> io::Result<()> {
        self.pw32_mask(R_TXPWR_RSTA, B_TXPWR_RSTA, 0)?;
        self.pw32_mask(R_TXPWR_RSTA, B_TXPWR_RSTA, 1)?;
        self.pw32_mask(R_TXPWR_RSTB, B_TXPWR_RSTB, 0)?;
        self.pw32_mask(R_TXPWR_RSTB, B_TXPWR_RSTB, 1)
    }

    /// rtw8922a_ctrl_trx_path(RF_PATH_AB, 2, RF_PATH_AB, 2)의 PHY 0 부분.
    fn ctrl_trx_path(&self) -> io::Result<()> {
        self.pw32_mask(R_MAC_SEL, B_MAC_SEL, 0)?;
        for (a, v) in TX_PATH_AB {
            self.w32(a, v)?;
        }
        // rtw8922a_ctrl_rx_path_tmac: EDCCA 보고가 이상해지지 않게 먼저 0.
        self.pw32_mask(R_ANT_CHBW, B_ANT_RX_SG0, 0)?;
        self.pw32_mask(R_ANT_CHBW, B_ANT_RX_SG0, 3)?;
        self.pw32_mask(R_FC0INV_SBW, B_RX_1RCCA, 3)?;
        self.rx_nss_limit_2()?;
        self.tssi_reset()?;
        self.rx_nss_limit_2()
    }

    /// rtw8922a_cfg_rx_nss_limit(2)
    fn rx_nss_limit_2(&self) -> io::Result<()> {
        for (a, m, v) in [
            (R_BRK_R, B_HTMCS_LMT, 1),
            (R_BRK_R, B_VHTMCS_LMT, 1),
            (R_BRK_HE, B_N_USR_MAX, HE_N_USER_MAX),
            (R_BRK_HE, B_NSS_MAX, 1),
            (R_BRK_HE, B_TB_NSS_MAX, 1),
            (R_BRK_EHT, B_RXEHT_NSS_MAX, 1),
            (R_BRK_RXEHT, B_RXEHTTB_NSS_MAX, 1),
            (R_BRK_RXEHT, B_RXEHT_N_USER_MAX, HE_N_USER_MAX),
        ] {
            self.pw32_mask(a, m, v)?;
        }
        Ok(())
    }

    /// rtw8922a_set_digital_pwr_comp (두 경로, 2 스트림)
    fn digital_pwr_comp(&self, band: Band) -> io::Result<()> {
        let (a, b) = if band == Band::G2 {
            (&PWR_COMP_2G_S0, &PWR_COMP_2G_S1)
        } else {
            (&PWR_COMP_5G, &PWR_COMP_5G)
        };
        for (base, vals) in [(R_BE_LTPC_T0_PATH0, a), (R_BE_LTPC_T0_PATH1, b)] {
            for (i, v) in vals.iter().enumerate() {
                self.pw32(base + 4 * i as u32, *v)?;
            }
        }
        Ok(())
    }

    /// rtw8922a_set_channel_mac
    fn set_channel_mac(&self, c: &Chan) -> io::Result<()> {
        let rf_mod = match c.bw {
            Bw::B160 => BE_WMAC_RFMOD_160M,
            Bw::B80 => BE_WMAC_RFMOD_80M,
            Bw::B40 => BE_WMAC_RFMOD_40M,
            Bw::B20 => BE_WMAC_RFMOD_20M,
        };
        self.w8_mask(R_BE_WMAC_RFMOD, B_BE_WMAC_RFMOD_MASK, rf_mod)?;
        self.w32(R_BE_TX_SUB_BAND_VALUE, tx_sub_band(c))?;

        let chk = if c.band == Band::G2 {
            B_BE_BAND_MODE
        } else {
            B_BE_CHECK_CCK_EN | B_BE_RTS_LIMIT_IN_OFDM6
        };
        self.clr8(R_BE_TXRATE_CHK, B_BE_BAND_MODE | B_BE_CHECK_CCK_EN | B_BE_RTS_LIMIT_IN_OFDM6)?;
        self.set8(R_BE_TXRATE_CHK, chk)?;

        let (t1, tb) = if c.bw >= Bw::B40 { (0x41, 0x41) } else { (0x3f, 0x3e) };
        self.w32_mask(R_BE_PREBKF_CFG_1, B_BE_SIFS_MACTXEN_T1_MASK, t1)?;
        self.w32_mask(R_BE_MUEDCA_EN, B_BE_SIFS_MACTXEN_TB_T1_MASK, tb)
    }

    /// 같은 중심과 폭에서 주 20 MHz 채널만 바꿉니다: MAC이 보내는 부대역, BB의 주 채널과 채널 색인.
    /// RF, 이득, 보정 표는 중심으로 정해져 그대로라 USB 전송 몇 번이면 됩니다 (AWDL 상대의 슬롯을 따라
    /// 80 MHz 안에서 옮길 때).
    pub fn set_primary(&self, c: &Chan) -> io::Result<()> {
        self.w32(R_BE_TX_SUB_BAND_VALUE, tx_sub_band(c))?;
        let prich = if c.bw == Bw::B20 { 0 } else { c.pri_sb() };
        self.pw32_mask(R_ANT_CHBW, B_CHBW_PRICH, prich)?;
        self.pw32_mask(R_MAC_PIN_SEL, B_CH_IDX_SEG0, c.chan_idx())
    }

    /// rtw8922a_set_channel_bb
    fn set_channel_bb(&self, c: &Chan) -> io::Result<()> {
        let is_2g = c.band == Band::G2;
        if is_2g && (1..14).contains(&c.primary) {
            // rtw8922a_ctrl_sco_cck
            let i = c.primary as usize - 1;
            self.pw32_mask(R_BK_FC0INV, B_BK_FC0INV, SCO_BARKER[i])?;
            self.pw32_mask(R_CCK_FC0INV, B_CCK_FC0INV, SCO_CCK[i])?;
        }
        self.ctrl_ch(c)?;
        self.ctrl_bw(c)?;
        // rtw8922a_ctrl_cck_en
        self.pw32_mask(R_RXCCA_BE1, B_RXCCA_BE1_DIS, !is_2g as u32)?;
        self.pw32_mask(R_UPD_CLK_ADC, B_ENABLE_CCK, is_2g as u32)?;
        self.pw32_mask(R_PD_ARBITER_OFF, B_PD_ARBITER_OFF, !is_2g as u32)?;
        // rtw8922a_spur_elimination: 8922A는 알려진 잡음 주파수가 없어 노치를 끕니다.
        self.pw32_mask(R_S0S1_CSI_WGT, B_S0S1_CSI_WGT_EN, 0)?;
        for a in [0x41a0, 0x41ac, 0x45a0, 0x45ac] {
            self.pw32_mask(a, 0x1000, 0)?;
        }
        self.pw32_mask(R_RSTB_ASYNC, B_RSTB_ASYNC_ALL, 1)?;
        self.tssi_reset()
    }

    /// rtw8922a_ctrl_ch (eFuse의 수신 이득 보정은 아직 읽지 않아 rtw89에서 그 값이 없을 때처럼 건너뜁니다).
    fn ctrl_ch(&self, c: &Chan) -> io::Result<()> {
        let is_2g = c.band == Band::G2;
        for path in 0..2 {
            self.set_gain(c, path)?;
        }
        for a in [0x4160, 0x4560] {
            self.pw32_mask(a, 1 << 26, is_2g as u32)?;
        }
        let freq = c.freq();
        self.pw32_mask(R_FC0, B_FC0, freq)?;
        self.pw32_mask(R_FC0INV_SBW, B_FC0_INV, ((1 << 18) + freq / 2) / freq)?;
        if is_2g {
            let p: [u32; 8] = if c.center == 14 {
                [0x3b13ff, 0x1c42de, 0xfdb0ad, 0xf60f6e, 0xfd8f92, 0x02d011, 0x01c02c, 0xfff00a]
            } else {
                [0x3a63ca, 0x2a833f, 0x1491f8, 0x03c0b0, 0xfccff1, 0xfccfc3, 0xfebfdc, 0xffdff7]
            };
            let regs = [
                (R_PCOEFF01, B_PCOEFF01),
                (R_PCOEFF23, B_PCOEFF23),
                (R_PCOEFF45, B_PCOEFF45),
                (R_PCOEFF67, B_PCOEFF67),
                (R_PCOEFF89, B_PCOEFF89),
                (R_PCOEFFAB, B_PCOEFFAB),
                (R_PCOEFFCD, B_PCOEFFCD),
                (R_PCOEFFEF, B_PCOEFFEF),
            ];
            for ((a, m), v) in regs.into_iter().zip(p) {
                self.pw32_mask(a, m, v)?;
            }
        }
        self.pw32_mask(R_MAC_PIN_SEL, B_CH_IDX_SEG0, c.chan_idx())
    }

    /// rtw8922a_set_gain: LNA/TIA 이득과 수신 세기 보정.
    fn set_gain(&self, c: &Chan, path: usize) -> io::Result<()> {
        let g = &*self.gain;
        let band = c.gain_band();
        let bw = (c.bw >= Bw::B80) as usize;
        let is_2g = c.band == Band::G2;
        let write = |table: &[GainReg], vals: &[i8]| -> io::Result<()> {
            for (&(rg, ra, mg, ma), &v) in table.iter().zip(vals) {
                let (reg, mask) = if is_2g { (rg[path], mg) } else { (ra[path], ma) };
                self.pw32_mask(reg, mask, v as i32 as u32)?;
            }
            Ok(())
        };
        write(&BB_GAIN_LNA, &g.lna[band][bw][path])?;
        write(&BB_GAIN_TIA, &g.tia[band][bw][path])?;
        write(&BB_OP1DB_LNA, &g.lna_op1db[band][bw][path])?;
        write(&BB_OP1DB_TIA_LNA, &g.tia_lna_op1db[band][bw][path])?;

        let o = if path == 1 { 0x400 } else { 0 };
        let rpl = |table: &[(u32, u32)], vals: &[i8]| -> io::Result<()> {
            for (&(a, m), &v) in table.iter().zip(vals) {
                self.pw32_mask(a | o, m, v as i32 as u32)?;
            }
            Ok(())
        };
        rpl(&RPL_160, &g.rpl160[band][path])?;
        rpl(&RPL_80, &g.rpl80[band][path])?;
        rpl(&RPL_40, &g.rpl40[band][path])?;
        rpl(&RPL_20, &g.rpl20[band][path])
    }

    /// rtw8922a_ctrl_bw
    fn ctrl_bw(&self, c: &Chan) -> io::Result<()> {
        let pri = c.pri_sb();
        let (bw, prich, map) = match c.bw {
            Bw::B20 => (0, 0, 0),
            Bw::B40 => (1, pri, 0),
            Bw::B80 => (2, pri, 1),
            Bw::B160 => (3, pri, 1),
        };
        self.pw32_mask(R_ANT_CHBW, B_CHBW_BW, bw)?;
        self.pw32_mask(R_FC0INV_SBW, B_SMALLBW, 0)?;
        self.pw32_mask(R_ANT_CHBW, B_CHBW_PRICH, prich)?;
        self.pw32_mask(R_DAC_CLK, B_DAC_CLK, 1)?;
        self.pw32_mask(R_GAIN_MAP0, B_GAIN_MAP0_EN, map)?;
        self.pw32_mask(R_GAIN_MAP1, B_GAIN_MAP1_EN, map)?;
        self.pw32_mask(R_FC0, B_BW40_2XFFT, (c.bw == Bw::B40) as u32)
    }

    /// rtw8922a_ctl_band_ch_bw: 두 경로의 RF 0x18(과 그 사본)에 채널을 씁니다.
    fn set_channel_rf(&self, c: &Chan) -> io::Result<()> {
        let addrs = [RR_CFGCH, RR_CFGCH_V1];
        let mut regs = [[0u32; 2]; 2];
        for (path, r) in regs.iter_mut().enumerate() {
            for (i, a) in addrs.iter().enumerate() {
                r[i] = self.read_rf(path, *a, RFREG_MASK)?;
            }
        }
        if self.read_rf(0, RR_CFGCH, RFREG_MASK)? == INV_RF_DATA {
            return Err(io::Error::other("RF 0x18 read failed"));
        }
        let keep = !(RR_CFGCH_BAND1 | RR_CFGCH_BW_V2 | RR_CFGCH_BAND0 | RR_CFGCH_CH);
        for (path, r) in regs.iter().enumerate() {
            for (i, a) in addrs.iter().enumerate() {
                if r[i] == INV_RF_DATA {
                    return Err(io::Error::other(format!("RF 0x18 of path {path} is invalid")));
                }
                self.write_rf(path, *a, RFREG_MASK, (r[i] & keep) | c.rf18())?;
                sleep(Duration::from_micros(100));
            }
        }
        if self.cv != CHIP_CAV {
            return Ok(());
        }
        let d1 = if c.band == Band::G2 { 0x0c990 } else { 0x0c190 };
        for (a, v) in [
            (RR_LUTWE, 0x80000),
            (RR_LUTWA, 0x00003),
            (RR_LUTWD1, d1),
            (RR_LUTWD0, 0xebe38),
            (RR_LUTWE, 0),
        ] {
            self.write_rf(0, a, RFREG_MASK, v)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_numbers() {
        let c = Chan::new(Band::G5, 155, 149, Bw::B80);
        assert_eq!(c.freq(), 5775);
        assert_eq!(c.pri_sb(), 0);
        assert_eq!(c.txsb(Bw::B20), 0);
        assert_eq!(c.chan_idx(), 0x50);
        assert_eq!(c.gain_band(), 3);
        let c = Chan::new(Band::G6, 15, 17, Bw::B160);
        assert_eq!(c.freq(), 6025);
        assert_eq!(c.pri_sb(), 4);
        assert_eq!(c.txsb(Bw::B20), 4);
        // rtw89_phy_get_txsb의 식 그대로 ((pri - center + 12) / 8).
        assert_eq!(c.txsb(Bw::B40), 1);
        assert_eq!(c.txsb(Bw::B80), 1);
        assert_eq!(c.chan_idx(), 0x78);
        assert_eq!(c.gain_band(), 4);
        assert_eq!(Chan::narrow(Band::G2, 6).chan_idx(), 6);
    }
}
