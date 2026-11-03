//! RTL8822B (그리고 그 2T2R USB 형제 RTL8812BU) 올리기. Linux rtw88(main.c, mac.c, fw.c, efuse.c,
//! rtw8822b.c)에서 BSD-3-Clause 라이선스로 옮겼습니다.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use crate::chip::{Regs, delay_ms, err};
use crate::generated::pwrseq;
use crate::generated::regs::*;
use crate::usb::Usb;

pub mod phy;

/// 8822B의 rtw88 tx_pkt_desc_sz.
pub const TX_DESC_SIZE: usize = 48;
pub const RX_DESC_SIZE: usize = 24;
/// [Rtl8822b::rx_reports]의 종류 (5비트, 88x2 벤더 드라이버의 rtw_dump_mac_rx_counters): MAC이 버린
/// MPDU, 그리고 정상으로 받은 VHT MPDU. 버린 것은 무선으로는 멀쩡히 받고도 호스트에 넘기지 못한
/// 것으로, 수신 FIFO(24 KB)가 USB로 퍼내는 것보다 빨리 찰 때 생깁니다: iPhone의 재전송 수와 거의
/// 같았습니다. VHT 정상 수는 PHY의 CRC 카운터와 같은 규모로 움직여 고른 종류가 맞음을 보여 줍니다.
pub const RXERR_MAC_DROP: u32 = 10;
pub const RXERR_VHT_OK: u32 = 16;
/// 송신 버퍼 페이지 (txff_size / 페이지 크기).
const TXFF_PAGES: u16 = (262144 / 128) as u16;
const RXFF_SIZE: u32 = 24576;
const C2H_PKT_BUF: u32 = 256;
const PHY_STATUS_SIZE: u8 = 4;
const SYS_FUNC_EN: u8 = 0xdc;
const USB_TX_AGG_DESC_NUM: u8 = 3;
const RPWM_ADDR: u32 = 0xfe58;

// rtw_dma_mapping
const DMA_EXTRA: u8 = 0;
const DMA_LOW: u8 = 1;
const DMA_NORMAL: u8 = 2;
const DMA_HIGH: u8 = 3;

pub const QSEL_BEACON: u8 = 16;
pub const QSEL_MGMT: u8 = 18;
pub const QSEL_H2C: u8 = 19;

const H2C_PKT_SIZE: usize = 32;
const H2C_PKT_HDR_SIZE: u16 = 8;

/// 올리기에 필요한 efuse 내용 (논리 맵).
#[derive(Debug, Clone, Default)]
pub struct Efuse {
    pub rfe_option: u8,
    pub crystal_cap: u8,
    pub thermal_meter: u8,
    pub pa_type: u8,
    pub lna_type_5g: u8,
    pub rf_board_option: u8,
    pub tx_bb_swing_5g: u8,
    pub usb_mode_switch: bool,
    pub mac: [u8; 6],
    /// 그 밖의 것을 위한 날것의 논리 맵.
    pub map: Vec<u8>,
}

impl Efuse {
    fn parse(map: Vec<u8>) -> Efuse {
        let b = |i: usize| map[i];
        let ff = |v: u8, d: u8| if v == 0xff { d } else { v };
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&map[0x107..0x10d]);
        Efuse {
            rfe_option: b(0xca),
            crystal_cap: ff(b(0xb9), 0),
            thermal_meter: b(0xba),
            pa_type: ff(b(0xbc), 0),
            lna_type_5g: ff(b(0xbf), 0),
            rf_board_option: ff(b(0xc1), 0),
            tx_bb_swing_5g: ff(b(0xc7), 0),
            usb_mode_switch: b(0x06) & 0x80 != 0,
            mac,
            map,
        }
    }
}

/// 송신 FIFO 배치 (3081 CPU 칩의 rtw_set_trx_fifo_info).
#[derive(Debug, Clone, Copy, Default)]
struct Fifo {
    rsvd_boundary: u16,
    fw_txbuf: u16,
    h2cq: u16,
}

impl Fifo {
    fn new() -> Fifo {
        const DRV: u16 = 8;
        const H2C_EXTRAINFO: u16 = 24;
        const H2C_STATICINFO: u16 = 8;
        const H2CQ: u16 = 8;
        const FW_TXBUF: u16 = 4;
        let mut cur = TXFF_PAGES; // CSI 버퍼 페이지 없음
        cur -= FW_TXBUF;
        let fw_txbuf = cur;
        cur -= H2CQ;
        let h2cq = cur;
        cur -= H2C_STATICINFO + H2C_EXTRAINFO + DRV;
        Fifo {
            rsvd_boundary: cur,
            fw_txbuf,
            h2cq,
        }
    }
}

