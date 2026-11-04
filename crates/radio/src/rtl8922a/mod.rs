//! Realtek RTL8912AU (RTL8922A 계열, Wi-Fi 7) USB 어댑터, 예: ipTIME BE6500UA. Linux rtw89에서
//! BSD-3-Clause로 옮기는 중입니다 (NOTICE.md): 지금은 USB 3 전환, 전원 켜기, eFuse, 펌웨어 내려받기.
//! 레지스터 주소는 24비트이고, 제조사 요청의 wValue/wIndex로 나뉘어 갑니다 ([Usb::read_wide]).
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::usb::{Usb, UsbId};

mod chan;
mod dm;
pub mod fw;
mod mac;
mod phy;
mod rfk;
pub mod rx;
mod tx;
mod txpwr;
// rtw89의 reg.h에서 생성: 손으로 고치거나 rustfmt로 정렬하지 않습니다 (tools/gen_rtw89.py).
#[rustfmt::skip]
mod generated;

pub use chan::{Band, Bw, Chan};
pub use dm::DigReport;
/// 레지스터 정의 (시험 도구가 직접 읽을 때).
pub use generated::regs;
use generated::regs::*;
pub use rfk::RfkResult;
pub use tx::{BACAM_STATIC_NUM, HwRate, TX_DESC_SIZE, TxFrame};
pub use txpwr::TxPowerTables;

/// Realtek의 RTL8922A 펌웨어 (linux-firmware rtw89/rtw8922a_fw-4.bin, 수정 없이 재배포 가능).
pub static FIRMWARE: &[u8] = include_bytes!("../../firmware/rtw8922a_fw-4.bin");

/// 이 드라이버로 여는 USB ID (시험한 것: ipTIME BE6500UA, Realtek 기본 ID).
pub const USB_IDS: &[UsbId] = &[UsbId::RTL8912AU];

/// H2C와 펌웨어 조각 앞의 서술자 (rtw89_rxdesc_short_v2 크기, 첫 dword만 씀).
const H2C_DESC_SIZE: usize = 24;
const RX_TYPE_H2C: u32 = 13;
const RX_TYPE_FWDL: u32 = 14;
/// 길이가 512의 배수인 전송을 피하려고 덧붙이는 바이트 (RTW89_USB_MOD512_PADDING).
const MOD512_PADDING: usize = 4;
/// 벌크 OUT 순서에서 H2C 채널의 자리 (rtw8922a_usb_info.bulkout_id[RTW89_DMA_H2C]).
const BULKOUT_H2C: usize = 2;
/// H2C 머리말 (fwcmd_hdr).
const H2C_HEADER_LEN: usize = 8;
const H2C_CAT_MAC: u32 = 1;
const H2C_CL_MAC_FWDL: u32 = 3;
const H2C_FUNC_MAC_FWHDR_DL: u32 = 0;
/// rtw89_fwdl_status
const FWDL_WCPU_FW_INIT_RDY: u8 = 7;
const CHIP_CAV: u8 = 0;
/// rtw89_bbmcu_write32의 BB MCU 주소 공간.
const BBMCU_ADDR_OFFSET: u32 = 0x30000;
/// rtw8922a.c의 bb_mcu0_init_reg.
const BB_MCU0_INIT: [(u32, u32); 10] = [
    (0x6990, 0x0000_0000),
    (0x6994, 0x0000_0000),
    (0x6998, 0x0000_0000),
    (0x6820, 0xffff_fffe),
    (0x6800, 0xc000_0ffe),
    (0x6808, 0x7654_3210),
    (0x6814, 0xbfbf_b000),
    (0x6818, 0x0478_c009),
    (0x6800, 0xc000_0fff),
    (0x6820, 0xffff_ffff),
];
const XTAL_SI_PLL: u8 = 0xe0;
const XTAL_SI_PLL_1: u8 = 0xe1;
const XTAL_SI_ANAPAR_WL: u8 = 0x90;
const XTAL_SI_WL_RFC_S0: u8 = 0x80;
const XTAL_SI_WL_RFC_S1: u8 = 0x81;
const XTAL_SI_XREF_RF1: u8 = 0x2d;
const XTAL_SI_XREF_RF2: u8 = 0x2e;
const XTAL_SI_SRAM_CTRL: u8 = 0xa1;
const XTAL_SI_SRAM_DIS: u8 = 1 << 1;
const XTAL_SI_CHIP_ID_L: u8 = 0xfd;
const XTAL_SI_CHIP_ID_H: u8 = 0xfe;
const EFUSE_SEC_START: u32 = 0x1580;
const EFUSE_FV_OFFSET: u32 = 0x17ca;

/// mask 자리의 값.
fn get(v: u32, mask: u32) -> u32 {
    (v & mask) >> mask.trailing_zeros()
}

/// mask 자리를 x로 바꾼 값.
fn put(v: u32, mask: u32, x: u32) -> u32 {
    (v & !mask) | ((x << mask.trailing_zeros()) & mask)
}

fn timeout(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, what.to_string())
}

/// 펌웨어가 칩에서 돌기 전까지의 RTL8912AU.
pub struct Rtl8922a {
    pub usb: Usb,
    /// 칩 컷 (rtw89_hal::cv; eFuse에 따로 적혀 있으면 그것).
    pub cv: u8,
    pub usb3: bool,
    /// 보안 부팅 칩이면 서명 키 선택.
    pub secure: Option<fw::Secure>,
    /// eFuse의 RFE 종류(보드 배선)와 크리스털 보정값 ([Rtl8922a::read_efuse_rf]).
    pub rfe: u8,
    pub xtal_k: u8,
    /// A-die 번호 (rtw89_hal::aid): 펌웨어의 칩 변형별 요소를 고를 때.
    pub aid: u16,
    /// 송신 출력 표와 대역별 규제 ([Rtl8922a::set_channel]이 채널마다 씁니다).
    txpwr: Option<(TxPowerTables, [u8; 3])>,
    h2c_seq: AtomicU8,
    /// 펌웨어의 BB 이득 표 ([Rtl8922a::phy_init]에서 읽음).
    gain: Box<phy::BbGain>,
    /// RF 보정 결과를 담는 표의 채널들 (rtw89_rfk_mcc_info_data; [Rtl8922a::set_channel]).
    rfk_tbl: Mutex<[Option<Chan>; chan::RFK_TABLES]>,
}

impl Rtl8922a {
    /// 어댑터를 열고 채널 c에 올려 RF 보정까지 마칩니다 (rtw89_core_start, 채널 설정, RF 보정).
    /// country(ISO 3166 두 글자)의 규제로 송신 출력을 정하고, 모르면 세계 공통 한도를 씁니다.
    pub fn start(c: &Chan, country: Option<[u8; 2]>) -> io::Result<(Rtl8922a, Vec<RfkResult>)> {
        Self::start_with(UsbId::RTL8912AU, c, country)
    }

