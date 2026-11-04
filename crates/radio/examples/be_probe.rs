//! 임시 (로컬 실험): RTL8912AU(ipTIME BE6500UA)를 WinUSB로 열어 칩 정보와 USB 속도를 보고,
//! --usb3면 rtw89의 rtw89_usb_switch_mode_be처럼 USB 3으로 다시 연결하게 합니다.
//!
//!     be_probe [--usb3]
use std::time::{Duration, Instant};

use wirdrop_radio::{Usb, UsbId};

const R_AX_SYS_CFG1: u32 = 0x00f0;
const R_BE_SYS_CHIPINFO: u32 = 0x00fc;
const R_BE_PAD_CTRL2: u32 = 0x00c4;
const R_BE_SYS_CFG5: u32 = 0x0170;

const B_BE_USB23_SW_MODE: u32 = 1 << 31;
const B_BE_USB3_GEN_MODE: u32 = 1 << 30;
const B_BE_USB3_LANE_MODE: u32 = 1 << 29;
const B_BE_USB_AUTO_INSTALL_MASK: u32 = 1 << 28;
const B_BE_FORCE_CLK_U2: u32 = 1 << 25;
const B_BE_FORCE_U2_CK: u32 = 1 << 24;
const B_BE_FORCE_U3_CK: u32 = 1 << 23;
const B_BE_USB2_FORCE: u32 = 1 << 22;
const B_BE_USB3_FORCE: u32 = 1 << 21;
const B_BE_NO_PDN_CHIPOFF_V1: u32 = 1 << 17;
const B_BE_RSM_EN_V1: u32 = 1 << 16;
const B_BE_MATCH_CNT: u32 = 0xff << 8;
const USB_SWITCH_DELAY: u32 = 0xf;

fn open() -> std::io::Result<Usb> {
    let usb = Usb::open(UsbId::RTL8912AU)?;
    println!(
        "opened: bulk in {:02x?}, bulk out {:02x?}, max packet {} ({})",
        usb.bulk_ins,
        usb.bulk_out,
        usb.max_packet,
        if usb.max_packet >= 1024 { "USB 3" } else { "USB 2" }
    );
    Ok(usb)
}

fn main() -> std::io::Result<()> {
    let usb3 = std::env::args().any(|a| a == "--usb3");
    let usb = open()?;
    for (name, addr) in [
        ("SYS_CFG1", R_AX_SYS_CFG1),
        ("SYS_CHIPINFO", R_BE_SYS_CHIPINFO),
        ("PAD_CTRL2", R_BE_PAD_CTRL2),
        ("SYS_CFG5", R_BE_SYS_CFG5),
    ] {
        println!("{name:>12} 0x{addr:04x} = 0x{:08x}", usb.read_wide(addr, 4)?);
    }
    if !usb3 || usb.max_packet >= 1024 {
        return Ok(());
    }
    let mut pad = usb.read_wide(R_BE_PAD_CTRL2, 4)?;
    if (pad & B_BE_MATCH_CNT) >> 8 == USB_SWITCH_DELAY {
        println!("already tried to switch: this port is USB 2");
        return Ok(());
    }
    pad = (pad & !B_BE_MATCH_CNT) | (USB_SWITCH_DELAY << 8);
    pad |= B_BE_RSM_EN_V1 | B_BE_NO_PDN_CHIPOFF_V1 | B_BE_USB_AUTO_INSTALL_MASK | B_BE_USB23_SW_MODE;
    pad &= !(B_BE_USB3_FORCE | B_BE_USB2_FORCE | B_BE_FORCE_U3_CK | B_BE_FORCE_U2_CK | B_BE_FORCE_CLK_U2 | B_BE_USB3_GEN_MODE | B_BE_USB3_LANE_MODE);
    println!("switching: PAD_CTRL2 <- 0x{pad:08x}");
    // 장치가 곧바로 끊기므로 응답이 실패해도 괜찮습니다.
    let _ = usb.write_wide(R_BE_PAD_CTRL2, pad, 4);
    drop(usb);
    let t = Instant::now();
    std::thread::sleep(Duration::from_millis(1500));
    while t.elapsed() < Duration::from_secs(15) {
        if let Ok(usb) = open() {
            println!("back after {} ms", t.elapsed().as_millis());
            println!("   PAD_CTRL2 = 0x{:08x}", usb.read_wide(R_BE_PAD_CTRL2, 4)?);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    println!("the adapter did not come back within 15 s");
    Ok(())
}
