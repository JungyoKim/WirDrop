//! BB/RF 초기화: 펌웨어 파일의 PHY 표(BB_REG, RADIO_A/B, RF_NCTL)를 칩의 RFE 종류와 컷에 맞는 갈래로
//! 골라 씁니다 (rtw89_phy_init_reg, rtw89_phy_init_bb_reg, rtw89_phy_init_rf_reg, rtw8922a_bb_postinit,
//! rtw89_phy_preinit_rf_nctl_be). RFE 종류는 eFuse의 RF 논리 블록에 있습니다.
use std::io;
use std::thread::sleep;
use std::time::Duration;

use super::fw;
use super::generated::regs::*;
use super::{Rtl8922a, put};

/// BE 세대 BB 레지스터의 주소 공간 (rtw89_phy_gen_be.cr_base).
pub(super) const PHY_CR_BASE: u32 = 0x20000;
/// rtw8922a: RF 경로마다 직접 쓰는 주소 (rf_base_addr).
const RF_BASE_ADDR: [u32; 2] = [0xe000, 0xf000];
/// A-die RF 레지스터를 쓰는 HWSI 자리 (rtw89_phy_write_full_rf_v2_a).
const HWSI_IDLE: [u32; 2] = [0x2c24, 0x2d24];
const HWSI_WRITE: [u32; 2] = [0x2ae0, 0x2be0];
/// A-die RF 레지스터를 읽는 HWSI 자리 (rtw89_phy_read_full_rf_v2_a).
const HWSI_READ: [u32; 2] = [0x2adc, 0x2bdc];
pub(super) const RFREG_MASK: u32 = 0xfffff;
/// 읽기에 실패한 RF 값 (INV_RF_DATA).
pub(super) const INV_RF_DATA: u32 = 0xffff_ffff;
const RF_ADDR_ADSEL: u32 = 1 << 16;
const BYPASS_CR_DATA: u32 = 0xbabe_cafe;
/// RF 표에서 펌웨어에도 알려 줄 쓰기의 한 쪽 크기와 쪽 수 (RTW89_H2C_RF_PAGE_*).
const RF_PAGE_SIZE: usize = 500;
const RF_PAGE_NUM: usize = 3;
/// eFuse: 물리 크기, 보안 영역 다음부터 시작, RF 논리 블록 (rtw8922a_efuse_blocks).
const EFUSE_PHY_SIZE: u32 = 0x1300;
const EFUSE_SEC_CTRL_SIZE: usize = 4;
const EFUSE_RF_PAGE: u32 = 1;
const EFUSE_RF_SIZE: usize = 0x240;
/// struct rtw8922a_efuse 안의 자리.
const EFUSE_XTAL_K: usize = 0xb9;
const EFUSE_RFE_TYPE: usize = 0xca;

/// BB 이득 표의 갈래 수 (rtw89_phy_bb_gain_info_be): 이득 대역 12, 대역폭 묶음 2, 경로 2.
const GAIN_BANDS: usize = 12;
const LNA_GAIN_NUM: usize = 7;
const TIA_GAIN_NUM: usize = 2;

/// 펌웨어의 BB_GAIN 표에서 읽은 수신 이득 보정값 (rtw89_phy_bb_gain_info_be). 채널을 바꿀 때
/// 그 대역의 값을 BB에 씁니다 ([Rtl8922a::set_channel]).
#[derive(Default)]
pub(super) struct BbGain {
    pub lna: [[[[i8; LNA_GAIN_NUM]; 2]; 2]; GAIN_BANDS],
    pub tia: [[[[i8; TIA_GAIN_NUM]; 2]; 2]; GAIN_BANDS],
    pub lna_op1db: [[[[i8; LNA_GAIN_NUM]; 2]; 2]; GAIN_BANDS],
    pub tia_lna_op1db: [[[[i8; LNA_GAIN_NUM + 1]; 2]; 2]; GAIN_BANDS],
    pub rpl20: [[[i8; 1]; 2]; GAIN_BANDS],
    pub rpl40: [[[i8; 2]; 2]; GAIN_BANDS],
    pub rpl80: [[[i8; 4]; 2]; GAIN_BANDS],
    pub rpl160: [[[i8; 8]; 2]; GAIN_BANDS],
}