pub struct Rtl8822b {
    pub regs: Regs,
    pub cut: u8,
    pub rf_paths: usize,
    pub efuse: Efuse,
    fifo: Fifo,
    /// 이만큼의 벌크 OUT 파이프에 대한 rqpn: (vo, vi, be, bk, mg, hi).
    rqpn: [u8; 6],
    h2c_seq: AtomicU16,
    /// FIFO 배치가 칩에 있습니다 (mac_init 뒤): 예약 페이지 쓰기는 그 경계를 되돌립니다.
    fifo_set: AtomicBool,
}

impl Rtl8822b {
    pub fn new(usb: Usb) -> io::Result<Rtl8822b> {
        let outs = usb.bulk_out.len();
        let rqpn = match outs {
            2 => [DMA_NORMAL, DMA_NORMAL, DMA_NORMAL, DMA_HIGH, DMA_HIGH, DMA_HIGH],
            3 => [DMA_NORMAL, DMA_NORMAL, DMA_LOW, DMA_LOW, DMA_HIGH, DMA_HIGH],
            4 => [DMA_NORMAL, DMA_NORMAL, DMA_LOW, DMA_LOW, DMA_EXTRA, DMA_HIGH],
            n => return Err(err(format!("{n} bulk OUT pipes"))),
        };
        let regs = Regs { usb };
        let cfg1 = regs.r32(REG_SYS_CFG1)?;
        let cut = ((cfg1 >> 12) & 0xf) as u8;
        let rf_paths = if cfg1 & BIT_RF_TYPE_ID != 0 { 2 } else { 1 };
        tracing::info!("RTL8822B cut {} {}T{}R (SYS_CFG1 0x{cfg1:08x})", (b'A' + cut) as char, rf_paths, rf_paths);
        Ok(Rtl8822b {
            regs,
            cut,
            rf_paths,
            efuse: Efuse::default(),
            fifo: Fifo::new(),
            rqpn,
            h2c_seq: AtomicU16::new(0),
            fifo_set: AtomicBool::new(false),
        })
    }

    /// 큐 선택이 가는 벌크 OUT 엔드포인트 (rtw_usb_parse의 qsel_to_ep).
    pub fn ep_for_qsel(&self, qsel: u8) -> u8 {
        let map = match qsel {
            0 | 3 => self.rqpn[2],
            1 | 2 => self.rqpn[3],
            4 | 5 => self.rqpn[1],
            6 | 7 => self.rqpn[0],
            QSEL_MGMT => self.rqpn[4],
            _ => self.rqpn[5], // 비콘, 높음, H2C
        };
        let idx = match map {
            DMA_HIGH => 0,
            DMA_NORMAL => 1,
            DMA_LOW => 2,
            _ => 3,
        };
        let outs = &self.regs.usb.bulk_out;
        outs[idx.min(outs.len() - 1)]
    }

    // ---- 전원 (mac.c)

    fn pre_system_cfg(&self) -> io::Result<()> {
        let r = &self.regs;
        r.w8(REG_RSV_CTRL, 0)?;
        r.set32(REG_PAD_CTRL1, BIT_PAPE_WLBT_SEL | BIT_LNAON_WLBT_SEL)?;
        r.clr32(REG_LED_CFG, BIT_PAPE_SEL_EN | BIT_LNAON_SEL_EN)?;
        r.set32(REG_GPIO_MUXCFG, BIT_WLRFE_4_5_EN)?;
        r.clr8(REG_SYS_FUNC_EN, BIT_FEN_BB_RSTB | BIT_FEN_BB_GLB_RST)?;
        r.clr8(REG_RF_CTRL, BIT_RF_SDM_RSTB | BIT_RF_RSTB | BIT_RF_EN)?;
        r.clr32(REG_WLRF1, BIT_WLRF1_BBRF_EN)
    }

    /// 칩이 이미 그 상태였으면 false.
    fn power_switch(&self, on: bool) -> io::Result<bool> {
        let r = &self.regs;
        let rpwm = r.r8(RPWM_ADDR)?;
        if r.r16(REG_MCUFW_CTRL)? == 0xc078 {
            r.w8(RPWM_ADDR, (rpwm ^ BIT_RPWM_TOGGLE as u8) & BIT_RPWM_TOGGLE as u8)?;
        }
        let cur = !(r.r8(REG_CR)? == 0xea || r.r8(REG_SYS_STATUS1 + 1)? & 1 != 0);
        if cur == on {
            return Ok(false);
        }
        let flow: [&[crate::chip::PwrCmd]; 2] = if on {
            [pwrseq::CARDDIS_TO_CARDEMU, pwrseq::CARDEMU_TO_ACT]
        } else {
            [pwrseq::ACT_TO_CARDEMU, pwrseq::CARDEMU_TO_CARDDIS]
        };
        for seq in flow {
            r.pwr_seq(seq, self.cut)?;
        }
        if on {
            r.clr8(REG_SYS_STATUS1 + 1, 1)?;
        }
        Ok(true)
    }