    /// [Rtl8922a::start]와 같지만 USB ID를 고릅니다.
    pub fn start_with(id: UsbId, c: &Chan, country: Option<[u8; 2]>) -> io::Result<(Rtl8922a, Vec<RfkResult>)> {
        let mut chip = Rtl8922a::open_with(id)?;
        chip.power_on()?;
        chip.download_firmware(FIRMWARE)?;
        chip.mac_init()?;
        chip.read_efuse_rf()?;
        chip.aid = chip.xtal_si_read(XTAL_SI_CHIP_ID_L)? as u16 | (chip.xtal_si_read(XTAL_SI_CHIP_ID_H)? as u16) << 8;
        let tables = TxPowerTables::load(FIRMWARE, chip.aid, chip.rfe)?;
        let regd = tables.regd_of(country);
        chip.txpwr = Some((tables, regd));
        chip.phy_init(FIRMWARE)?;
        chip.dm_init(FIRMWARE)?;
        chip.set_channel(c)?;
        let rfk = {
            let mut reader = chip.usb.reader(4, 32 * 1024)?;
            chip.calibrate(&mut reader, c, true)?
        };
        Ok((chip, rfk))
    }

    /// 어댑터를 엽니다. USB 2로 붙어 있으면 USB 3으로 다시 연결하게 하고 기다립니다
    /// (rtw89_usb_switch_mode_be; USB 2 포트면 한 번 시도한 표시가 남아 다시 하지 않습니다).
    pub fn open() -> io::Result<Self> {
        Self::open_with(UsbId::RTL8912AU)
    }

    /// [Rtl8922a::open]과 같지만 USB ID를 고릅니다. 막 꽂은 어댑터는 WinUSB가 잡기 전에 목록에 나올
    /// 수 있어 3초까지 기다립니다.
    pub fn open_with(id: UsbId) -> io::Result<Self> {
        let t0 = Instant::now();
        let mut usb = loop {
            match Usb::open(id) {
                Err(e) if e.kind() == io::ErrorKind::NotFound && t0.elapsed() < Duration::from_secs(3) => sleep(Duration::from_millis(100)),
                r => break r?,
            }
        };
        if usb.max_packet < 1024 && Self::switch_to_usb3(&usb)? {
            drop(usb);
            let t = Instant::now();
            sleep(Duration::from_millis(1500));
            usb = loop {
                match Usb::open(id) {
                    Ok(u) => break u,
                    Err(_) if t.elapsed() < Duration::from_secs(15) => sleep(Duration::from_millis(300)),
                    Err(e) => return Err(e),
                }
            };
        }
        let usb3 = usb.max_packet >= 1024;
        // rtw89는 첫 번째 벌크 IN(in_pipe[0])으로 받습니다.
        usb.bulk_in = usb.bulk_ins[0];
        let mut chip = Rtl8922a {
            usb,
            cv: 0,
            usb3,
            secure: None,
            rfe: 0,
            xtal_k: 0,
            aid: 0,
            txpwr: None,
            h2c_seq: AtomicU8::new(0),
            gain: Box::default(),
            rfk_tbl: Mutex::new([None; chan::RFK_TABLES]),
        };
        chip.cv = get(chip.r32(R_AX_SYS_CFG1)?, B_AX_CHIP_VER_MASK) as u8;
        Ok(chip)
    }

    /// USB 3 전환을 요청했으면 true (장치가 곧 끊겼다가 다시 붙습니다).
    fn switch_to_usb3(usb: &Usb) -> io::Result<bool> {
        let mut pad = usb.read_wide(R_BE_PAD_CTRL2, 4)?;
        if get(pad, B_BE_MATCH_CNT) == USB_SWITCH_DELAY {
            return Ok(false);
        }
        pad = put(pad, B_BE_MATCH_CNT, USB_SWITCH_DELAY);
        pad |= B_BE_RSM_EN_V1 | B_BE_NO_PDN_CHIPOFF_V1 | B_BE_USB_AUTO_INSTALL_MASK | B_BE_USB23_SW_MODE;
        pad &=
            !(B_BE_USB3_FORCE | B_BE_USB2_FORCE | B_BE_FORCE_U3_CK | B_BE_FORCE_U2_CK | B_BE_FORCE_CLK_U2 | B_BE_USB3_GEN_MODE | B_BE_USB3_LANE_MODE);
        // 장치가 바로 끊기므로 응답 실패는 괜찮습니다.
        let _ = usb.write_wide(R_BE_PAD_CTRL2, pad, 4);
        Ok(true)
    }

    // 레지스터 접근 (rtw89_read32 등).
    pub fn r32(&self, a: u32) -> io::Result<u32> {
        self.usb.read_wide(a, 4)
    }
    pub fn r16(&self, a: u32) -> io::Result<u16> {
        Ok(self.usb.read_wide(a, 2)? as u16)
    }
    pub fn r8(&self, a: u32) -> io::Result<u8> {
        Ok(self.usb.read_wide(a, 1)? as u8)
    }
    pub fn w32(&self, a: u32, v: u32) -> io::Result<()> {
        self.usb.write_wide(a, v, 4)
    }
    pub fn w16(&self, a: u32, v: u16) -> io::Result<()> {
        self.usb.write_wide(a, v as u32, 2)
    }
    pub fn w8(&self, a: u32, v: u8) -> io::Result<()> {
        self.usb.write_wide(a, v as u32, 1)
    }
    fn set32(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w32(a, self.r32(a)? | bits)
    }
    fn clr32(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w32(a, self.r32(a)? & !bits)
    }
    fn set16(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w16(a, self.r16(a)? | bits as u16)
    }
    fn clr16(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w16(a, self.r16(a)? & !(bits as u16))
    }
    fn set8(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w8(a, self.r8(a)? | bits as u8)
    }
    fn clr8(&self, a: u32, bits: u32) -> io::Result<()> {
        self.w8(a, self.r8(a)? & !(bits as u8))
    }
    fn w32_mask(&self, a: u32, mask: u32, x: u32) -> io::Result<()> {
        self.w32(a, put(self.r32(a)?, mask, x))
    }
    fn w16_mask(&self, a: u32, mask: u32, x: u32) -> io::Result<()> {
        self.w16(a, put(self.r16(a)? as u32, mask, x) as u16)
    }

