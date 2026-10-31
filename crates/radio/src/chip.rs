//! 레지스터 접근과, rtw88 코드가 기대는 작은 도우미들.
use std::io;
use std::time::{Duration, Instant};

use crate::generated::regs::*;
use crate::usb::Usb;

/// 전원 시퀀스 한 단계 (rtw88의 rtw_pwr_seq_cmd, USB 항목만).
#[derive(Debug, Clone, Copy)]
pub struct PwrCmd {
    pub offset: u16,
    pub cut_mask: u8,
    pub base: u8,
    pub cmd: u8,
    pub mask: u8,
    pub value: u8,
}

const PWR_CMD_WRITE: u8 = 1;
const PWR_CMD_POLLING: u8 = 2;
const PWR_CMD_DELAY: u8 = 3;

pub const RF_BASE: [u32; 2] = [0x2800, 0x2c00];
pub const RF_SIPI: [u32; 2] = [0xc90, 0xe90];
pub const RFREG_MASK: u32 = 0xfffff;

pub fn delay_us(us: u64) {
    let end = Instant::now() + Duration::from_micros(us);
    if us >= 2000 {
        std::thread::sleep(Duration::from_micros(us - 1000));
    }
    while Instant::now() < end {
        std::hint::spin_loop();
    }
}

pub fn delay_ms(ms: u64) {
    delay_us(ms * 1000)
}

fn shift_of(mask: u32) -> u32 {
    mask.trailing_zeros()
}

pub fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// USB 위 88xx 계열 칩의 레지스터 인터페이스.
pub struct Regs {
    pub usb: Usb,
}

impl Regs {
    pub fn r8(&self, a: u32) -> io::Result<u8> {
        Ok(self.usb.read(a as u16, 1)? as u8)
    }
    pub fn r16(&self, a: u32) -> io::Result<u16> {
        Ok(self.usb.read(a as u16, 2)? as u16)
    }
    pub fn r32(&self, a: u32) -> io::Result<u32> {
        self.usb.read(a as u16, 4)
    }
    pub fn w8(&self, a: u32, v: u8) -> io::Result<()> {
        self.usb.write(a as u16, v as u32, 1)
    }
    pub fn w16(&self, a: u32, v: u16) -> io::Result<()> {
        self.usb.write(a as u16, v as u32, 2)
    }
    pub fn w32(&self, a: u32, v: u32) -> io::Result<()> {
        self.usb.write(a as u16, v, 4)
    }
    pub fn set8(&self, a: u32, bits: u32) -> io::Result<()> {
        let v = self.r8(a)?;
        self.w8(a, v | bits as u8)
    }
    pub fn clr8(&self, a: u32, bits: u32) -> io::Result<()> {
        let v = self.r8(a)?;
        self.w8(a, v & !(bits as u8))
    }
    pub fn set16(&self, a: u32, bits: u32) -> io::Result<()> {
        let v = self.r16(a)?;
        self.w16(a, v | bits as u16)
    }
    pub fn set32(&self, a: u32, bits: u32) -> io::Result<()> {
        let v = self.r32(a)?;
        self.w32(a, v | bits)
    }
    pub fn clr32(&self, a: u32, bits: u32) -> io::Result<()> {
        let v = self.r32(a)?;
        self.w32(a, v & !bits)
    }
    /// rtw_read32_mask: `mask` 아래의 필드를 아래로 밀어서.
    pub fn r32m(&self, a: u32, mask: u32) -> io::Result<u32> {
        Ok((self.r32(a)? & mask) >> shift_of(mask))
    }
    /// rtw_write32_mask: `data`를 `mask` 아래의 필드에.
    pub fn w32m(&self, a: u32, mask: u32, data: u32) -> io::Result<()> {
        if mask == u32::MAX {
            return self.w32(a, data);
        }
        let v = self.r32(a)?;
        self.w32(a, (v & !mask) | ((data << shift_of(mask)) & mask))
    }
    pub fn w8m(&self, a: u32, mask: u32, data: u8) -> io::Result<()> {
        let v = self.r8(a)? as u32;
        self.w8(a, ((v & !mask) | (((data as u32) << shift_of(mask)) & mask)) as u8)
    }
    /// rtw_write32s_mask: 0xC00-0xCFF와 0xE00-0xEFF(경로 A와 B)는 배치가 같습니다.
    pub fn w32sm(&self, a: u32, mask: u32, data: u32) -> io::Result<()> {
        self.w32m(a, mask, data)?;
        self.w32m(a + 0x200, mask, data)
    }

    /// check_hw_ready: `mask` 아래의 필드가 `target`이 될 때까지 (최대 ~10 ms) 확인합니다.
    pub fn ready(&self, a: u32, mask: u32, target: u32) -> io::Result<bool> {
        let end = Instant::now() + Duration::from_millis(50);
        loop {
            if self.r32m(a, mask)? == target {
                return Ok(true);
            }
            if Instant::now() > end {
                return Ok(false);
            }
            delay_us(10);
        }
    }

    pub fn rf_read(&self, path: usize, addr: u32, mask: u32) -> io::Result<u32> {
        self.r32m(RF_BASE[path] + ((addr & 0xff) << 2), mask & RFREG_MASK)
    }

    /// rtw_phy_write_rf_reg_sipi.
    pub fn rf_write(&self, path: usize, addr: u32, mask: u32, data: u32) -> io::Result<()> {
        let addr = addr & 0xff;
        let mask = mask & RFREG_MASK;
        let data = if mask != RFREG_MASK {
            let old = self.rf_read(path, addr, RFREG_MASK)?;
            (old & !mask) | (data << shift_of(mask))
        } else {
            data
        };
        self.w32(RF_SIPI[path], ((addr << 20) | (data & 0xfffff)) & 0x0fff_ffff)?;
        delay_us(13);
        Ok(())
    }

    /// 하위 시퀀스 하나에 대한 rtw_pwr_seq_parser.
    pub fn pwr_seq(&self, seq: &[PwrCmd], cut: u8) -> io::Result<()> {
        let cut_mask = 1u8 << (cut + 1);
        for c in seq {
            if c.cut_mask & cut_mask == 0 {
                continue;
            }
            let offset = c.offset as u32;
            match c.cmd {
                PWR_CMD_WRITE => {
                    let v = self.r8(offset)?;
                    self.w8(offset, (v & !c.mask) | (c.value & c.mask))?;
                }
                PWR_CMD_POLLING => {
                    let target = c.value & c.mask;
                    let end = Instant::now() + Duration::from_millis(100);
                    while self.r8(offset)? & c.mask != target {
                        if Instant::now() > end {
                            return Err(err(format!(
                                "power sequence: 0x{offset:04x} & 0x{:02x} never became 0x{target:02x}",
                                c.mask
                            )));
                        }
                        delay_us(50);
                    }
                }
                PWR_CMD_DELAY => {
                    if c.value == 0 {
                        delay_us(c.offset as u64)
                    } else {
                        delay_ms(c.offset as u64)
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub(crate) const fn _unused() -> u32 {
    REG_CR
}