    /// rtw_mac_power_on: 꺼진 상태에서 MAC 전원을 올립니다 (켜져 있었으면 껐다 켭니다).
    pub fn power_on(&self) -> io::Result<()> {
        self.pre_system_cfg()?;
        if !self.power_switch(true)? {
            self.power_switch(false)?;
            self.pre_system_cfg()?;
            self.power_switch(true)?;
        }
        let r = &self.regs;
        r.set32(REG_CPU_DMEM_CON, BIT_WL_PLATFORM_RST | BIT_DDMA_EN)?;
        r.set8(REG_SYS_FUNC_EN + 1, SYS_FUNC_EN as u32)?;
        let v = (r.r8(REG_CR_EXT + 3)? & 0xf0) | 0x0c;
        r.w8(REG_CR_EXT + 3, v)?;
        let fw = r.r32(REG_MCUFW_CTRL)?;
        if fw & BIT_BOOT_FSPI_EN != 0 {
            r.w32(REG_MCUFW_CTRL, fw & !BIT_BOOT_FSPI_EN)?;
            r.clr32(REG_GPIO_MUXCFG, BIT_FSPI_EN)?;
        }
        Ok(())
    }

    pub fn power_off(&self) -> io::Result<()> {
        self.power_switch(false).map(|_| ())
    }

    // ---- efuse (efuse.c)

    /// 논리 efuse 맵을 읽습니다. 물리 맵은 쓰는 만큼만 읽습니다.
    pub fn read_efuse(&mut self) -> io::Result<()> {
        const PHYS: usize = 1024 - 96; // 물리 크기에서 보호된 꼬리를 뺀 것
        const LOGICAL: usize = 768;
        let r = &self.regs;
        r.w32m(REG_LDO_EFUSE_CTRL, BIT_MASK_EFUSE_BANK_SEL, 0)?;
        r.clr8(REG_LDO_EFUSE_CTRL + 3, BIT_LDO25_EN)?;
        let mut ctl = r.r32(REG_EFUSE_CTRL)?;
        let mut phy = vec![0xffu8; PHYS + 2];
        let mut have = 0usize;
        let mut byte = |idx: usize, phy: &mut Vec<u8>, have: &mut usize| -> io::Result<u8> {
            while *have <= idx && *have < PHYS {
                ctl &= !(BIT_MASK_EF_DATA | BITS_EF_ADDR);
                ctl |= ((*have as u32) & BIT_MASK_EF_ADDR) << BIT_SHIFT_EF_ADDR;
                r.w32(REG_EFUSE_CTRL, ctl & !BIT_EF_FLAG)?;
                let mut n = 0;
                loop {
                    ctl = r.r32(REG_EFUSE_CTRL)?;
                    if ctl & BIT_EF_FLAG != 0 {
                        break;
                    }
                    n += 1;
                    if n > 1000 {
                        return Err(err("efuse read timed out"));
                    }
                }
                phy[*have] = (ctl & BIT_MASK_EF_DATA) as u8;
                *have += 1;
            }
            Ok(phy[idx])
        };
        let mut log = vec![0xffu8; LOGICAL];
        let mut i = 0usize;
        while i < PHYS {
            let h1 = byte(i, &mut phy, &mut have)?;
            let h2 = byte(i + 1, &mut phy, &mut have)?;
            if h1 == 0xff || (h1 & 0x1f == 0xf && h2 == 0xff) {
                break;
            }
            let (blk, word_en) = if h1 & 0x1f == 0xf {
                i += 2;
                ((((h2 & 0xf0) >> 1) | ((h1 >> 5) & 7)) as usize, h2 & 0xf)
            } else {
                i += 1;
                (((h1 & 0xf0) >> 4) as usize, h1 & 0xf)
            };
            for w in 0..4 {
                if word_en & (1 << w) != 0 {
                    continue;
                }
                let li = (blk << 3) + (w << 1);
                if i + 1 > PHYS || li + 1 > LOGICAL {
                    return Err(err("efuse map is corrupt"));
                }
                log[li] = byte(i, &mut phy, &mut have)?;
                log[li + 1] = byte(i + 1, &mut phy, &mut have)?;
                i += 2;
            }
        }
        tracing::debug!("efuse: {have} physical bytes read");
        self.efuse = Efuse::parse(log);
        Ok(())
    }

    // ---- 펌웨어 (mac.c, fw.c)

    fn ltecoex_read(&self, offset: u32) -> io::Result<u32> {
        let r = &self.regs;
        if !r.ready(LTECOEX_ACCESS_CTRL, LTECOEX_READY, 1)? {
            return Err(err("LTE coex access not ready"));
        }
        r.w32(LTECOEX_ACCESS_CTRL, 0x800f_0000 | offset)?;
        r.r32(LTECOEX_READ_DATA)
    }