    /// done(값)이 참이 될 때까지 읽습니다.
    fn poll(&self, a: u32, ms: u64, what: &str, done: impl Fn(u32) -> bool) -> io::Result<u32> {
        let t = Instant::now();
        loop {
            let v = self.r32(a)?;
            if done(v) {
                return Ok(v);
            }
            if t.elapsed() > Duration::from_millis(ms) {
                return Err(timeout(&format!("{what} (0x{a:05x} = 0x{v:08x})")));
            }
        }
    }

    /// 크리스털 쪽 SI 레지스터 쓰기 (rtw89_mac_write_xtal_si_be).
    fn xtal_si_write(&self, offset: u8, val: u8, mask: u8) -> io::Result<()> {
        let v = put(0, B_BE_WL_XTAL_SI_ADDR_MASK, offset as u32)
            | put(0, B_BE_WL_XTAL_SI_DATA_MASK, val as u32)
            | put(0, B_BE_WL_XTAL_SI_BITMASK_MASK, mask as u32)
            | put(0, B_BE_WL_XTAL_SI_MODE_MASK, XTAL_SI_NORMAL_WRITE)
            | B_BE_WL_XTAL_SI_CMD_POLL;
        self.w32(R_BE_WLAN_XTAL_SI_CTRL, v)?;
        self.poll(R_BE_WLAN_XTAL_SI_CTRL, 50, "xtal si write", |v| v & B_BE_WL_XTAL_SI_CMD_POLL == 0)?;
        Ok(())
    }

    /// 크리스털 쪽 SI 레지스터 읽기 (rtw89_mac_read_xtal_si_be).
    fn xtal_si_read(&self, offset: u8) -> io::Result<u8> {
        let v = put(0, B_BE_WL_XTAL_SI_ADDR_MASK, offset as u32) | put(0, B_BE_WL_XTAL_SI_MODE_MASK, XTAL_SI_NORMAL_READ) | B_BE_WL_XTAL_SI_CMD_POLL;
        self.w32(R_BE_WLAN_XTAL_SI_CTRL, v)?;
        let v = self.poll(R_BE_WLAN_XTAL_SI_CTRL, 50, "xtal si read", |v| v & B_BE_WL_XTAL_SI_CMD_POLL == 0)?;
        Ok(get(v, B_BE_WL_XTAL_SI_DATA_MASK) as u8)
    }

    /// 크리스털 용량 조정값 (XTAL_SI_XTAL_SC_XI, _XO): 주파수 오차를 맞출 때 rtw89와 공식 드라이버가 둘을
    /// 같은 값으로 씁니다.
    fn xtal_cap(&self) -> io::Result<(u8, u8)> {
        Ok((self.xtal_si_read(0x04)?, self.xtal_si_read(0x05)?))
    }

    /// [Rtl8922a::xtal_cap]을 바꿉니다 (공식 드라이버처럼 XO 다음 XI).
    fn set_xtal_cap(&self, cap: u8) -> io::Result<()> {
        self.xtal_si_write(0x05, cap, 0xff)?;
        self.xtal_si_write(0x04, cap, 0xff)
    }

