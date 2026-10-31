//! 주기적으로 도는 동적 조정 (rtw88의 2초마다 도는 watchdog): 오경보와 가장 약한 상대의 신호로
//! 정하는 동적 초기 이득 (phy.c rtw_phy_dig), 그것을 따르는 EDCCA 문턱 (rtw8822b_adaptivity), 칩
//! 온도에 맞춘 송신 출력 추적과 많이 어긋났을 때의 IQK 재실행 (rtw8822b_pwr_track). rtw88에서
//! BSD-3-Clause로 옮겼습니다.
use std::io;

use crate::rtl8822b::Rtl8822b;

const DIG_PERF_FA_TH_LOW: u32 = 250;
const DIG_PERF_FA_TH_HIGH: u32 = 500;
const DIG_PERF_FA_TH_EXTRA_HIGH: u32 = 750;
const DIG_PERF_MAX: u8 = 0x5a;
const DIG_PERF_MID: u8 = 0x40;
const DIG_CVRG_FA_TH_LOW: u32 = 2000;
const DIG_CVRG_FA_TH_HIGH: u32 = 4000;
const DIG_CVRG_FA_TH_EXTRA_HIGH: u32 = 5000;
const DIG_CVRG_MAX: u8 = 0x2a;
const DIG_CVRG_MID: u8 = 0x26;
const DIG_CVRG_MIN: u8 = 0x1c;
const DIG_RSSI_GAIN_OFFSET: u8 = 15;
/// rtw8822b dig_min
const DIG_MIN: u8 = 0x1c;
const EDCCA_TH_L2H_LB: i32 = 48;
const EDCCA_IGI_L2H_DIFF: i32 = 8;
const EDCCA_L2H_H2L_DIFF_NORMAL: i32 = 8;
const IQK_THRESHOLD: u8 = 8;
const RF_T_METER: u32 = 0x42;
/// 5 GHz 출력 추적 (8822B의 두 경로와 모든 5 GHz 대역에 같음, RFE 형식 0 표): 보정 때보다 차가울
/// 때(n)와 뜨거울 때(p) 온도 차에 따른 송신 출력 지수 단계.
const PWRTRK_5G_N: [u8; 30] = [
    0, 1, 2, 2, 3, 4, 5, 5, 6, 7, 8, 8, 9, 10, 11, 11, 12, 13, 14, 14, 15, 16, 17, 17, 18, 19, 20, 20, 21, 22,
];
const PWRTRK_5G_P: [u8; 30] = [
    0, 1, 2, 2, 3, 4, 5, 5, 6, 7, 8, 9, 9, 10, 11, 12, 13, 14, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 22, 23,
];
const TXSCALE: [u32; 37] = [
    0x081, 0x088, 0x090, 0x099, 0x0a2, 0x0ac, 0x0b6, 0x0c0, 0x0cc, 0x0d8, 0x0e5, 0x0f2, 0x101, 0x110, 0x120, 0x131, 0x143, 0x156, 0x16a, 0x180,
    0x197, 0x1af, 0x1c8, 0x1e3, 0x200, 0x21e, 0x23e, 0x261, 0x285, 0x2ab, 0x2d3, 0x2fe, 0x32b, 0x35c, 0x38e, 0x3c4, 0x3fe,
];

/// rtw_phy_rf_power_2_rssi: 경로별 출력(dBm)을 rtw88의 0-100 RSSI로.
pub fn rssi_of(power: &[i8]) -> u8 {
    let lin: f32 = power.iter().map(|&p| 10f32.powf(p.clamp(-100, 0) as f32 / 10.0)).sum::<f32>() / power.len() as f32;
    (100.0 + 10.0 * lin.log10()).clamp(0.0, 100.0) as u8
}

/// 조정들이 실행 사이에 이어 가는 상태.
#[derive(Debug, Default)]
pub struct Dm {
    /// EDCCA 문턱이 IGI를 따르게 합니다 (rtw88의 평소 적응 모드). 끄면 올리기 때의 최댓값에 두어,
    /// 진짜 Wi-Fi 프리앰블만 우리 송신을 막습니다.
    pub edcca: bool,
    /// 칩 온도에 맞춰 송신 출력을 추적합니다 (efuse의 속도별 출력에 맞춘 rtw88의 표라, 높은 고정
    /// 지수 위에 더하면 PA가 선형 범위를 넘습니다). 온도계는 어느 쪽이든 읽어 로그에 남기고,
    /// 어긋나면 IQK를 다시 돌립니다.
    pub pwr_track: bool,
    igi_history: [u8; 4],
    fa_history: [u32; 4],
    igi_bitmap: u8,
    damping: bool,
    damping_rssi: u8,
    damping_cnt: u8,
    min_rssi: u8,
    pre_min_rssi: u8,
    started: bool,
    // 출력 추적
    trk_triggered: bool,
    trk_init_done: bool,
    thermal_ewma: u32,
    delta_idx: [i8; 2],
    thermal_k: u8,
    default_ofdm: u8,
}

