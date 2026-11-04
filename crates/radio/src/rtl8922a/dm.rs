//! 동적 패킷 감지 문턱 (rtw89 phy.c의 DIG 가운데 RTL8922A가 쓰는 부분).
//!
//! 이 칩은 이득 코드를 하드웨어 AGC가 정하므로 (rtw89의 support_igi가 거짓) 드라이버가 움직이는 것은
//! OFDM 패킷 감지의 아래 문턱뿐입니다: 가장 약한 상대보다 한참 약한 신호는 패킷으로 잡지 않아
//! 먼 이웃의 프레임에 수신기가 붙잡히지 않게 합니다. AWDL은 가까운 상대만 있을 때도 멀리서 새로
//! 나타난 기기를 들어야 하므로 문턱은 [MAX_FLOOR_DBM]을 넘지 않습니다 (rtw89는 공유기 하나만 봅니다).
//!
//! rtw89는 접속하면 같은 레지스터의 공간 재사용(spatial reuse) 검출도 켜지만, 여기서는 늘 끕니다.
//! 그것을 켜면 칩이 AWDL 상대(우리 BSS에 속하지 않은 iPhone)의 ACK을 다른 BSS의 신호로 보고 패킷으로
//! 잡지 않아, 상대는 프레임마다 ACK을 보내는데 칩은 한도까지 다시 보냅니다 (공유기에 접속한 rtw89와
//! 공식 드라이버는 상대가 같은 BSS라 이 일이 없습니다).
use std::io;

use super::Rtl8922a;
use super::generated::regs::*;

/// rtw89 phy.h의 상수.
const PD_TH_MAX_RSSI: u8 = 70;
const PD_TH_MIN_RSSI: u8 = 8;
/// rtw89_phy_dig_para_reset의 pd_low_th_offset.
const PD_LOW_TH_OFFSET: u8 = 16;
/// 연결된 상대가 없을 때의 세기 (rtw89의 rssi_nolink, dBm + 110).
const RSSI_NOLINK: u8 = 22;
/// 문턱의 상한: 이보다 센 신호는 언제나 패킷으로 잡습니다 (AWDL이 MCS0으로 겨우 닿는 세기).
const MAX_FLOOR_DBM: i16 = -80;

/// watchdog 한 번의 결과.
#[derive(Debug, Clone, Copy)]
pub struct DigReport {
    /// 패킷 감지 아래 문턱 레지스터 값 (0 = 가장 민감).
    pub pd: u8,
    /// 그 문턱이 거르는 세기 (dBm 근사, 0이면 거르지 않음).
    pub floor_dbm: i16,
}

/// 가장 약한 상대의 세기(dBm + 110)와 채널 폭으로 정한 문턱 값과 그 세기 (rtw89_phy_dig_dyn_pd_th).
fn pd_lower_bound(rssi: u8, bw_mhz: u16) -> (u8, i16) {
    let bw_cmp = match bw_mhz {
        160 => 9,
        80 => 6,
        40 => 3,
        _ => 0,
    };
    let under = PD_LOW_TH_OFFSET + bw_cmp;
    let cap = (under as i16 + 110 + MAX_FLOOR_DBM) as u8;
    let cca_th = rssi.min(cap).clamp(PD_TH_MIN_RSSI + under, PD_TH_MAX_RSSI + under);
    let pd = (cca_th - under - PD_TH_MIN_RSSI) >> 1;
    let floor = if pd == 0 { 0 } else { cca_th as i16 - under as i16 - 110 };
    (pd, floor)
}

impl Rtl8922a {
    /// 2초마다 부릅니다. weakest는 최근에 들린 가장 약한 상대의 세기 (rtw88 척도: dBm + 100).
    pub fn watchdog(&self, bw_mhz: u16, weakest: Option<u8>) -> io::Result<DigReport> {
        // rtw89의 igi_rssi는 dBm + 110.
        let rssi = weakest.map(|r| r.saturating_add(10)).unwrap_or(RSSI_NOLINK);
        let (pd, floor_dbm) = pd_lower_bound(rssi, bw_mhz);
        self.pw32_mask(R_SEG0R_PD_V2, B_SEG0R_PD_LOWER_BOUND_MSK, pd as u32)?;
        self.pw32_mask(R_SEG0R_PD_V2, B_SEG0R_PD_SPATIAL_REUSE_EN_MSK_V1, 0)?;
        Ok(DigReport { pd, floor_dbm })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_follows_weakest_peer() {
        // -70 dBm 상대, 80 MHz: rtw89와 같은 5, 상대보다 22 dB 약한 것부터 거름.
        assert_eq!(pd_lower_bound(40, 80), (5, -92));
        // -50 dBm 상대: rtw89라면 -72 dBm까지 올리지만 -80 dBm에서 멈춤.
        assert_eq!(pd_lower_bound(60, 80), (11, -80));
        // 상대가 없으면 거르지 않음.
        assert_eq!(pd_lower_bound(RSSI_NOLINK, 160).0, 0);
    }
}