    fn ltecoex_write(&self, offset: u32, value: u32) -> io::Result<()> {
        let r = &self.regs;
        if !r.ready(LTECOEX_ACCESS_CTRL, LTECOEX_READY, 1)? {
            return Err(err("LTE coex access not ready"));
        }
        r.w32(LTECOEX_WRITE_DATA, value)?;
        r.w32(LTECOEX_ACCESS_CTRL, 0xc00f_0000 | offset)
    }

    /// 드라이버 내부 패킷(예약 페이지, H2C)용 송신 서술자: 크기, 오프셋, qsel, LS.
    fn internal_desc(&self, size: usize, qsel: u8, offset: bool) -> [u8; TX_DESC_SIZE] {
        let mut d = [0u8; TX_DESC_SIZE];
        let mut w0 = size as u32 & 0xffff;
        if offset {
            w0 |= (TX_DESC_SIZE as u32) << 16 | (1 << 26);
        }
        d[0..4].copy_from_slice(&w0.to_le_bytes());
        let w1 = (qsel as u32 & 0x1f) << 8;
        d[4..8].copy_from_slice(&w1.to_le_bytes());
        checksum(&mut d);
        d
    }

    fn write_internal(&self, data: &[u8], qsel: u8, offset: bool) -> io::Result<()> {
        let mut buf = Vec::with_capacity(TX_DESC_SIZE + data.len());
        buf.extend_from_slice(&self.internal_desc(data.len(), qsel, offset));
        buf.extend_from_slice(data);
        self.regs.usb.bulk_write(self.ep_for_qsel(qsel), &buf)
    }

    /// rtw_fw_write_data_rsvd_page: 비콘 큐로 송신 버퍼의 `pg` 페이지에 data를 씁니다.
    fn write_rsvd_page(&self, pg: u16, data: &[u8]) -> io::Result<()> {
        let r = &self.regs;
        let bcn_ctrl = r.r8(REG_BCN_CTRL)?;
        r.w16(REG_FIFOPAGE_CTRL_2, ((pg as u32 & BIT_MASK_BCN_HEAD_1_V1) | BIT_BCN_VALID_V1) as u16)?;
        let cr1 = r.r8(REG_CR + 1)?;
        r.w8(REG_CR + 1, cr1 | (BIT_ENSWBCN >> 8) as u8)?;
        r.w8(REG_BCN_CTRL, (bcn_ctrl & !(BIT_EN_BCN_FUNCTION as u8)) | BIT_DIS_TSF_UDT as u8)?;
        let mut res = self.write_internal(data, QSEL_BEACON, true);
        if res.is_ok() && !r.ready(REG_FIFOPAGE_CTRL_2, BIT_BCN_VALID_V1, 1)? {
            res = Err(err("reserved page write: beacon valid never set"));
        }
        r.w16(REG_FIFOPAGE_CTRL_2, (self.fifo_boundary_now() as u32 | BIT_BCN_VALID_V1) as u16)?;
        r.w8(REG_BCN_CTRL, bcn_ctrl)?;
        r.w8(REG_CR + 1, cr1)?;
        res
    }

    fn fifo_boundary_now(&self) -> u16 {
        if self.fifo_set.load(Ordering::Relaxed) {
            self.fifo.rsvd_boundary
        } else {
            0
        }
    }

    fn download_to_mem(&self, data: &[u8], dst: u32) -> io::Result<()> {
        let r = &self.regs;
        r.set32(REG_DDMA_CH0CTRL, BIT_DDMACH0_RESET_CHKSUM_STS)?;
        let mut first = true;
        for (n, chunk) in data.chunks(0x1000).enumerate() {
            let mut pkt = chunk.to_vec();
            // 패킷 경계에서 끝나는 벌크 전송에는 칩이 기대하지 않는 ZLP가 필요해집니다.
            if (pkt.len() + TX_DESC_SIZE).is_multiple_of(512) {
                pkt.push(0);
            }
            self.write_rsvd_page(0, &pkt)?;
            if !r.ready(REG_DDMA_CH0CTRL, BIT_DDMACH0_OWN, 0)? {
                return Err(err("firmware DDMA busy"));
            }
            let mut ctrl = BIT_DDMACH0_CHKSUM_EN | BIT_DDMACH0_OWN | (chunk.len() as u32 & BIT_MASK_DDMACH0_DLEN);
            if !first {
                ctrl |= BIT_DDMACH0_CHKSUM_CONT;
            }
            r.w32(REG_DDMA_CH0SA, 0x1878_0000 + TX_DESC_SIZE as u32)?;
            r.w32(REG_DDMA_CH0DA, dst + (n * 0x1000) as u32)?;
            r.w32(REG_DDMA_CH0CTRL, ctrl)?;
            if !r.ready(REG_DDMA_CH0CTRL, BIT_DDMACH0_OWN, 0)? {
                return Err(err("firmware DDMA did not finish"));
            }
            first = false;
        }
        let mut fw_ctrl = r.r8(REG_MCUFW_CTRL)?;
        let imem = dst < 0x0020_0000;
        let (dw, ok) = if imem {
            (BIT_IMEM_DW_OK, BIT_IMEM_CHKSUM_OK)
        } else {
            (BIT_DMEM_DW_OK, BIT_DMEM_CHKSUM_OK)
        };
        if r.r32(REG_DDMA_CH0CTRL)? & BIT_DDMACH0_CHKSUM_STS != 0 {
            fw_ctrl = (fw_ctrl | dw as u8) & !(ok as u8);
            r.w8(REG_MCUFW_CTRL, fw_ctrl)?;
            return Err(err("firmware checksum mismatch"));
        }
        r.w8(REG_MCUFW_CTRL, fw_ctrl | (dw | ok) as u8)
    }