/// 한 번의 실행이 본 것과 한 것 (로그용).
#[derive(Debug, Clone, Copy, Default)]
pub struct DmReport {
    pub false_alarms: u32,
    pub counters: crate::rtl8822b::phy::PhyCounters,
    pub igi: u8,
    pub thermal: Option<u8>,
    pub iqk: bool,
}

impl Rtl8822b {
    /// watchdog 한 번. `min_rssi`는 상대가 있으면 가장 약한 상대의 0-100 RSSI, `tx_power`는 쓰고
    /// 있는 고정 송신 출력 지수입니다.
    pub fn watchdog(&self, dm: &mut Dm, min_rssi: Option<u8>, tx_power: u8) -> io::Result<DmReport> {
        let r = &self.regs;
        if !dm.started {
            dm.started = true;
            dm.igi_history[0] = self.igi()?;
            dm.thermal_k = self.efuse.thermal_meter;
            let swing = r.r32m(0xc1c, 0xffe0_0000)?;
            dm.default_ofdm = TXSCALE.iter().position(|&v| v == swing).unwrap_or(24) as u8;
        }
        let counters = self.counters()?;
        let mut rep = DmReport {
            false_alarms: counters.false_alarms,
            counters,
            ..Default::default()
        };
        // ---- DIG
        let linked = min_rssi.is_some();
        dm.pre_min_rssi = dm.min_rssi;
        dm.min_rssi = min_rssi.unwrap_or(0);
        let fa = rep.false_alarms;
        let mut igi = dm.igi_history[0];
        if !self.dig_damping(dm) {
            let (th, step) = if linked {
                let step = if dm.pre_min_rssi > dm.min_rssi { [6, 4, 2] } else { [4, 3, 2] };
                ([DIG_PERF_FA_TH_EXTRA_HIGH, DIG_PERF_FA_TH_HIGH, DIG_PERF_FA_TH_LOW], step)
            } else {
                ([DIG_CVRG_FA_TH_EXTRA_HIGH, DIG_CVRG_FA_TH_HIGH, DIG_CVRG_FA_TH_LOW], [4, 3, 2])
            };
            let mut cur = igi as i32;
            if let Some(level) = th.iter().position(|&t| fa > t) {
                cur += step[level];
            }
            cur -= 2;
            let (dig_max, dig_mid, dig_min, rssi) = if linked {
                (DIG_PERF_MAX, DIG_PERF_MID, DIG_MIN, dm.min_rssi.max(DIG_MIN))
            } else {
                (DIG_CVRG_MAX, DIG_CVRG_MID, DIG_CVRG_MIN, DIG_CVRG_MIN)
            };
            let dig_max = dig_max.min(rssi.saturating_add(DIG_RSSI_GAIN_OFFSET));
            let lower = rssi.clamp(dig_min, dig_mid);
            let upper = lower.saturating_add(DIG_RSSI_GAIN_OFFSET).clamp(dig_min, dig_max.max(dig_min));
            let cur = cur.clamp(lower as i32, upper as i32) as u8;
            // rtw_phy_dig_recorder
            dm.igi_bitmap = ((dm.igi_bitmap << 1) & 0xfe) | (cur > dm.igi_history[0]) as u8;
            dm.igi_history = [cur, dm.igi_history[0], dm.igi_history[1], dm.igi_history[2]];
            dm.fa_history = [fa, dm.fa_history[0], dm.fa_history[1], dm.fa_history[2]];
            if cur != igi {
                self.set_igi(cur)?;
            }
            igi = cur;
        }
        rep.igi = igi;
        // ---- EDCCA (평소 모드): 문턱이 IGI를 따릅니다
        if dm.edcca {
            let l2h = (igi as i32 + EDCCA_IGI_L2H_DIFF).max(EDCCA_TH_L2H_LB);
            let h2l = l2h - EDCCA_L2H_H2L_DIFF_NORMAL;
            r.w32m(0x8a4, 0xff, l2h as u32 & 0xff)?;
            r.w32m(0x8a4, 0xff00, h2l as u32 & 0xff)?;
        }
        // ---- 출력 추적: 온도계를 켜고 다음 실행에 읽습니다
        if self.efuse.thermal_meter != 0xff {
            if !dm.trk_triggered {
                r.rf_write(0, RF_T_METER, (1 << 17) | (1 << 16), 0x03)?;
                dm.trk_triggered = true;
            } else {
                dm.trk_triggered = false;
                let t = r.rf_read(0, RF_T_METER, 0xfc00)? as u8;
                rep.thermal = Some(t);
                rep.iqk = self.pwr_track(dm, t, tx_power)?;
            }
        }
        Ok(rep)
    }