/// data의 아래 바이트부터 차례로 dst에 넣습니다.
fn fill_bytes(dst: &mut [i8], data: u32) {
    for (i, d) in dst.iter_mut().enumerate() {
        *d = (data >> (8 * i)) as u8 as i8;
    }
}

/// PHY 표의 조건 줄 (phy.h).
const PHY_HEADLINE_VALID: u32 = 0xf;
const PHY_COND_BRANCH_IF: u32 = 0x8;
const PHY_COND_BRANCH_ELIF: u32 = 0x9;
const PHY_COND_BRANCH_ELSE: u32 = 0xa;
const PHY_COND_BRANCH_END: u32 = 0xb;
const PHY_COND_CHECK: u32 = 0x4;
const PHY_COND_DONT_CARE: u32 = 0xff;

fn phy_compare(rfe: u32, cv: u32) -> u32 {
    (rfe << 16) | cv
}

/// PHY 1 사본의 주소 (rtw89_phy0_phy1_offset_be): 정해진 페이지만 0x1000 위.
pub(super) fn phy1(addr: u32) -> u32 {
    let page = addr >> 8;
    let shifted = matches!(page, 0x4..=0xf | 0x20..=0x2b | 0x40..=0x4f | 0x60..=0x6f | 0xe4..=0xe5 | 0xe8..=0xed);
    addr + if shifted { 0x1000 } else { 0 }
}

/// 표에서 칩(rfe, cv)에 맞는 머리 줄을 고릅니다 (rtw89_phy_sel_headline). (머리 줄 수, 고른 줄의 대상).
fn select_headline(regs: &[(u32, u32)], rfe: u32, cv: u32) -> io::Result<(usize, u32)> {
    let n = regs.iter().take_while(|(a, _)| a >> 28 == PHY_HEADLINE_VALID).count();
    if n == 0 {
        return Ok((0, 0));
    }
    let target = |a: u32| a & 0x0fff_ffff;
    let heads = &regs[..n];
    for want in [phy_compare(rfe, cv), phy_compare(rfe, PHY_COND_DONT_CARE)] {
        if let Some((a, _)) = heads.iter().find(|(a, _)| target(*a) == want) {
            return Ok((n, target(*a)));
        }
    }
    for rfe_want in [rfe, PHY_COND_DONT_CARE] {
        let best = heads.iter().filter(|(a, _)| (a >> 16) & 0xff == rfe_want).max_by_key(|(a, _)| a & 0xff);
        if let Some((a, _)) = best {
            return Ok((n, target(*a)));
        }
    }
    Err(io::Error::other(format!("PHY table has no branch for RFE {rfe} cv {cv}")))
}

/// 표를 읽어 칩에 맞는 줄만 f에 넘깁니다 (rtw89_phy_init_reg).
fn walk(regs: &[(u32, u32)], rfe: u32, cv: u32, mut f: impl FnMut(u32, u32) -> io::Result<()>) -> io::Result<()> {
    let (n, cfg_target) = select_headline(regs, rfe, cv)?;
    let (mut target, mut matched, mut found) = (0, true, false);
    for &(addr, data) in &regs[n..] {
        match addr >> 28 {
            PHY_COND_BRANCH_IF | PHY_COND_BRANCH_ELIF => target = addr & 0x0fff_ffff,
            PHY_COND_BRANCH_ELSE => {
                matched = false;
                if !found {
                    return Err(io::Error::other(format!("PHY table: no branch for CR {addr:x}/{data:x}")));
                }
            }
            PHY_COND_BRANCH_END => {
                matched = true;
                found = false;
            }
            PHY_COND_CHECK => {
                if found {
                    matched = false;
                } else {
                    matched = target == cfg_target;
                    found = matched;
                }
            }
            _ if matched => f(addr, data)?,
            _ => {}
        }
    }
    Ok(())
}