    /// 켜기 전에 칩을 꺼진 상태로 맞춥니다 (rtw89_mac_reset_pwr_state_be).
    fn reset_pwr_state(&self) -> io::Result<()> {
        let v = (self.r32(R_BE_SYSON_FSM_MON)? & WLAN_FSM_MASK) | WLAN_FSM_SET;
        self.w32(R_BE_SYSON_FSM_MON, v)?;
        self.poll(R_BE_SYSON_FSM_MON, 2000, "WLAN PMC", |v| v & WLAN_FSM_STATE_MASK == WLAN_FSM_IDLE)?;
        let state = get(self.r32(R_BE_IC_PWR_STATE)?, B_BE_WLMAC_PWR_STE_MASK);
        if state > MAC_AX_MAC_LPS {
            return Ok(());
        }
        self.clr32(R_BE_HCI_OPT_CTRL, B_BE_HAXIDMA_IO_EN)?;
        self.poll(R_BE_HCI_OPT_CTRL, 2000, "HAXI IO", |v| {
            v & (B_BE_HAXIDMA_IO_ST | B_BE_HAXIDMA_BACKUP_RESTORE_ST) == 0
        })?;
        self.clr32(R_BE_HCI_OPT_CTRL, B_BE_HCI_WLAN_IO_EN)?;
        self.poll(R_BE_HCI_OPT_CTRL, 2000, "WLAN IO", |v| v & B_BE_HCI_WLAN_IO_ST == 0)?;
        if state == MAC_AX_MAC_LPS {
            self.set32(R_BE_WLLPS_CTRL, B_BE_FORCE_LEAVE_LPS)?;
            self.poll(R_BE_IC_PWR_STATE, 2000, "leave LPS", |v| get(v, B_BE_WLMAC_PWR_STE_MASK) == MAC_AX_MAC_ON)?;
        }
        if state != MAC_AX_MAC_OFF {
            self.set32(R_BE_SYS_PW_CTRL, B_BE_EN_WLON)?;
            self.set32(R_BE_SYS_PW_CTRL, B_BE_APFM_OFFMAC)?;
            self.poll(R_BE_SYS_PW_CTRL, 2000, "MAC off", |v| v & B_BE_APFM_OFFMAC == 0)?;
        }
        if state == MAC_AX_MAC_LPS {
            self.clr32(R_BE_WLLPS_CTRL, B_BE_FORCE_LEAVE_LPS)?;
        }
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_EN_WLON)?;
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_APFM_SWLPS)?;
        Ok(())
    }

    /// 전원 켜기 (rtw89_mac_power_switch(on) + rtw8922a_pwr_on_func), 그리고 eFuse의 컷과 보안
    /// 부팅 정보.
    pub fn power_on(&mut self) -> io::Result<()> {
        // rtw89_mac_power_switch_boot_mode: USB로 처음 붙으면 부트 모드일 수 있습니다.
        if get(self.r32(R_AX_GPIO_MUXCFG)?, B_AX_BOOT_MODE) != 0 {
            self.clr32(R_AX_SYS_PW_CTRL, B_AX_APFN_ONMAC)?;
            self.clr32(R_AX_SYS_STATUS1, B_AX_AUTO_WLPON)?;
            self.clr32(R_AX_GPIO_MUXCFG, B_AX_BOOT_MODE)?;
            self.clr32(R_AX_RSV_CTRL, B_AX_R_DIS_PRST)?;
        }
        // 앞 세션이 칩을 켜 둔 채 끝났으면 아날로그와 RF까지 모두 끄고 다시 켭니다 (rtw89_mac_pwr_on이
        // 이미 켜진 칩에 하는 것).
        if get(self.r32(R_BE_IC_PWR_STATE)?, B_BE_WLMAC_PWR_STE_MASK) == MAC_AX_MAC_ON {
            self.pwr_off()?;
        }
        self.reset_pwr_state()?;

        self.clr32(R_BE_SYS_PW_CTRL, B_BE_AFSM_WLSUS_EN | B_BE_AFSM_PCIE_SUS_EN)?;
        self.set32(R_BE_SYS_PW_CTRL, B_BE_DIS_WLBT_PDNSUSEN_SOPC)?;
        self.set32(R_BE_WLLPS_CTRL, B_BE_DIS_WLBT_LPSEN_LOPC)?;
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_APDM_HPDN)?;
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_APFM_SWLPS)?;
        self.poll(R_BE_SYS_PW_CTRL, 3000, "system power", |v| v & B_BE_RDY_SYSPWR != 0)?;
        self.set32(R_BE_SYS_PW_CTRL, B_BE_EN_WLON)?;
        self.set32(R_BE_WLRESUME_CTRL, B_BE_LPSROP_CMAC0 | B_BE_LPSROP_CMAC1)?;
        self.set32(R_BE_SYS_PW_CTRL, B_BE_APFN_ONMAC)?;
        self.poll(R_BE_SYS_PW_CTRL, 3000, "MAC on", |v| v & B_BE_APFN_ONMAC == 0)?;
        self.clr32(R_BE_AFE_ON_CTRL1, B_BE_REG_CK_MON_CK960M_EN)?;
        self.set8(R_BE_ANAPAR_POW_MAC, B_BE_POW_PC_LDO_PORT0 | B_BE_POW_PC_LDO_PORT1)?;
        self.clr32(R_BE_FEN_RST_ENABLE, B_BE_R_SYM_ISO_ADDA_P02PP | B_BE_R_SYM_ISO_ADDA_P12PP)?;
        self.set8(R_BE_PLATFORM_ENABLE, B_BE_PLATFORM_EN)?;
        self.set32(R_BE_HCI_OPT_CTRL, B_BE_HCI_WLAN_IO_EN)?;
        self.poll(R_BE_HCI_OPT_CTRL, 3000, "HCI WLAN IO", |v| v & B_BE_HCI_WLAN_IO_ST != 0)?;

        self.xtal_si_write(XTAL_SI_PLL, 0x02, 0x02)?;
        self.xtal_si_write(XTAL_SI_PLL, 0x01, 0x01)?;
        self.set32(R_BE_SYS_ADIE_PAD_PWR_CTRL, B_BE_SYM_PADPDN_WL_RFC1_1P3)?;
        self.xtal_si_write(XTAL_SI_ANAPAR_WL, 0x40, 0x40)?;
        self.set32(R_BE_SYS_ADIE_PAD_PWR_CTRL, B_BE_SYM_PADPDN_WL_RFC0_1P3)?;
        for (offset, val, mask) in [
            (XTAL_SI_ANAPAR_WL, 0x20, 0x20),
            (XTAL_SI_ANAPAR_WL, 0x04, 0x04),
            (XTAL_SI_ANAPAR_WL, 0x08, 0x08),
            (XTAL_SI_ANAPAR_WL, 0, 0x10),
            (XTAL_SI_WL_RFC_S0, 0xeb, 0xff),
            (XTAL_SI_WL_RFC_S1, 0xeb, 0xff),
            (XTAL_SI_ANAPAR_WL, 0x01, 0x01),
            (XTAL_SI_ANAPAR_WL, 0x02, 0x02),
            (XTAL_SI_ANAPAR_WL, 0, 0x80),
            (XTAL_SI_XREF_RF1, 0, 0x40),
            (XTAL_SI_XREF_RF2, 0, 0x40),
            (XTAL_SI_PLL_1, 0x40, 0x60),
            (XTAL_SI_SRAM_CTRL, 0, XTAL_SI_SRAM_DIS),
        ] {
            self.xtal_si_write(offset, val, mask)?;
        }
        if self.cv != CHIP_CAV {
            self.set32(R_BE_PMC_DBG_CTRL2, B_BE_SYSON_DIS_PMCR_BE_WRMSK)?;
            self.set32(R_BE_SYS_ISO_CTRL, B_BE_ISO_EB2CORE)?;
            self.clr32(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_B)?;
            sleep(Duration::from_millis(1));
            self.clr32(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_S)?;
            self.clr32(R_BE_PMC_DBG_CTRL2, B_BE_SYSON_DIS_PMCR_BE_WRMSK)?;
        }
        self.set32(
            R_BE_DMAC_FUNC_EN,
            B_BE_MAC_FUNC_EN
                | B_BE_DMAC_FUNC_EN
                | B_BE_MPDU_PROC_EN
                | B_BE_WD_RLS_EN
                | B_BE_DLE_WDE_EN
                | B_BE_TXPKT_CTRL_EN
                | B_BE_STA_SCH_EN
                | B_BE_DLE_PLE_EN
                | B_BE_PKT_BUF_EN
                | B_BE_DMAC_TBL_EN
                | B_BE_PKT_IN_EN
                | B_BE_DLE_CPUIO_EN
                | B_BE_DISPATCHER_EN
                | B_BE_BBRPT_EN
                | B_BE_MAC_SEC_EN
                | B_BE_H_AXIDMA_EN
                | B_BE_DMAC_MLO_EN
                | B_BE_PLRLS_EN
                | B_BE_P_AXIDMA_EN
                | B_BE_DLE_DATACPUIO_EN,
        )?;
        self.set32(
            R_BE_CMAC_SHARE_FUNC_EN,
            B_BE_CMAC_SHARE_EN | B_BE_RESPBA_EN | B_BE_ADDRSRCH_EN | B_BE_BTCOEX_EN,
        )?;
        self.set32(
            R_BE_CMAC_FUNC_EN,
            B_BE_CMAC_EN
                | B_BE_CMAC_TXEN
                | B_BE_CMAC_RXEN
                | B_BE_SIGB_EN
                | B_BE_PHYINTF_EN
                | B_BE_CMAC_DMA_EN
                | B_BE_PTCLTOP_EN
                | B_BE_SCHEDULER_EN
                | B_BE_TMAC_EN
                | B_BE_RMAC_EN
                | B_BE_TXTIME_EN
                | B_BE_RESP_PKTCTL_EN,
        )?;
        self.set32(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB_IP_RSTN | B_BE_FEN_BBPLAT_RSTB)?;

        // rtw89_mac_power_switch의 켠 뒤: eFuse의 컷과 보안 부팅.
        let fv = self.efuse_read(EFUSE_FV_OFFSET & !3, 4)?;
        let ecv = fv[(EFUSE_FV_OFFSET & 3) as usize] >> 4;
        if ecv != 15 {
            self.cv = ecv;
        }
        self.secure = self.efuse_secure()?;
        Ok(())
    }

    /// 물리 eFuse를 4바이트씩 읽습니다 (rtw89_dump_physical_efuse_map_be, ddv).
    pub fn efuse_read(&self, addr: u32, size: u32) -> io::Result<Vec<u8>> {
        // rtw89_cnv_efuse_state_be(false)
        self.clr32(R_BE_WL_BT_PWR_CTRL, B_BE_BT_DISN_EN)?;
        let _ = self.poll(R_BE_IC_PWR_STATE, 5, "efuse state", |v| {
            get(v, B_BE_WHOLE_SYS_PWR_STE_MASK) == MAC_AX_SYS_ACT
        });
        let aphy_patch = self.cv != CHIP_CAV;
        self.set8(R_BE_PMC_DBG_CTRL2, B_BE_SYSON_DIS_PMCR_BE_WRMSK)?;
        if aphy_patch {
            self.set16(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_S)?;
            sleep(Duration::from_millis(1));
            self.set16(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_B)?;
            self.clr16(R_BE_SYS_ISO_CTRL, B_BE_ISO_EB2CORE)?;
        }
        self.set32(R_BE_EFUSE_CTRL_2_V1, B_BE_EF_BURST)?;
        let mut map = Vec::with_capacity(size as usize);
        let mut result = Ok(());
        for a in (addr..addr + size).step_by(4) {
            if let Err(e) = self
                .w32(R_BE_EFUSE_CTRL, put(0, B_BE_EF_ADDR_MASK, a) & !B_BE_EF_RDY)
                .and_then(|_| self.poll(R_BE_EFUSE_CTRL, 1000, "efuse", |v| v & B_BE_EF_RDY != 0))
                .and_then(|_| self.r32(R_BE_EFUSE_CTRL_1_V1))
                .map(|d| map.extend_from_slice(&d.to_le_bytes()))
            {
                result = Err(e);
                break;
            }
        }
        if aphy_patch {
            self.set16(R_BE_SYS_ISO_CTRL, B_BE_ISO_EB2CORE)?;
            self.clr16(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_B)?;
            sleep(Duration::from_millis(1));
            self.clr16(R_BE_SYS_ISO_CTRL, B_BE_PWC_EV2EF_S)?;
        }
        self.clr8(R_BE_PMC_DBG_CTRL2, B_BE_SYSON_DIS_PMCR_BE_WRMSK)?;
        self.clr32(R_BE_EFUSE_CTRL_2_V1, B_BE_EF_BURST)?;
        self.set32(R_BE_WL_BT_PWR_CTRL, B_BE_BT_DISN_EN)?;
        result.map(|_| map)
    }

    /// 보안 부팅 칩인지 (rtw89_efuse_read_fw_secure_be). 아직 키 선택은 옮기지 않았습니다.
    fn efuse_secure(&self) -> io::Result<Option<fw::Secure>> {
        let map = self.efuse_read(EFUSE_SEC_START, 4)?;
        let sb_cryp_sel = u16::from_le_bytes([map[2], map[3]]);
        if sb_cryp_sel == 0xffff {
            return Ok(None);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("secure boot chip (sb_cryp_sel 0x{sb_cryp_sel:04x}, efuse {map:02x?}): key selection is not ported yet"),
        ))
    }

    /// H2C 채널로 펌웨어 명령이나 펌웨어 조각 하나를 보냅니다 (rtw89_usb_tx_write_fwcmd).
    fn h2c_tx(&self, payload: &[u8], fwdl: bool) -> io::Result<()> {
        let mut pkt_size = payload.len();
        let mut buf = vec![0u8; H2C_DESC_SIZE];
        buf.extend_from_slice(payload);
        if (pkt_size + H2C_DESC_SIZE).is_multiple_of(512) {
            buf.extend_from_slice(&[0; MOD512_PADDING]);
            pkt_size += MOD512_PADDING;
        }
        let kind = if fwdl { RX_TYPE_FWDL } else { RX_TYPE_H2C };
        let dword0 = (pkt_size as u32 & 0x3fff) | (kind << 24);
        buf[..4].copy_from_slice(&dword0.to_le_bytes());
        self.usb.bulk_write(self.usb.bulk_out[BULKOUT_H2C], &buf)
    }

    /// H2C 명령 하나 (rtw89_h2c_pkt_set_hdr): 분류, 묶음, 기능, 받음/끝남 응답 요청.
    pub fn h2c(&self, cat: u32, class: u32, func: u32, rack: bool, dack: bool, payload: &[u8]) -> io::Result<()> {
        let len = payload.len();
        let seq = self.h2c_seq.fetch_add(1, Ordering::Relaxed);
        let hdr0 = cat | (class << 2) | (func << 8) | ((seq as u32) << 24);
        let hdr1 = ((len + H2C_HEADER_LEN) as u32 & 0x3fff) | ((rack as u32) << 14) | ((dack as u32) << 15);
        let mut pkt = Vec::with_capacity(H2C_HEADER_LEN + len);
        pkt.extend_from_slice(&hdr0.to_le_bytes());
        pkt.extend_from_slice(&hdr1.to_le_bytes());
        pkt.extend_from_slice(payload);
        self.h2c_tx(&pkt, false)
    }

    /// 펌웨어 내려받기 상태 (fwdl_get_status_be): 기다리는 일이 끝났으면 FWDL_WCPU_FW_INIT_RDY.
    fn fwdl_status(&self, done_bit: Option<u32>) -> io::Result<u8> {
        let v = self.r32(R_BE_WCPU_FW_CTRL)?;
        if let Some(bit) = done_bit
            && v & bit == 0
        {
            return Ok(FWDL_WCPU_FW_INIT_RDY);
        }
        const MAP: [u8; 10] = [0, 1, 6, 7, 2, 3, 3, 4, 5, 5];
        let st = get(v, B_BE_WCPU_FWDL_STATUS_MASK) as usize;
        Ok(MAP.get(st).copied().unwrap_or(st as u8))
    }

    /// 레지스터로 주고받는 펌웨어 명령 (rtw89_fw_msg_reg): 머리말 2바이트 + 내용 len바이트를 보내고
    /// 응답 네 워드를 받습니다. 응답 w0의 아래 7비트가 응답 종류입니다.
    pub fn h2c_reg(&mut self, id: u32, words: [u32; 4], content_len: u32) -> io::Result<[u32; 4]> {
        self.poll(R_BE_H2CREG_CTRL, 5, "H2C registers free", |v| v & 0xff == 0)?;
        let mut w = words;
        w[0] = put(w[0], RTW89_H2CREG_HDR_FUNC_MASK, id);
        w[0] = put(w[0], RTW89_H2CREG_HDR_LEN_MASK, (content_len + RTW89_H2CREG_HDR_LEN).div_ceil(4));
        for (r, v) in [R_BE_H2CREG_DATA0, R_BE_H2CREG_DATA1, R_BE_H2CREG_DATA2, R_BE_H2CREG_DATA3]
            .into_iter()
            .zip(w)
        {
            self.w32(r, v)?;
        }
        self.w8(R_BE_H2CREG_CTRL, B_AX_H2CREG_TRIGGER as u8)?;
        self.poll(R_BE_C2HREG_CTRL, 1000, "C2H registers", |v| v & 0xff != 0)?;
        let mut out = [0u32; 4];
        for (o, r) in out
            .iter_mut()
            .zip([R_BE_C2HREG_DATA0, R_BE_C2HREG_DATA1, R_BE_C2HREG_DATA2, R_BE_C2HREG_DATA3])
        {
            *o = self.r32(r)?;
        }
        self.w8(R_BE_C2HREG_CTRL, 0)?;
        Ok(out)
    }

    /// rtw89_fw_check_rdy
    fn fw_check_ready(&self, done_bit: Option<u32>, what: &str) -> io::Result<()> {
        let t = Instant::now();
        loop {
            let st = self.fwdl_status(done_bit)?;
            if st == FWDL_WCPU_FW_INIT_RDY {
                return Ok(());
            }
            if t.elapsed() > Duration::from_millis(400) {
                let why = match st {
                    2 => "checksum fail",
                    3 => "security fail",
                    4 => "cv not match",
                    _ => "unexpected status",
                };
                return Err(io::Error::other(format!(
                    "{what}: {why} {st} (WCPU_FW_CTRL 0x{:08x}, BOOT_DBG 0x{:08x}, PC 0x{:08x})",
                    self.r32(R_BE_WCPU_FW_CTRL)?,
                    self.r32(R_BE_BOOT_DBG)?,
                    self.r32(R_BE_WLCPU_PORT_PC)?
                )));
            }
        }
    }

    /// WLAN CPU 상태 (WCPU_FW_CTRL)와 지금 실행 위치 (PC): 펌웨어가 도는지 볼 때.
    pub fn cpu_state(&self) -> io::Result<(u32, u32)> {
        Ok((self.r32(R_BE_WCPU_FW_CTRL)?, self.r32(R_BE_WLCPU_PORT_PC)?))
    }

    /// 펌웨어 하나를 보냅니다 (rtw89_fw_download_suit).
    fn download_suit(&mut self, fw: &[u8], normal: bool, done_bit: u32, what: &str) -> io::Result<()> {
        let img = fw::parse(fw, self.secure.as_ref())?;
        if self.secure.is_some() {
            self.w32_mask(R_AX_WCPU_FW_CTRL, B_AX_IDMEM_SHARE_MODE_RECORD_MASK, img.idmem_share_mode as u32)?;
            self.set32(R_AX_WCPU_FW_CTRL, B_AX_IDMEM_SHARE_MODE_RECORD_VALID)?;
        }
        if normal {
            self.w32(R_BE_SECURE_BOOT_MALLOC_INFO, 0x2024_8000)?;
        }
        self.poll(R_BE_WCPU_FW_CTRL, 1000, "H2C path ready", |v| v & B_BE_H2C_PATH_RDY != 0)?;

        // 머리말 (H2C, 머리말 있음)
        let len = img.header.len();
        let mut pkt = Vec::with_capacity(H2C_HEADER_LEN + len);
        let hdr0 = H2C_CAT_MAC | (H2C_CL_MAC_FWDL << 2) | (H2C_FUNC_MAC_FWHDR_DL << 8) | ((self.h2c_seq.load(Ordering::Relaxed) as u32) << 24);
        let hdr1 = (len + H2C_HEADER_LEN) as u32 & 0x3fff;
        pkt.extend_from_slice(&hdr0.to_le_bytes());
        pkt.extend_from_slice(&hdr1.to_le_bytes());
        pkt.extend_from_slice(&img.header);
        self.h2c_tx(&pkt, false)?;
        self.poll(R_BE_WCPU_FW_CTRL, 1000, "FWDL path ready", |v| v & B_BE_DLFW_PATH_RDY != 0)?;
        self.w32(R_AX_HALT_H2C_CTRL, 0)?;
        self.w32(R_AX_HALT_C2H_CTRL, 0)?;

        // 구간들 (조각마다 머리말 없이)
        for s in &img.sections {
            let mut left = s.data;
            let single = s.data.len() <= img.part_size;
            while !left.is_empty() {
                let n = left.len().min(img.part_size);
                match s.key {
                    Some(key) if single => {
                        let mut chunk = left[..n].to_vec();
                        let at = n - key.len();
                        chunk[at..].copy_from_slice(key);
                        self.h2c_tx(&chunk, true)?;
                    }
                    _ => self.h2c_tx(&left[..n], true)?,
                }
                left = &left[n..];
            }
        }
        self.fw_check_ready(Some(done_bit), what)
    }

    /// 펌웨어를 내려받고 돌릴 때까지 (rtw89_mac_partial_init(include_bb) + rtw89_fw_download).
    /// 먼저 [power_on]이 필요합니다. 펌웨어 버전을 돌려줍니다.
    pub fn download_firmware(&mut self, file: &[u8]) -> io::Result<[u8; 4]> {
        let normal = fw::suit(file, fw::TYPE_NORMAL, self.cv)?;
        let bbmcu = fw::bbmcu(file, self.cv)?;
        let version = fw::parse(normal, self.secure.as_ref())?.version;

        // rtw89_mac_ctrl_hci_dma_trx(true)
        self.set32(R_BE_HCI_FUNC_EN, B_AX_HCI_TXDMA_EN | B_AX_HCI_RXDMA_EN)?;
        // rtw8922a_bb_preinit(PHY 0)
        self.w32_mask(R_BE_DMAC_SYS_CR32B, B_BE_DMAC_BB_PHY0_MASK, 0x7ff9)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB_IP_RSTN, 0)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BBPLAT_RSTB, 0)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB_IP_RSTN, 1)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_BOOT_RDY0, 0)?;
        self.w32_mask(R_BE_MEM_PWR_CTRL, B_BE_MEM_BBMCU0_DS_V1, 0)?;
        for (a, d) in BB_MCU0_INIT {
            self.w32(a + BBMCU_ADDR_OFFSET, d)?;
        }
        // rtw8922a_bb_preinit(PHY 1): rtw89는 BE 칩을 dbcc_en으로 켜서 BB1도 깨웁니다. 깨우지 않으면
        // 경로 B로는 아무것도 받지 못합니다. BB MCU1의 CR은 0x20000 위에 있습니다 (rtw89_bbmcu_write32).
        self.w32_mask(R_BE_DMAC_SYS_CR32B, B_BE_DMAC_BB_PHY1_MASK, 0x7ff9)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB1_IP_RSTN, 0)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB1PLAT_RSTB, 0)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB1_IP_RSTN, 1)?;
        self.w32_mask(R_BE_FEN_RST_ENABLE, B_BE_BOOT_RDY1, 1)?;
        self.w32_mask(R_BE_MEM_PWR_CTRL, B_BE_MEM_BBMCU0_DS_V1, 0)?;
        sleep(Duration::from_micros(1));
        for (a, d) in BB_MCU0_INIT {
            self.w32(a + 0x20000 + BBMCU_ADDR_OFFSET, d)?;
        }
        // rtw89_mac_dmac_pre_init: HCI, DMA 방식, 내려받기용 메모리 할당, H2C 흐름 제어.
        self.set32(R_BE_HCI_FUNC_EN, B_BE_HCI_TXDMA_EN | B_BE_HCI_RXDMA_EN)?;
        let mut v = put(self.r32(R_BE_HAXI_INIT_CFG1)?, B_BE_DMA_MODE_MASK, S_BE_DMA_MOD_USB);
        v = (v & !B_BE_STOP_AXI_MST) | B_BE_TXDMA_EN | B_BE_RXDMA_EN;
        self.w32(R_BE_HAXI_INIT_CFG1, v)?;
        self.clr32(R_BE_HAXI_DMA_STOP1, B_BE_TX_STOP1_MASK)?;
        self.set32(R_BE_DMAC_TABLE_CTRL, B_BE_DMAC_ADDR_MODE)?;
        self.dle_init_dlfw()?;
        // rtw89_mac_hfc_init(reset, en=false, h2c_en=true): H2C 채널만 (USB 3 SCC의 h2c_prec 32).
        self.clr32(R_BE_HCI_FC_CTRL, B_BE_HCI_FC_EN | B_BE_HCI_FC_CH12_EN)?;
        self.w32(R_BE_CH_PAGE_CTRL, put(0, B_BE_PREC_PAGE_CH12_V1_MASK, 32))?;
        let fc = self.r32(R_BE_HCI_FC_CTRL)?;
        self.w32(R_BE_HCI_FC_CTRL, (fc & !B_BE_HCI_FC_EN) | B_BE_HCI_FC_CH12_EN)?;
        // rtw89_mac_fwdl_preconfig_be
        self.clr32(R_BE_FW_AUTO_CAL_DELAY, B_BE_WCPU_FW_DELAY_COUNT_VALID)?;
        self.w32_mask(R_BE_FW_AUTO_CAL_DELAY, B_BE_WCPU_FW_DELAY_COUNT_MASK, 0)?;

        let mut last = Ok(());
        for attempt in 0..5 {
            last = self.try_download(normal, bbmcu);
            if last.is_ok() {
                tracing::debug!(
                    "firmware {}.{}.{}.{} in {} attempt(s)",
                    version[0],
                    version[1],
                    version[2],
                    version[3],
                    attempt + 1
                );
                return Ok(version);
            }
            tracing::warn!("firmware download attempt {} failed: {:?}", attempt + 1, last.as_ref().err());
        }
        last.map(|_| version)
    }

    /// __rtw89_fw_download(NORMAL, include_bb)
    fn try_download(&mut self, normal: &[u8], bbmcu: &[u8]) -> io::Result<()> {
        // rtw89_mac_disable_cpu_be
        self.clr32(R_BE_PLATFORM_ENABLE, B_BE_WCPU_EN)?;
        self.set32(R_BE_PLATFORM_ENABLE, B_BE_HOLD_AFTER_RESET)?;
        self.set32(R_BE_PLATFORM_ENABLE, B_BE_WCPU_EN)?;
        let v = self.r32(R_BE_WCPU_FW_CTRL)? & B_BE_RUN_ENV_MASK;
        self.w32(R_BE_WCPU_FW_CTRL, v)?;
        self.set32(R_BE_DCPU_PLATFORM_ENABLE, B_BE_DCPU_PLATFORM_EN)?;
        self.w32(R_BE_UDM0, 0)?;
        self.w32(R_BE_HALT_C2H, 0)?;
        self.w32(R_BE_UDM2, 0)?;
        // rtw89_mac_fwdl_enable_wcpu_be(boot_reason 0, dlfw, include_bb): set_cpu_en + wcpu_on
        self.set32(R_BE_WCPU_FW_CTRL, B_BE_WLANCPU_FWDL_EN | B_BE_BBMCU0_FWDL_EN)?;
        for r in [
            R_BE_UDM1,
            R_BE_UDM2,
            R_BE_BOOT_DBG,
            R_BE_HALT_H2C,
            R_BE_HALT_C2H,
            R_BE_HALT_H2C_CTRL,
            R_BE_HALT_C2H_CTRL,
        ] {
            self.w32(r, 0)?;
        }
        self.w32(R_BE_HISR0, B_BE_HALT_C2H_INT)?;
        self.set32(R_BE_SYS_CLK_CTRL, B_BE_CPU_CLK_EN)?;
        self.clr32(R_BE_SYS_CFG5, B_BE_WDT_WAKE_PCIE_EN | B_BE_WDT_WAKE_USB_EN)?;
        self.clr32(R_BE_WCPU_FW_CTRL, B_BE_WDT_PLT_RST_EN | B_BE_WCPU_ROM_CUT_GET)?;
        self.w32(R_BE_SECURE_BOOT_MALLOC_INFO, 0)?;
        self.clr32(R_BE_GPIO_MUXCFG, B_BE_BOOT_MODE)?;
        self.w32_mask(R_BE_WCPU_FW_CTRL, B_BE_WCPU_ROM_CUT_VAL_MASK, self.cv as u32 + 1)?;
        self.w16_mask(R_BE_BOOT_REASON, B_BE_BOOT_REASON_MASK, 0)?;
        self.clr32(R_BE_PLATFORM_ENABLE, B_BE_WCPU_EN)?;
        self.clr32(R_BE_PLATFORM_ENABLE, B_BE_HOLD_AFTER_RESET)?;
        self.set32(R_BE_PLATFORM_ENABLE, B_BE_WCPU_EN)?;

        self.download_suit(normal, true, B_BE_WLANCPU_FWDL_EN, "WLAN CPU firmware")?;
        self.download_suit(bbmcu, false, B_BE_BBMCU0_FWDL_EN, "BB MCU firmware")?;
        self.h2c_seq.store(0, Ordering::Relaxed);
        sleep(Duration::from_millis(5));
        self.fw_check_ready(None, "firmware start")
    }

    /// 내려받기 동안의 WDE/PLE 메모리 할당 (rtw89_mac_dle_init(DLFW, SCC); 8922A USB 3의 표).
    fn dle_init_dlfw(&self) -> io::Result<()> {
        self.clr32(R_BE_DMAC_FUNC_EN, B_BE_DLE_WDE_EN | B_BE_DLE_PLE_EN)?;
        self.set32(R_BE_DMAC_CLK_EN, B_BE_DLE_WDE_CLK_EN | B_BE_DLE_PLE_CLK_EN)?;
        // wde_size3_v1 = {64바이트 쪽, 연결 0, 미연결 3328, 시작 0}
        let mut v = self.r32(R_BE_WDE_PKTBUF_CFG)?;
        v = put(v, B_BE_WDE_PAGE_SEL_MASK, S_AX_WDE_PAGE_SEL_64);
        v = put(v, B_BE_WDE_START_BOUND_MASK, 0);
        v = put(v, B_BE_WDE_FREE_PAGE_NUM_MASK, 0);
        self.w32(R_BE_WDE_PKTBUF_CFG, v)?;
        // ple_size3_v1 = {128바이트 쪽, 연결 2928, 미연결 0, 시작 212992}
        let mut v = self.r32(R_BE_PLE_PKTBUF_CFG)?;
        v = put(v, B_BE_PLE_PAGE_SEL_MASK, S_AX_PLE_PAGE_SEL_128);
        v = put(v, B_BE_PLE_START_BOUND_MASK, 212_992 / DLE_BOUND_UNIT);
        v = put(v, B_BE_PLE_FREE_PAGE_NUM_MASK, 2928);
        self.w32(R_BE_PLE_PKTBUF_CFG, v)?;
        // wde_qt4(최소=최대=0), WCPU 최소는 SCC의 wde_qt4_v1.wcpu = 6.
        let wde: [(u32, u32); 5] = [(0, 0), (6, 6), (0, 0), (0, 0), (0, 0)];
        for (i, (min, max)) in wde.into_iter().enumerate() {
            let r = R_BE_WDE_QTA0_CFG + 4 * i as u32;
            self.w32(r, put(0, B_BE_WDE_Q0_MIN_SIZE_MASK, min) | put(0, B_BE_WDE_Q0_MAX_SIZE_MASK, max))?;
        }
        // ple_qt9 = {cma0_tx, cma1_tx, c2h 32, h2c 256, wcpu, mpdu_proc, cma0_dma, cma1_dma, bb_rpt,
        // wd_rel, cpu_io 1, tx_rpt, h2d}
        let ple: [u32; 13] = [0, 0, 32, 256, 0, 0, 0, 0, 0, 0, 1, 0, 0];
        for (i, q) in ple.into_iter().enumerate() {
            let r = R_BE_PLE_QTA0_CFG + 4 * i as u32;
            self.w32(r, put(0, B_BE_PLE_Q0_MIN_SIZE_MASK, q) | put(0, B_BE_PLE_Q0_MAX_SIZE_MASK, q))?;
        }
        self.set32(R_BE_DMAC_FUNC_EN, B_BE_DLE_WDE_EN | B_BE_DLE_PLE_EN)?;
        self.poll(R_AX_WDE_INI_STATUS, 50, "WDE ready", |v| v & WDE_MGN_INI_RDY == WDE_MGN_INI_RDY)?;
        self.poll(R_AX_PLE_INI_STATUS, 50, "PLE ready", |v| v & PLE_MGN_INI_RDY == PLE_MGN_INI_RDY)?;
        Ok(())
    }
}