    /// rtw_phy_dig_check_damping: IGI가 오경보에 맞서 출렁이기 시작하면 붙잡습니다.
    fn dig_damping(&self, dm: &mut Dm) -> bool {
        if dm.damping {
            let diff = dm.min_rssi.abs_diff(dm.damping_rssi);
            dm.damping_cnt += 1;
            if diff > 3 || dm.damping_cnt > 20 {
                dm.damping = false;
                return false;
            }
            return true;
        }
        let (h, f) = (&dm.igi_history, &dm.fa_history);
        let (lo, hi) = (DIG_PERF_FA_TH_LOW, DIG_PERF_FA_TH_HIGH);
        let damping = match dm.igi_bitmap & 0xf {
            5 => h[0] > h[1] && h[2] > h[3] && h[0] - h[1] >= 2 && h[2] - h[3] >= 2 && f[0] > hi && f[1] < lo && f[2] > hi && f[3] < lo,
            9 => h[0] > h[1] && h[3] > h[2] && h[0] - h[1] >= 4 && h[3] - h[2] >= 2 && f[0] > hi && f[1] < lo && f[2] < lo && f[3] > hi,
            _ => false,
        };
        if damping {
            dm.damping = true;
            dm.damping_cnt = 0;
            dm.damping_rssi = dm.min_rssi;
        }
        damping
    }

    /// rtw8822b_phy_pwrtrack: efuse 보정과의 온도 차만큼 경로마다 송신 AGC / BB 스윙을 옮기고,
    /// 8단계 넘게 어긋나면 IQK를 다시 돌립니다. IQK를 돌렸는지 돌려줍니다.
    fn pwr_track(&self, dm: &mut Dm, thermal: u8, tx_power: u8) -> io::Result<bool> {
        let r = &self.regs;
        // DECLARE_EWMA(thermal, 10, 4)
        let v = (thermal as u32) << 10;
        dm.thermal_ewma = if dm.thermal_ewma == 0 { v } else { (dm.thermal_ewma * 3 + v) / 4 };
        let avg = (dm.thermal_ewma >> 10) as u8;
        if !dm.pwr_track {
            return self.maybe_iqk(dm, avg);
        }
        let changed = avg != thermal;
        if dm.trk_init_done && !changed {
            return self.maybe_iqk(dm, avg);
        }
        dm.trk_init_done = true;
        let base = self.efuse.thermal_meter;
        let delta = (avg.abs_diff(base) as usize).min(29);
        let idx: i8 = if avg > base {
            PWRTRK_5G_P[delta] as i8
        } else {
            -(PWRTRK_5G_N[delta] as i8)
        };
        for path in 0..self.rf_paths.min(2) {
            if dm.delta_idx[path] == idx {
                continue;
            }
            dm.delta_idx[path] = idx;
            // rtw8822b_txagc_swing_offset: 송신 AGC로 15단계까지, 나머지는 BB 스윙으로.
            let offset = 0x3fu8.saturating_sub(tx_power.min(0x3f)).min(0xf) as i8;
            let def = dm.default_ofdm as i32;
            let (agc, swing) = if idx >= 0 {
                if idx <= offset {
                    (idx, def)
                } else {
                    (offset, (def + (idx - offset) as i32).min(def + 10))
                }
            } else {
                (0, (def + idx as i32).max(0))
            };
            let swing = (swing as usize).min(TXSCALE.len() - 1);
            let (r1, r2) = if path == 0 { (0xc94, 0xc1c) } else { (0xe94, 0xe1c) };
            r.w32m(r1, 0x3e00_0000, agc as u32 & 0x1f)?;
            r.w32m(r2, 0xffe0_0000, TXSCALE[swing])?;
            tracing::debug!("power tracking: thermal {avg} (efuse {base}), path {path} AGC +{agc}, swing {swing}");
        }
        self.maybe_iqk(dm, avg)
    }

    fn maybe_iqk(&self, dm: &mut Dm, avg: u8) -> io::Result<bool> {
        if avg.abs_diff(dm.thermal_k) >= IQK_THRESHOLD {
            dm.thermal_k = avg;
            self.iqk()?;
            return Ok(true);
        }
        Ok(false)
    }
}