    /// 3081 CPU 칩의 __rtw_download_firmware.
    pub fn download_firmware(&self, fw: &[u8]) -> io::Result<()> {
        if fw.len() < 64 || u16::from_le_bytes([fw[0], fw[1]]) != 0x8822 {
            return Err(err("not RTL8822B firmware"));
        }
        let le32 = |o: usize| u32::from_le_bytes(fw[o..o + 4].try_into().unwrap());
        let dmem = le32(0x24) as usize + 8;
        let imem = le32(0x30) as usize + 8;
        let emem = if fw[0x18] & 0x10 != 0 { le32(0x34) as usize + 8 } else { 0 };
        if 64 + dmem + imem + emem != fw.len() {
            return Err(err("firmware size does not match its header"));
        }
        tracing::info!("firmware {}.{}.{} ({} bytes)", u16::from_le_bytes([fw[4], fw[5]]), fw[6], fw[7], fw.len());
        let r = &self.regs;
        let lte = self.ltecoex_read(0x38)?;
        // wlan_cpu_enable(false)
        r.clr8(REG_SYS_FUNC_EN + 1, BIT_FEN_CPUEN)?;
        r.clr8(REG_RSV_CTRL + 1, BIT_WLMCU_IOIF)?;
        // download_firmware_reg_backup
        let pq_map1 = r.r8(REG_TXDMA_PQ_MAP + 1)?;
        r.w8(REG_TXDMA_PQ_MAP + 1, DMA_HIGH << 6)?;
        let cr = r.r8(REG_CR)?;
        r.w8(REG_CR, (BIT_HCI_TXDMA_EN | BIT_TXDMA_EN) as u8)?;
        r.w32(REG_H2CQ_CSR, BIT_H2CQ_FULL)?;
        let info1 = r.r16(REG_FIFOPAGE_INFO_1)?;
        let rqpn2 = r.r32(REG_RQPN_CTRL_2)? | BIT_LD_RQPN;
        r.w16(REG_FIFOPAGE_INFO_1, 0x200)?;
        r.w32(REG_RQPN_CTRL_2, rqpn2)?;
        let bcn = r.r8(REG_BCN_CTRL)?;
        r.w8(REG_BCN_CTRL, (bcn & !(BIT_EN_BCN_FUNCTION as u8)) | BIT_DIS_TSF_UDT as u8)?;
        // download_firmware_reset_platform
        r.clr8(REG_CPU_DMEM_CON + 2, BIT_WL_PLATFORM_RST >> 16)?;
        r.clr8(REG_SYS_CLK_CTRL + 1, BIT_CPU_CLK_EN >> 8)?;
        r.set8(REG_CPU_DMEM_CON + 2, BIT_WL_PLATFORM_RST >> 16)?;
        r.set8(REG_SYS_CLK_CTRL + 1, BIT_CPU_CLK_EN >> 8)?;
        // start_download_firmware
        let v = (r.r16(REG_MCUFW_CTRL)? & 0x3800) | BIT_MCUFWDL_EN as u16;
        r.w16(REG_MCUFW_CTRL, v)?;
        let mut at = 64;
        let res = (|| {
            self.download_to_mem(&fw[at..at + dmem], le32(0x20) & !(1 << 31))?;
            at += dmem;
            self.download_to_mem(&fw[at..at + imem], le32(0x3c) & !(1 << 31))?;
            at += imem;
            if emem > 0 {
                self.download_to_mem(&fw[at..at + emem], le32(0x38) & !(1 << 31))?;
            }
            Ok(())
        })();
        if let Err(e) = res {
            r.clr8(REG_MCUFW_CTRL, BIT_MCUFWDL_EN)?;
            r.set8(REG_SYS_FUNC_EN + 1, BIT_FEN_CPUEN)?;
            return Err(e);
        }
        // 복원
        r.w8(REG_TXDMA_PQ_MAP + 1, pq_map1)?;
        r.w8(REG_CR, cr)?;
        r.w32(REG_H2CQ_CSR, BIT_H2CQ_FULL)?;
        r.w16(REG_FIFOPAGE_INFO_1, info1)?;
        r.w32(REG_RQPN_CTRL_2, rqpn2)?;
        r.w8(REG_BCN_CTRL, bcn)?;
        // download_firmware_end_flow
        r.w32(REG_TXDMA_STATUS, BTI_PAGE_OVF)?;
        let fw_ctrl = r.r16(REG_MCUFW_CTRL)? as u32;
        if fw_ctrl & BIT_CHECK_SUM_OK == BIT_CHECK_SUM_OK {
            r.w16(REG_MCUFW_CTRL, ((fw_ctrl | BIT_FW_DW_RDY) & !BIT_MCUFWDL_EN) as u16)?;
        }
        // wlan_cpu_enable(true)
        r.set8(REG_RSV_CTRL + 1, BIT_WLMCU_IOIF)?;
        r.set8(REG_SYS_FUNC_EN + 1, BIT_FEN_CPUEN)?;
        self.ltecoex_write(0x38, lte)?;
        let end = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while r.r32(REG_MCUFW_CTRL)? & FW_READY_MASK != FW_READY {
            if std::time::Instant::now() > end {
                let key = r.r32(REG_FW_DBG7)? & FW_KEY_MASK;
                return Err(err(format!(
                    "firmware did not start (MCUFW_CTRL 0x{:04x}, key 0x{key:08x})",
                    r.r16(REG_MCUFW_CTRL)?
                )));
            }
            delay_ms(1);
        }
        self.h2c_seq.store(0, Ordering::Relaxed);
        tracing::info!("firmware running (MCUFW_CTRL 0x{:04x})", r.r16(REG_MCUFW_CTRL)?);
        Ok(())
    }