impl Rtl8922a {
    /// 전원 끄기 (rtw8922a_pwr_off_func의 USB 경로): 아날로그와 RF 전원, BB 리셋, 그리고 MAC.
    pub(crate) fn pwr_off(&self) -> io::Result<()> {
        for (offset, val, mask) in [
            (XTAL_SI_ANAPAR_WL, 0x10, 0x10),
            (XTAL_SI_ANAPAR_WL, 0, 0x08),
            (XTAL_SI_ANAPAR_WL, 0, 0x04),
            (XTAL_SI_WL_RFC_S0, 0xc6, 0xff),
            (XTAL_SI_WL_RFC_S1, 0xc6, 0xff),
            (XTAL_SI_ANAPAR_WL, 0x80, 0x80),
            (XTAL_SI_ANAPAR_WL, 0, 0x02),
            (XTAL_SI_ANAPAR_WL, 0, 0x01),
            (XTAL_SI_PLL, 0x02, 0xff),
            (XTAL_SI_PLL, 0x00, 0xff),
        ] {
            self.xtal_si_write(offset, val, mask)?;
        }
        self.set32(R_BE_FEN_RST_ENABLE, B_BE_R_SYM_ISO_ADDA_P02PP | B_BE_R_SYM_ISO_ADDA_P12PP)?;
        self.clr8(R_BE_ANAPAR_POW_MAC, B_BE_POW_PC_LDO_PORT0 | B_BE_POW_PC_LDO_PORT1)?;
        self.set32(R_BE_SYS_PW_CTRL, B_BE_EN_WLON)?;
        self.clr8(R_BE_FEN_RST_ENABLE, B_BE_FEN_BB_IP_RSTN | B_BE_FEN_BBPLAT_RSTB)?;
        self.clr32(R_BE_SYS_ADIE_PAD_PWR_CTRL, B_BE_SYM_PADPDN_WL_RFC0_1P3)?;
        self.xtal_si_write(XTAL_SI_ANAPAR_WL, 0, 0x20)?;
        self.clr32(R_BE_SYS_ADIE_PAD_PWR_CTRL, B_BE_SYM_PADPDN_WL_RFC1_1P3)?;
        self.xtal_si_write(XTAL_SI_ANAPAR_WL, 0, 0x40)?;
        self.clr32(R_BE_HCI_OPT_CTRL, B_BE_HCI_WLAN_IO_EN)?;
        self.poll(R_BE_HCI_OPT_CTRL, 3000, "WLAN IO off", |v| v & B_BE_HCI_WLAN_IO_ST == 0)?;
        self.set32(R_BE_SYS_PW_CTRL, B_BE_APFM_OFFMAC)?;
        self.poll(R_BE_SYS_PW_CTRL, 3000, "MAC off", |v| v & B_BE_APFM_OFFMAC == 0)?;
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_SOP_EASWR)?;
        self.clr32(R_BE_SYS_PW_CTRL, B_BE_XTAL_OFF_A_DIE)?;
        let v = self.r32(R_BE_SYS_PW_CTRL)?;
        self.w32(R_BE_SYS_PW_CTRL, (v | B_BE_AFSM_WLSUS_EN) & !B_BE_AFSM_PCIE_SUS_EN)
    }
}