/// 요소 본문의 (주소, 값) 쌍 (rtw89_build_phy_tbl_from_elm; 본문 앞 8바이트는 reg2 머리말).
fn regs_of(e: &fw::Element) -> Vec<(u32, u32)> {
    e.body
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            (
                u32::from_le_bytes(c[..4].try_into().unwrap()),
                u32::from_le_bytes(c[4..].try_into().unwrap()),
            )
        })
        .collect()
}

/// 펌웨어 파일의 PHY 표. 같은 번호의 요소가 여럿이면 첫 것 (rtw89_build_phy_tbl_from_elm).
fn phy_table(elements: &[fw::Element], id: u32) -> io::Result<Vec<(u32, u32)>> {
    elements
        .iter()
        .find(|e| e.id == id)
        .map(regs_of)
        .ok_or_else(|| io::Error::other(format!("firmware has no PHY element {id}")))
}

impl Rtl8922a {
    // BB 레지스터 (rtw89_phy_write32 등: 주소 + PHY_CR_BASE).
    pub fn pr32(&self, a: u32) -> io::Result<u32> {
        self.r32(a + PHY_CR_BASE)
    }
    pub fn pw32(&self, a: u32, v: u32) -> io::Result<()> {
        self.w32(a + PHY_CR_BASE, v)
    }
    pub fn pw32_mask(&self, a: u32, mask: u32, v: u32) -> io::Result<()> {
        if mask == u32::MAX {
            return self.pw32(a, v);
        }
        self.w32_mask(a + PHY_CR_BASE, mask, v)
    }
    pub(super) fn pset32(&self, a: u32, bits: u32) -> io::Result<()> {
        self.set32(a + PHY_CR_BASE, bits)
    }
    pub(super) fn pclr32(&self, a: u32, bits: u32) -> io::Result<()> {
        self.clr32(a + PHY_CR_BASE, bits)
    }

    /// RF 레지스터 쓰기 (rtw89_phy_write_rf_v2). mask가 전체가 아니면 읽어서 고쳐 씁니다.
    pub(super) fn write_rf(&self, path: usize, addr: u32, mask: u32, data: u32) -> io::Result<()> {
        if addr & RF_ADDR_ADSEL != 0 {
            self.pw32_mask(RF_BASE_ADDR[path] + ((addr & 0xff) << 2), mask & RFREG_MASK, data)?;
            sleep(Duration::from_micros(1));
            return Ok(());
        }
        let val = if mask == RFREG_MASK {
            data
        } else {
            let old = self.read_rf_hwsi(path, addr)?;
            put(old, mask, data)
        };
        let idle = HWSI_IDLE[path] + PHY_CR_BASE;
        self.poll(idle, 10, "RF HWSI", |v| v & (1 << 29) == 0)?;
        self.pw32(HWSI_WRITE[path], put(0, B_HWSI_DATA_ADDR, addr) | put(0, B_HWSI_DATA_VAL, val))
    }

    /// RF 레지스터 읽기 (rtw89_phy_read_rf_v2).
    pub(super) fn read_rf(&self, path: usize, addr: u32, mask: u32) -> io::Result<u32> {
        let v = if addr & RF_ADDR_ADSEL != 0 {
            self.pr32(RF_BASE_ADDR[path] + ((addr & 0xff) << 2))? & RFREG_MASK
        } else {
            self.read_rf_hwsi(path, addr)?
        };
        Ok((v & mask) >> mask.trailing_zeros())
    }

    /// A-die RF 레지스터 전체 읽기 (rtw89_phy_read_full_rf_v2_a).
    fn read_rf_hwsi(&self, path: usize, addr: u32) -> io::Result<u32> {
        let (ctl, val) = (HWSI_READ[path], HWSI_IDLE[path]);
        self.pw32_mask(ctl, B_HWSI_ADD_CTL_MASK, 0x1)?;
        self.poll(val + PHY_CR_BASE, 10, "RF HWSI read busy", |v| v & B_HWSI_VAL_BUSY == 0)?;
        self.pw32_mask(ctl, B_HWSI_ADD_MASK, addr)?;
        self.pw32_mask(ctl, B_HWSI_ADD_RD, 0x1)?;
        let r = self.poll(val + PHY_CR_BASE, 10, "RF HWSI read", |v| v & B_HWSI_VAL_RDONE != 0);
        let out = r.map(|v| v & RFREG_MASK);
        self.pw32_mask(ctl, B_HWSI_ADD_POLL_MASK, 0)?;
        out
    }