    // ---- H2C (fw.c)

    /// H2C 큐로 보내는 H2C 패킷 (펌웨어 오프로드 명령).
    pub fn h2c_pkt(&self, sub_id: u8, payload: &[u8]) -> io::Result<()> {
        let mut p = [0u8; H2C_PKT_SIZE];
        let total = H2C_PKT_HDR_SIZE + payload.len() as u16;
        let w0: u32 = 0x01 | (0xff << 8) | ((sub_id as u32) << 16);
        let seq = self.h2c_seq.fetch_add(1, Ordering::Relaxed);
        let w1: u32 = total as u32 | ((seq as u32) << 16);
        p[0..4].copy_from_slice(&w0.to_le_bytes());
        p[4..8].copy_from_slice(&w1.to_le_bytes());
        p[8..8 + payload.len()].copy_from_slice(payload);
        self.write_internal(&p, QSEL_H2C, false)
    }

    // ---- MAC (mac.c, rtw8822b.c)

    pub fn mac_init(&self) -> io::Result<()> {
        let r = &self.regs;
        // txdma_queue_mapping
        let [vo, vi, be, bk, mg, hi] = self.rqpn.map(|m| m as u32);
        let pq = (hi << 14) | (mg << 12) | (bk << 10) | (be << 8) | (vi << 6) | (vo << 4);
        r.w16(REG_TXDMA_PQ_MAP, pq as u16)?;
        r.w8(REG_CR, 0)?;
        r.w8(REG_CR, MAC_TRX_ENABLE as u8)?;
        r.w32(REG_H2CQ_CSR, BIT_H2CQ_FULL)?;
        r.set8(REG_TXDMA_PQ_MAP, BIT_RXDMA_ARBBW_EN)?;
        // priority_queue_cfg
        let f = self.fifo;
        let (hq, nq, lq, exq, gapq) = match self.regs.usb.bulk_out.len() {
            2 => (64u16, 64u16, 0u16, 0u16, 1u16),
            3 => (64, 64, 64, 0, 1),
            _ => (64, 64, 64, 64, 1),
        };
        let pubq = f.rsvd_boundary - hq - nq - lq - exq - gapq;
        r.w16(REG_FIFOPAGE_INFO_1, hq)?;
        r.w16(REG_FIFOPAGE_INFO_2, lq)?;
        r.w16(REG_FIFOPAGE_INFO_3, nq)?;
        r.w16(REG_FIFOPAGE_INFO_4, exq)?;
        r.w16(REG_FIFOPAGE_INFO_5, pubq)?;
        r.set32(REG_RQPN_CTRL_2, BIT_LD_RQPN)?;
        r.w16(REG_FIFOPAGE_CTRL_2, f.rsvd_boundary)?;
        r.set8(REG_FWHW_TXQ_CTRL + 2, BIT_EN_WR_FREE_TAIL >> 16)?;
        r.w16(REG_BCNQ_BDNY_V1, f.rsvd_boundary)?;
        r.w16(REG_FIFOPAGE_CTRL_2 + 2, f.rsvd_boundary)?;
        r.w16(REG_BCNQ1_BDNY_V1, f.rsvd_boundary)?;
        r.w32(REG_RXFF_BNDY, RXFF_SIZE - C2H_PKT_BUF - 1)?;
        r.w8m(REG_AUTO_LLT_V1, BIT_MASK_BLK_DESC_NUM, USB_TX_AGG_DESC_NUM)?;
        r.w8(REG_AUTO_LLT_V1 + 3, USB_TX_AGG_DESC_NUM)?;
        r.set8(REG_TXDMA_OFFSET_CHK + 1, 1 << 1)?;
        r.set8(REG_AUTO_LLT_V1, BIT_AUTO_INIT_LLT_V1)?;
        if !r.ready(REG_AUTO_LLT_V1, BIT_AUTO_INIT_LLT_V1, 0)? {
            return Err(err("LLT init timed out"));
        }
        r.w8(REG_CR + 3, 0)?;
        self.fifo_set.store(true, Ordering::Relaxed);
        // init_h2c
        let h2cq_addr = (f.h2cq as u32) << 7;
        let h2cq_size = 8u32 << 7;
        let v = r.r32(REG_H2C_HEAD)?;
        r.w32(REG_H2C_HEAD, (v & 0xfffc_0000) | h2cq_addr)?;
        let v = r.r32(REG_H2C_READ_ADDR)?;
        r.w32(REG_H2C_READ_ADDR, (v & 0xfffc_0000) | h2cq_addr)?;
        let v = r.r32(REG_H2C_TAIL)?;
        r.w32(REG_H2C_TAIL, (v & 0xfffc_0000) | (h2cq_addr + h2cq_size))?;
        let v = r.r8(REG_H2C_INFO)?;
        r.w8(REG_H2C_INFO, (v & 0xfc) | 0x01)?;
        let v = r.r8(REG_H2C_INFO)?;
        r.w8(REG_H2C_INFO, (v & 0xfb) | 0x04)?;
        let v = r.r8(REG_TXDMA_OFFSET_CHK + 1)?;
        r.w8(REG_TXDMA_OFFSET_CHK + 1, (v & 0x7f) | 0x80)?;
        let wp = r.r32(REG_H2C_PKT_WRITEADDR)? & 0x3ffff;
        let rp = r.r32(REG_H2C_PKT_READADDR)? & 0x3ffff;
        let free = if wp >= rp { h2cq_size - (wp - rp) } else { rp - wp };
        if free != h2cq_size {
            return Err(err("H2C queue mismatch"));
        }
        self.mac_init_8822b()?;
        // rtw_drv_info_cfg
        r.w8(REG_RX_DRVINFO_SZ, PHY_STATUS_SIZE)?;
        let v = (r.r8(REG_TRXFF_BNDY + 1)? & 0xf0) | 0xf;
        r.w8(REG_TRXFF_BNDY + 1, v)?;
        r.set32(REG_RCR, BIT_APP_PHYSTS)?;
        r.clr32(REG_WMAC_OPTION_FUNCTION + 4, (1 << 8) | (1 << 9))?;
        // rtw_usb_interface_cfg: 버스 속도에 맞춘 RX DMA 버스트
        let burst = if self.regs.usb.high_speed { 1 } else { 2 }; // 512 또는 64바이트
        r.w8(REG_RXDMA_MODE, (BIT_DMA_BURST_CNT | BIT_DMA_MODE | (burst << 4)) as u8)?;
        r.set16(REG_TXDMA_OFFSET_CHK, BIT_DROP_DATA_EN)?;
        Ok(())
    }

