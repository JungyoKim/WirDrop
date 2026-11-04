//! wirdrop-radio: Realtek USB Wi-Fi 어댑터를 사용자 모드에서 WinUSB로 구동합니다. RTL8822BU /
//! RTL8812BU(예: TP-Link Archer T3U Nano)와 RTL8912AU(예: ipTIME BE6500UA). 어댑터의 하드웨어 ACK와
//! Block Ack 응답기가 우리 주소에 대해 일하는 상태로 AWDL 프레임을 보내고 받습니다.
//!
//! 올리기(전원 시퀀스, 펌웨어 다운로드, MAC/BB/RF 설정, 채널 전환, 서술자)는 Linux rtw88과 rtw89에서
//! BSD-3-Clause 라이선스로 옮겼습니다 (NOTICE.md 참고). 펌웨어는 linux-firmware의 Realtek
//! rtw8822b_fw.bin과 rtw8922a_fw-4.bin이고, 수정 없이 재배포할 수 있습니다
//! (firmware/LICENCE.rtlwifi_firmware.txt).
#![cfg(windows)]

pub mod chip;
pub mod dm;
// rtw88에서 생성한 표: 손으로 고치거나 rustfmt로 정렬하지 않습니다 (tools/gen_rtw88.py가 다시 만듭니다).
#[rustfmt::skip]
mod generated;
pub mod radio;
pub mod rtl8822b;
pub mod rtl8922a;
pub mod usb;

pub use radio::{Config, PhyStatus, Radio, RxFrame, TxFrame, TxRate};
pub use usb::{Usb, UsbId};