    /// eFuse RF 논리 블록 (rtw89_parse_efuse_map_be의 RF 부분, rtw89_eeprom_parser_be).
    pub fn efuse_rf_map(&self) -> io::Result<Vec<u8>> {
        let phy = self.efuse_read(0, EFUSE_PHY_SIZE)?;
        let mut log = vec![0xffu8; EFUSE_RF_SIZE];
        let mut i = EFUSE_SEC_CTRL_SIZE;
        while i + 3 <= phy.len() {
            let (h1, h2, h3) = (phy[i], phy[i + 1], phy[i + 2]);
            if h1 == 0xff || h2 == 0xff || h3 == 0xff {
                break;
            }
            i += 3;
            let hdr = ((h1 as u32) << 16) | ((h2 as u32) << 8) | h3 as u32;
            let page = (hdr >> 17) & 0x7;
            let blk = (hdr >> 4) & 0x1fff;
            let word_en = hdr & 0xf;
            for w in 0..4 {
                if word_en & (1 << w) != 0 {
                    continue;
                }
                if i + 1 >= phy.len() {
                    return Err(io::Error::other("efuse: truncated block"));
                }
                let at = ((blk << 3) + (w << 1)) as usize;
                if page == EFUSE_RF_PAGE && at + 1 < EFUSE_RF_SIZE {
                    log[at] = phy[i];
                    log[at + 1] = phy[i + 1];
                }
                i += 2;
            }
        }
        Ok(log)
    }

    /// BB와 RF를 초기화합니다 (rtw89_core_start의 BB/RF 부분). [Rtl8922a::mac_init] 다음에.
    pub fn phy_init(&mut self, file: &[u8]) -> io::Result<()> {
        let elements = fw::elements(file)?;
        let table = |id: u32| phy_table(&elements, id);
        let (rfe, cv) = (self.rfe as u32, self.cv as u32);

        // rtw89_phy_init_bb_reg: BB 표를 PHY 0과 PHY 1에, 이득 표 (init_txpwr_unit과 bb_reset은 8922A에서
        // 비어 있음).
        let bb = table(fw::ELM_BB_REG)?;
        walk(&bb, rfe, cv, |a, d| self.config_bb_reg(a, d))?;
        walk(&bb, rfe, cv, |a, d| self.config_bb_reg(if a >= 0x100 { phy1(a) } else { a }, d))?;
        let mut gain = Box::<BbGain>::default();
        walk(&table(fw::ELM_BB_GAIN)?, rfe, cv, |a, d| self.config_bb_gain(&mut gain, a, d))?;
        self.gain = gain;
        // rtw8922a_bb_postinit(PHY 0), 그다음 PHY 1. rtw89_phy_set_phy_regs는 두 PHY에 다 씁니다.
        self.set32(R_BE_FEN_RST_ENABLE, B_BE_BOOT_RDY0)?;
        for phy in 0..2 {
            self.set32(R_BE_FEN_RST_ENABLE, if phy == 0 { B_BE_FEN_BBPLAT_RSTB } else { B_BE_FEN_BB1PLAT_RSTB })?;
            self.bb_postinit(phy)?;
        }

        // rtw89_phy_init_rf_reg: 경로마다 RF 표를 쓰고, 0x100 이상 주소는 펌웨어에도 쪽으로 알립니다.
        self.init_rf_reg(&elements, rfe, cv)
    }