    fn mac_init_8822b(&self) -> io::Result<()> {
        let r = &self.regs;
        r.clr8(REG_SW_AMPDU_BURST_MODE_CTRL, BIT_PRE_TX_CMD)?;
        r.w8(REG_AMPDU_MAX_TIME_V1, 0x70)?;
        r.set8(REG_TX_HANG_CTRL, BIT_EN_EOF_V1)?;
        r.w32(REG_PROT_MODE_CTRL, 0xff | (0x08 << 8) | (0x20 << 16) | (0x20 << 24))?;
        r.w16(REG_BAR_MODE_CTRL + 2, 0x01 | (0x08 << 8))?;
        r.w8(REG_FAST_EDCA_VOVI_SETTING, 0x06)?;
        r.w8(REG_FAST_EDCA_VOVI_SETTING + 2, 0x06)?;
        r.w8(REG_FAST_EDCA_BEBK_SETTING, 0x06)?;
        r.w8(REG_FAST_EDCA_BEBK_SETTING + 2, 0x06)?;
        r.clr8(REG_TIMER0_SRC_SEL, BIT_TSFT_SEL_TIMER0)?;
        r.w16(REG_TXPAUSE, 0)?;
        r.w8(REG_SLOT, 0x09)?;
        r.w8(REG_PIFS, 0x19)?;
        let sifs = 0x0a | (0x0e << BIT_SHIFT_SIFS_OFDM_CTX) | (0x10 << BIT_SHIFT_SIFS_CCK_TRX) | (0x10 << BIT_SHIFT_SIFS_OFDM_TRX);
        r.w32(REG_SIFS, sifs)?;
        r.w16(REG_EDCA_VO_PARAM + 2, 0x186)?;
        r.w16(REG_EDCA_VI_PARAM + 2, 0x3bc)?;
        r.w32(REG_RD_NAV_NXT, 0x05 | (0x1b << 16))?;
        r.w16(REG_RXTSF_OFFSET_CCK, 0x30 | (0x30 << 8))?;
        r.set8(REG_BCN_CTRL, BIT_EN_BCN_FUNCTION)?;
        r.w32(REG_TBTT_PROHIBIT, 0x04 | (0x064 << BIT_SHIFT_TBTT_HOLD_TIME_AP))?;
        r.w8(REG_DRVERLYINT, 0x04)?;
        r.w8(REG_BCNDMATIM, 0x02)?;
        r.clr8(REG_TX_PTCL_CTRL + 1, BIT_SIFS_BK_EN >> 8)?;
        r.w16(REG_RXFLTMAP0, 0xffff)?;
        r.w16(REG_RXFLTMAP1, 0x0fff)?;
        r.w16(REG_RXFLTMAP2, 0xffff)?;
        r.w32(REG_RCR, 0xe400_220e)?;
        r.w8(REG_RX_PKT_LIMIT, (12288 >> 9) as u8)?;
        r.w8(REG_TCR + 2, 0x30)?;
        r.w8(REG_TCR + 1, 0x30)?;
        r.w32(REG_WMAC_OPTION_FUNCTION + 8, 0xb081_0041)?;
        r.w8(REG_WMAC_OPTION_FUNCTION + 4, 0x98)?;
        r.set8(REG_SND_PTCL_CTRL, BIT_DIS_CHK_VHTSIGB_CRC)
    }

