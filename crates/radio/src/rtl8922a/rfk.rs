//! RF 보정 (RFK). 8922A는 보정을 펌웨어가 하고, 끝나면 C2H로 결과를 알립니다
//! (rtw89_phy_rfk_*_and_wait, rtw89_fw_h2c_rf_*, rtw8922a_rfk_init_late, rtw8922a_rfk_channel).
//! 펌웨어 0.35.80의 형식: 사전 알림 v2와 MCC v0, IQK v1, RXDCK v1.
use std::io;
use std::time::{Duration, Instant};

use super::chan::{Chan, RFK_TABLES};
use super::generated::regs::*;
use super::{Rtl8922a, rx};
use crate::usb::Reader;

const H2C_FUNC_RFK_IQK: u32 = 0x1;
const H2C_FUNC_RFK_DPK: u32 = 0x3;
const H2C_FUNC_RFK_TXGAPK: u32 = 0x4;
const H2C_FUNC_RFK_DACK: u32 = 0x5;
const H2C_FUNC_RFK_RXDCK: u32 = 0x6;
const H2C_FUNC_RFK_PRE_NOTIFY: u32 = 0x8;
/// C2H: 분류 OUTSRC, 묶음 RFK_REPORT, 기능 STATE (내용: 상태, 판).
const C2H_CAT_OUTSRC: u8 = 2;
const C2H_CLASS_RFK_REPORT: u8 = 9;
const C2H_FUNC_RFK_STATE: u8 = 0;
const RFK_STATE_OK: u8 = 1;
/// MLO_2_PLUS_0_1RF
const MLO_2_PLUS_0_1RF: u32 = 0x1002;
const RF_AB: u8 = 0x3;
/// USB는 H2C/C2H 왕복이 길어 rtw89도 기다리는 시간을 네 배로 잡습니다.
const USB_WAIT_SCALE: u64 = 4;

/// 보정 하나의 결과.
#[derive(Debug, Clone)]
pub struct RfkResult {
    pub name: &'static str,
    /// 펌웨어가 알린 상태 (1 = 성공), 답이 없으면 None.
    pub state: Option<u8>,
    pub ms: u128,
}