    fn bb_postinit(&self, phy: u32) -> io::Result<()> {
        self.pset32(R_BBCLK, B_CLK_640M)?;
        self.pclr32(R_TXSCALE, B_TXFCTR_EN)?;
        for (a, m, v) in [
            (R_TXFCTR, B_TXFCTR_THD, 0x200),
            (R_SLOPE, B_EHT_RATE_TH, 0xa),
            (R_BEDGE, B_HE_RATE_TH, 0xa),
            (R_BEDGE2, B_HT_VHT_TH, 0xaaa),
            (R_BEDGE, B_EHT_MCS14, 0x1),
            (R_BEDGE2, B_EHT_MCS15, 0x1),
            (R_BEDGE3, B_EHTTB_EN, 0x0),
            (R_BEDGE3, B_HEERSU_EN, 0x0),
            (R_BEDGE3, B_HEMU_EN, 0x0),
            (R_BEDGE3, B_TB_EN, 0x0),
            (R_SU_PUNC, B_SU_PUNC_EN, 0x1),
            (R_BEDGE5, B_HWGEN_EN, 0x1),
            (R_BEDGE5, B_PWROFST_COMP, 0x1),
            (R_MAG_AB, B_BY_SLOPE, 0x1),
            (R_MAG_A, B_MGA_AEND, 0xe0),
            (R_MAG_AB, B_MAG_AB, 0xe0c000),
            (R_SLOPE, B_SLOPE_A, 0x3fe0),
            (R_SLOPE, B_SLOPE_B, 0x3fe0),
            (R_SC_CORNER, B_SC_CORNER, 0x200),
        ] {
            self.pw32_mask(a, m, v)?;
            self.pw32_mask(phy1(a), m, v)?;
        }
        let udp = if phy == 0 { R_UDP_COEEF } else { phy1(R_UDP_COEEF) };
        self.pw32_mask(udp, B_UDP_COEEF, 0)?;
        self.pw32_mask(udp, B_UDP_COEEF, 1)
    }

    fn init_rf_reg(&self, elements: &[fw::Element], rfe: u32, cv: u32) -> io::Result<()> {
        let table = |id: u32| phy_table(elements, id);
        for (path, id) in [(0usize, fw::ELM_RADIO_A), (1, fw::ELM_RADIO_B)] {
            let mut pages: Vec<u32> = Vec::new();
            walk(&table(id)?, rfe, cv, |a, d| {
                if a == 0xfe {
                    sleep(Duration::from_millis(50));
                    return Ok(());
                }
                self.write_rf(path, a, RFREG_MASK, d)?;
                if a >= 0x100 {
                    pages.push((a << 20) | d);
                }
                Ok(())
            })?;
            if pages.len() > RF_PAGE_SIZE * RF_PAGE_NUM {
                return Err(io::Error::other(format!("RF path {path}: {} parameters don't fit", pages.len())));
            }
            let class = if path == 0 { H2C_CL_OUTSRC_RF_REG_A } else { H2C_CL_OUTSRC_RF_REG_B };
            for (page, chunk) in pages.chunks(RF_PAGE_SIZE).enumerate() {
                let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
                self.h2c(H2C_CAT_OUTSRC, class, page as u32, false, false, &bytes)?;
            }
        }
        Ok(())
    }