    /// rtw_usb_dynamic_rx_agg_v1: USB 수신 묶음 (5페이지, 시간 제한 0x20).
    pub fn rx_agg(&self, on: bool) -> io::Result<()> {
        let r = &self.regs;
        r.set8(REG_TXDMA_PQ_MAP, BIT_RXDMA_AGG_EN)?;
        r.clr8(REG_RXDMA_AGG_PG_TH + 3, 1 << 7)?;
        let (size, timeout) = if on { (0x5u16, 0x20u16) } else { (0, 1) };
        r.w16(REG_RXDMA_AGG_PG_TH, size | (timeout << 8))
    }

    /// MAC의 수신 보고 카운터들을 읽고 한꺼번에 0으로 돌립니다 (REG_RXERR_RPT, 종류는 [RXERR_MAC_DROP]
    /// 같은 상수). 리셋은 모든 종류를 지우므로 다 읽은 뒤 한 번만 합니다. 지난 호출 뒤의 수입니다.
    pub fn rx_reports(&self, kinds: &[u32]) -> io::Result<Vec<u32>> {
        const REG_RXERR_RPT: u32 = 0x0664;
        const RESET: u32 = 1 << 27;
        const SEL_BIT4: u32 = 1 << 26;
        let r = &self.regs;
        let mut counts = Vec::with_capacity(kinds.len());
        for &kind in kinds {
            let select = ((kind & 0xf) << 28) | if kind & 0x10 != 0 { SEL_BIT4 } else { 0 };
            r.w32(REG_RXERR_RPT, select)?;
            counts.push(r.r32(REG_RXERR_RPT)? & 0xffff);
        }
        r.w32(REG_RXERR_RPT, RESET)?;
        r.w32(REG_RXERR_RPT, 0)?;
        Ok(counts)
    }

    /// rtw_fw_send_general_info + rtw_fw_send_phydm_info.
    pub fn send_fw_info(&self) -> io::Result<()> {
        let mut general = [0u8; 4];
        general[2] = (self.fifo.fw_txbuf - self.fifo.rsvd_boundary) as u8;
        self.h2c_pkt(0x0d, &general)?;
        let paths = if self.rf_paths == 2 { 0x3u8 } else { 0x1 };
        let rf_type = if self.rf_paths == 2 { 2u8 } else { 0 };
        let mut phydm = [0u8; 8];
        phydm[0] = self.efuse.rfe_option;
        phydm[1] = rf_type;
        phydm[2] = self.cut;
        phydm[3] = paths | (paths << 4);
        self.h2c_pkt(0x11, &phydm)
    }
}

/// 처음 32바이트에 대한 fill_txdesc_checksum_common.
pub fn checksum(d: &mut [u8]) {
    d[28] = 0;
    d[29] = 0;
    let mut c = 0u16;
    for i in 0..16 {
        c ^= u16::from_le_bytes([d[2 * i], d[2 * i + 1]]);
    }
    d[28..30].copy_from_slice(&c.to_le_bytes());
}