fn le(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

impl Rtl8922a {
    /// 처음 한 번의 보정과 이 채널의 보정을 차례로 합니다 (rtw8922a_rfk_init_late, rtw8922a_rfk_channel).
    /// 송신 전력 보정(TSSI)은 아직 송신이 없어 뺐습니다. reader로 C2H를 받습니다.
    pub fn calibrate(&self, reader: &mut Reader, c: &Chan, first: bool) -> io::Result<Vec<RfkResult>> {
        let mut out = Vec::new();
        if first {
            out.push(self.pre_ntfy(reader, c)?);
            out.push(self.rfk(reader, "DACK", H2C_FUNC_RFK_DACK, &[3, 0, 0], 58)?);
            out.push(self.rfk(reader, "RXDCK", H2C_FUNC_RFK_RXDCK, &self.rxdck_args(c, false), 128)?);
        }
        self.wait_rx_mode()?;
        out.push(self.pre_ntfy(reader, c)?);
        let (band, bw, ch, cv) = (c.band as u8, c.bw as u8, c.center, self.cv);
        out.push(self.rfk(reader, "TXGAPK", H2C_FUNC_RFK_TXGAPK, &[8, 2, 0, RF_AB, band, bw, ch, cv], 54)?);
        out.push(self.rfk(reader, "IQK", H2C_FUNC_RFK_IQK, &[8, 0, 0, RF_AB, band, bw, ch, cv], 84)?);
        out.push(self.rfk(reader, "DPK", H2C_FUNC_RFK_DPK, &[8, 0, 1, RF_AB, band, bw, ch, 0], 34)?);
        out.push(self.rfk(reader, "RXDCK", H2C_FUNC_RFK_RXDCK, &self.rxdck_args(c, true), 32)?);
        Ok(out)
    }

    /// rtw89_h2c_rf_rxdck: len, phy, is_afe, kpath, band, bw, ch, dbg, is_chl_k
    fn rxdck_args(&self, c: &Chan, chl_k: bool) -> [u8; 9] {
        [9, 0, 0, RF_AB, c.band as u8, c.bw as u8, c.center, 0, chl_k as u8]
    }

    /// _wait_rx_mode: 두 경로의 RF가 송신 모드(2)에서 벗어날 때까지.
    fn wait_rx_mode(&self) -> io::Result<()> {
        for path in 0..2 {
            let t = Instant::now();
            while self.read_rf(path, RR_MOD, RR_MOD_MASK)? == 2 && t.elapsed() < Duration::from_millis(5) {}
        }
        Ok(())
    }

    /// rtw89_phy_rfk_pre_ntfy_and_wait: 보정 표의 채널들을 펌웨어에 알립니다 (사전 알림 v2, 이어서 MCC v0).
    fn pre_ntfy(&self, reader: &mut Reader, c: &Chan) -> io::Result<RfkResult> {
        let tbl = self.rfk_table();
        let cur = tbl.iter().position(|t| t.as_ref() == Some(c)).unwrap_or(0);
        let field = |f: fn(&Chan) -> u32| -> [u32; RFK_TABLES] { std::array::from_fn(|i| tbl[i].as_ref().map_or(0, f)) };
        let (chs, bands, rf18s) = (field(|t| t.center as u32), field(|t| t.band as u32), field(Chan::rf18));
        // rtw89_fw_h2c_rfk_pre_info_v2: dbcc.ch[경로][표], dbcc.band[경로][표], mlo_mode, cur_ch[경로],
        // cur_band[경로], phy_idx, mlo_1_1, cur_bandwidth[경로]. 두 경로가 같은 표를 씁니다.
        let mut w = Vec::new();
        w.extend(chs.iter().chain(&chs));
        w.extend(bands.iter().chain(&bands));
        w.push(MLO_2_PLUS_0_1RF);
        w.extend([chs[cur], chs[cur], bands[cur], bands[cur], 0, 0, c.bw as u32, c.bw as u32]);
        let r = self.rfk(reader, "PRE_NTFY", H2C_FUNC_RFK_PRE_NOTIFY, &le(&w), 5)?;
        // rtw89_fw_h2c_rfk_pre_info_mcc_v0: tbl_18[표][경로], cur_18[경로], mlo_mode.
        let mut m: Vec<u32> = rf18s.iter().flat_map(|v| [*v, *v]).collect();
        m.extend([rf18s[cur], rf18s[cur], MLO_2_PLUS_0_1RF]);
        self.h2c(
            H2C_CAT_OUTSRC,
            H2C_CL_OUTSRC_RF_FW_NOTIFY,
            H2C_FUNC_OUTSRC_RF_MCC_INFO,
            false,
            false,
            &le(&m),
        )?;
        Ok(r)
    }

    /// 보정 명령 하나를 보내고 결과 C2H를 기다립니다.
    fn rfk(&self, reader: &mut Reader, name: &'static str, func: u32, args: &[u8], ms: u64) -> io::Result<RfkResult> {
        let t = Instant::now();
        self.h2c(H2C_CAT_OUTSRC, H2C_CL_OUTSRC_RF_FW_RFK, func, false, false, args)?;
        let state = self.wait_c2h(reader, C2H_CAT_OUTSRC, C2H_CLASS_RFK_REPORT, C2H_FUNC_RFK_STATE, ms * USB_WAIT_SCALE)?;
        let state = state.and_then(|p| p.first().copied());
        Ok(RfkResult {
            name,
            state,
            ms: t.elapsed().as_millis(),
        })
    }

    /// 벌크 IN에서 (cat, class, func) C2H가 올 때까지 다른 패킷은 버리며 읽습니다. 내용을 돌려줍니다.
    pub fn wait_c2h(&self, reader: &mut Reader, cat: u8, class: u8, func: u8, ms: u64) -> io::Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let got = reader.next_timeout(left, |buf| {
                let mut found = None;
                rx::packets(buf, |d, data| {
                    if found.is_none()
                        && d.kind == rx::RX_TYPE_C2H
                        && let Some(c) = rx::c2h(data)
                        && (c.cat, c.class, c.func) == (cat, class, func)
                    {
                        found = Some(c.payload.to_vec());
                    }
                });
                found
            })?;
            if let Some(Some(p)) = got {
                return Ok(Some(p));
            }
        }
    }
}

impl RfkResult {
    pub fn ok(&self) -> bool {
        self.state == Some(RFK_STATE_OK)
    }
}