    /// rtw89_phy_init_rf_nctl: rtw89_phy_preinit_rf_nctl_be, 그리고 NCTL 표.
    pub(super) fn init_rf_nctl(&self, file: &[u8]) -> io::Result<()> {
        let elements = fw::elements(file)?;
        let (rfe, cv) = (self.rfe as u32, self.cv as u32);
        for (a, m, v) in [
            (R_GOTX_IQKDPK_C0, B_GOTX_IQKDPK, 0x3),
            (R_GOTX_IQKDPK_C1, B_GOTX_IQKDPK, 0x3),
            (R_IQKDPK_HC, B_IQKDPK_HC, 0x1),
            (R_CLK_GCK, B_CLK_GCK, 0x00f_ffff),
            (R_IOQ_IQK_DPK, B_IOQ_IQK_DPK_CLKEN, 0x3),
            (R_IQK_DPK_RST, B_IQK_DPK_RST, 0x1),
            (R_IQK_DPK_PRST, B_IQK_DPK_PRST, 0x1),
            (R_IQK_DPK_PRST_C1, B_IQK_DPK_PRST, 0x1),
            (R_TXRFC, B_TXRFC_RST, 0x1),
            (R_TX_COLLISION_T2R_ST, B_RX_CFIR_PATH_EN, 0x1),
            (R_TX_COLLISION_T2R_ST, B_RX_CFIR_PATH, 0x3),
        ] {
            self.pw32_mask(a, m, v)?;
        }
        self.pw32(R_NCTL_CFG, B_NCTL_CHK_EN)?;
        let mut ok = false;
        for _ in 0..1000 {
            self.pw32(R_NCTL_RW, B_NCTL_CHECK)?;
            sleep(Duration::from_micros(10));
            if self.pr32(R_NCTL_RW)? == B_NCTL_CHECK {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(io::Error::other("NCTL init check timed out"));
        }
        walk(&phy_table(&elements, fw::ELM_RF_NCTL)?, rfe, cv, |a, d| self.config_bb_reg(a, d))?;
        Ok(())
    }

    /// rtw89_phy_config_bb_gain_be: 대부분은 표에 담아 두고, 종류 15만 BB에 바로 씁니다.
    fn config_bb_gain(&self, g: &mut BbGain, addr: u32, data: u32) -> io::Result<()> {
        let (kind, path_bw, band, cfg) = (addr as u8, (addr >> 8) as u8, (addr >> 16) as u8 as usize, (addr >> 24) as u8);
        let (bw, path) = ((path_bw >> 4) as usize, (path_bw & 0xf) as usize);
        if bw >= 2 || band >= GAIN_BANDS || path >= 2 || (0xf9..=0xfe).contains(&addr) {
            return Ok(());
        }
        match (cfg, kind) {
            (0, 0) => fill_bytes(&mut g.lna[band][bw][path][..4], data),
            (0, 1) => fill_bytes(&mut g.lna[band][bw][path][4..], data),
            (0, 2) => fill_bytes(&mut g.tia[band][bw][path], data),
            // 종류의 위 4비트가 대역폭 (RTW89_CMAC_BW_*), 160 MHz는 아래 4비트가 반쪽.
            (1, k) => match k >> 4 {
                0 => g.rpl20[band][path][0] = data as u8 as i8,
                1 => fill_bytes(&mut g.rpl40[band][path], data),
                2 => fill_bytes(&mut g.rpl80[band][path], data),
                3 => {
                    let at = if k & 0xf == 0 { 0 } else { 4 };
                    fill_bytes(&mut g.rpl160[band][path][at..at + 4], data)
                }
                _ => {}
            },
            (3, 0) => fill_bytes(&mut g.lna_op1db[band][bw][path][..4], data),
            (3, 1) => fill_bytes(&mut g.lna_op1db[band][bw][path][4..], data),
            (3, 2) => fill_bytes(&mut g.tia_lna_op1db[band][bw][path][..4], data),
            (3, 3) => fill_bytes(&mut g.tia_lna_op1db[band][bw][path][4..], data),
            (15, _) => self.pw32_mask(addr & 0xfffff, 0xffff_0000, data)?,
            _ => {}
        }
        Ok(())
    }

    /// rtw89_phy_config_bb_reg: 0xf9-0xfe는 기다림, 나머지는 BB 레지스터 쓰기.
    fn config_bb_reg(&self, addr: u32, data: u32) -> io::Result<()> {
        match addr {
            0xfe => sleep(Duration::from_millis(50)),
            0xfd => sleep(Duration::from_millis(5)),
            0xfc => sleep(Duration::from_millis(1)),
            0xfb => sleep(Duration::from_micros(50)),
            0xfa => sleep(Duration::from_micros(5)),
            0xf9 => sleep(Duration::from_micros(1)),
            _ if data == BYPASS_CR_DATA => {}
            _ => self.pw32(addr, data)?,
        }
        Ok(())
    }

    /// eFuse RF 블록에서 RFE 종류와 크리스털 보정값을 읽어 둡니다.
    pub fn read_efuse_rf(&mut self) -> io::Result<()> {
        let map = self.efuse_rf_map()?;
        self.rfe = map[EFUSE_RFE_TYPE];
        self.xtal_k = map[EFUSE_XTAL_K];
        Ok(())
    }
}
