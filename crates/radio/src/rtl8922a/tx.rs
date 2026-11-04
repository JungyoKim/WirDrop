//! 보내기: 우리 주소로 오는 프레임에 하드웨어가 ACK/Block Ack을 보내게 하는 역할 설정
//! (rtw89_mac_vif_init: 포트, MACID 풀기, 역할, 접속 정보, 주소 CAM, 기본 CMAC/DMAC 표)과
//! 송신 서술자 v2 (rtw89_core_fill_txdesc_v2, rtw89_usb_ops_tx_write). AWDL은 BSSID가 정해진
//! 애드혹 망과 비슷해서 rtw89의 애드혹 역할(NL80211_IFTYPE_ADHOC)을 씁니다.
use std::io;

use super::generated::regs::*;
use super::{Rtl8922a, put};

/// 송신 서술자: 본문 (rtw89_txwd_body_v2) + 정보 (rtw89_txwd_info_v2).
pub const TX_DESC_SIZE: usize = 64;
const WD_BODY_SIZE: usize = 32;
/// 벌크 OUT 순서에서 DMA 채널의 자리 (rtw8922a_usb_info.bulkout_id).
const BULKOUT_ACH0: usize = 3;
const BULKOUT_B0MG: usize = 0;
/// DMA 채널 (rtw89_txch)과 큐 (rtw89_tx_qsel). USB는 rtw89_core_get_ch_dma_v2: BE/VO는 ACH0.
const TXCH_ACH0: u32 = 0;
const TXCH_B0MG: u32 = 8;
const QSEL_BE_0: u32 = 0x00;
const QSEL_VO_0: u32 = 0x03;
const QSEL_B0_MGMT: u32 = 0x12;
/// 우리 역할의 MACID와 포트.
const MACID_SELF: u32 = 0;
/// rtw89 열거형 값.
const NET_TYPE_AD_HOC: u32 = 1;
const WIFI_ROLE_ADHOC: u32 = 4;
const SELF_ROLE_CLIENT: u32 = 0;
const ROLE_CREATE: u32 = 0;
const FW_AX_STA: u32 = 1;
const ADDR_CAM_SEC_NORMAL: u32 = 2;
const H2C_FUNC_MAC_MACID_PAUSE_SLEEP: u32 = 0x28;
const H2C_FUNC_USR_EDCA: u32 = 0xf;
/// Block Ack CAM (fw.h): 묶음과 기능, 칩의 동적 항목 수 (rtw8922a의 bacam_num 24, 그중 동적 8).
const H2C_CL_BA_CAM: u32 = 0xc;
const H2C_FUNC_MAC_BA_CAM_V1: u32 = 0x1;
const BACAM_DYNAMIC_NUM: u8 = 8;
/// 정적 Block Ack CAM 항목 수.
pub const BACAM_STATIC_NUM: u8 = 16;
/// ac_to_fw_idx[IEEE80211_AC_VO]
const FW_AC_VO: u32 = 3;
/// IEEE80211_EML_CAP_EMLSR_PADDING_DELAY_256US, _TRANSITION_DELAY_256US
const EMLSR_PADDING_256US: u32 = 4;
const EMLSR_TRANSITION_256US: u32 = 5;

/// 보내는 속도 (rtw89 하드웨어 속도 부호 v1: 10..8 방식, 7..5 NSS-1, 4..0 MCS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwRate {
    pub code: u16,
    /// 0 = 20 MHz, 1 = 40, 2 = 80, 3 = 160.
    pub bw: u8,
    /// RTW89_GILTF_*: HE는 0 = 4x 3.2 us, 1 = 4x 0.8, 2 = 2x 1.6, 3 = 2x 0.8, 4 = 1x 1.6, 5 = 1x 0.8.
    /// HT/VHT는 1이면 짧은 GI.
    pub gi_ltf: u8,
    pub ldpc: bool,
    pub stbc: bool,
}

impl HwRate {
    /// 20 MHz 레거시: 0-3 CCK, 4-11 OFDM 6-54 Mb/s.
    pub const fn legacy(idx: u8) -> HwRate {
        HwRate {
            code: idx as u16,
            bw: 0,
            gi_ltf: 0,
            ldpc: false,
            stbc: false,
        }
    }
    pub const OFDM6: HwRate = HwRate::legacy(4);
    pub const OFDM24: HwRate = HwRate::legacy(8);

    pub const fn ht(mcs: u8, bw: u8, sgi: bool) -> HwRate {
        HwRate {
            code: 0x100 | (mcs & 0x1f) as u16,
            bw,
            gi_ltf: sgi as u8,
            ldpc: false,
            stbc: false,
        }
    }
    pub const fn vht(nss: u8, mcs: u8, bw: u8, sgi: bool, ldpc: bool) -> HwRate {
        HwRate {
            code: Self::v1(2, nss, mcs),
            bw,
            gi_ltf: sgi as u8,
            ldpc,
            stbc: false,
        }
    }
    pub const fn he(nss: u8, mcs: u8, bw: u8, gi_ltf: u8, ldpc: bool) -> HwRate {
        HwRate {
            code: Self::v1(3, nss, mcs),
            bw,
            gi_ltf,
            ldpc,
            stbc: false,
        }
    }
    const fn v1(mode: u16, nss: u8, mcs: u8) -> u16 {
        let nss = if nss == 0 { 0 } else { nss - 1 };
        (mode << 8) | (((nss & 7) as u16) << 5) | (mcs & 0x1f) as u16
    }
}

/// USB 전송 하나에 담는 한도 ([Rtl8922a::send_many]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxAgg {
    /// 프레임 수 (서술자 칸은 127까지).
    pub frames: usize,
    /// 서술자를 포함한 바이트.
    pub bytes: usize,
}

impl TxAgg {
    /// 전송 하나에 프레임 하나 (rtw89).
    pub const OFF: TxAgg = TxAgg {
        frames: 1,
        bytes: usize::MAX,
    };

    pub fn new(frames: usize, bytes: usize) -> TxAgg {
        TxAgg {
            frames: frames.clamp(1, 127),
            bytes: bytes.max(TX_DESC_SIZE + 2304),
        }
    }
}

/// 내보낼 프레임 (802.11 머리말부터, FCS 없이).
#[derive(Debug, Clone, Copy)]
pub struct TxFrame<'a> {
    pub data: &'a [u8],
    pub rate: HwRate,
    /// A-MPDU에 실어도 됩니다 (받는 쪽과 Block Ack 합의가 열려 있음).
    pub ampdu: bool,
    /// A-MPDU 하나의 최대 MPDU 수.
    pub ampdu_max: u8,
    /// A-MPDU 최소 MPDU 간격 (802.11 부호).
    pub ampdu_density: u8,
    /// ACK를 기대하지 않습니다.
    pub no_ack: bool,
    /// 이 번호(4비트)를 단 송신 보고를 청합니다.
    pub report: Option<u8>,
    /// 최선 노력 대신 음성 큐로.
    pub voice: bool,
    /// 하드웨어 재전송 횟수 (없으면 칩 기본값).
    pub retry: Option<u8>,
}

impl<'a> TxFrame<'a> {
    pub fn new(data: &'a [u8], rate: HwRate) -> TxFrame<'a> {
        TxFrame {
            data,
            rate,
            ampdu: false,
            ampdu_max: 32,
            ampdu_density: 6,
            no_ack: false,
            report: None,
            voice: false,
            retry: None,
        }
    }
}

fn put_le(buf: &mut [u8], words: &[u32]) {
    for (i, w) in words.iter().enumerate() {
        buf[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
}

fn le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// rtw89_cam_addr_hash
fn addr_hash(a: &[u8; 6]) -> u32 {
    a.iter().fold(0u8, |h, b| h ^ b) as u32
}

impl Rtl8922a {
    /// 애드혹 역할 하나를 만듭니다 (rtw89_mac_vif_init): 주소 mac으로 온 프레임에 하드웨어가 ACK와
    /// Block Ack을 보내고, bssid가 같은 프레임을 우리 망으로 봅니다. [Rtl8922a::set_channel] 다음에.
    pub fn vif_init(&self, mac: [u8; 6], bssid: [u8; 6]) -> io::Result<()> {
        self.port_init()?;
        // rtw89_fw_h2c_macid_pause (MACID_PAUSE_SLEEP 형식): MACID 0을 멈춤과 잠듦에서 풀기.
        let mut w = [0u32; 64];
        w[4] = 1 << MACID_SELF; // n[0].pause_mask_grp[0]
        w[12] = 1 << MACID_SELF; // n[0].sleep_mask_grp[0]
        self.h2c(
            H2C_CAT_MAC,
            H2C_CL_MAC_FW_OFLD,
            H2C_FUNC_MAC_MACID_PAUSE_SLEEP,
            true,
            false,
            &le_bytes(&w),
        )?;
        // rtw89_fw_h2c_role_maintain
        let role = put(0, RTW89_H2C_ROLE_MAINTAIN_W0_MACID, MACID_SELF)
            | put(0, RTW89_H2C_ROLE_MAINTAIN_W0_SELF_ROLE, SELF_ROLE_CLIENT)
            | put(0, RTW89_H2C_ROLE_MAINTAIN_W0_UPD_MODE, ROLE_CREATE)
            | put(0, RTW89_H2C_ROLE_MAINTAIN_W0_WIFI_ROLE, WIFI_ROLE_ADHOC);
        self.h2c(
            H2C_CAT_MAC,
            H2C_CL_MAC_MEDIA_RPT,
            H2C_FUNC_MAC_FWROLE_MAINTAIN,
            false,
            true,
            &role.to_le_bytes(),
        )?;
        self.join_info(false)?;
        self.addr_cam(&mac, &bssid)?;
        self.default_cmac_tbl(MACID_SELF as u8)?;
        self.default_dmac_tbl(MACID_SELF as u8)
    }

    /// rtw89_mac_port_update의 포트 0 부분 (애드혹): 망 종류, 비콘 보호, BSSID 맞추기, TSF. 비콘은
    /// 보내지 않습니다 (AWDL은 액션 프레임으로 알립니다).
    fn port_init(&self) -> io::Result<()> {
        let p = R_BE_PORT_CFG_P0;
        self.clr32(p, B_AX_PORT_FUNC_EN)?;
        self.w32_mask(p, B_AX_NET_TYPE_MASK, NET_TYPE_AD_HOC)?;
        self.set32(p, B_AX_TBTT_PROHIB_EN | B_AX_BRK_SETUP | B_AX_RX_BSSID_FIT_EN)?;
        self.clr32(p, B_AX_TSF_UDT_EN | B_AX_BCNTX_EN)?;
        self.set32(p, B_AX_PORT_FUNC_EN)
    }

    /// rtw89_fw_h2c_join_info (BE의 v1 형식). connected = false면 "끊김" 상태로 둡니다.
    fn join_info(&self, connected: bool) -> io::Result<()> {
        let w0 = put(0, RTW89_H2C_JOININFO_W0_MACID, MACID_SELF)
            | put(0, RTW89_H2C_JOININFO_W0_OP, !connected as u32)
            | put(0, RTW89_H2C_JOININFO_W0_NET_TYPE, NET_TYPE_AD_HOC)
            | put(0, RTW89_H2C_JOININFO_W0_WIFI_ROLE, WIFI_ROLE_ADHOC)
            | put(0, RTW89_H2C_JOININFO_W0_SELF_ROLE, SELF_ROLE_CLIENT);
        let w1 = put(0, RTW89_H2C_JOININFO_W1_STA_TYPE, FW_AX_STA)
            | put(0, RTW89_H2C_JOININFO_W1_MAIN_MACID, MACID_SELF)
            | put(0, RTW89_H2C_JOININFO_W1_MLO_MODE, RTW89_H2C_JOININFO_MLO_MODE_MLSR)
            | put(0, RTW89_H2C_JOININFO_W1_EMLSR_PADDING, EMLSR_PADDING_256US)
            | put(0, RTW89_H2C_JOININFO_W1_EMLSR_TRANS_DELAY, EMLSR_TRANSITION_256US);
        self.h2c(
            H2C_CAT_MAC,
            H2C_CL_MAC_MEDIA_RPT,
            H2C_FUNC_MAC_JOININFO,
            false,
            true,
            &le_bytes(&[w0, w1, 0]),
        )
    }

    /// rtw89_fw_h2c_cam (ADDR_CAM_V0 형식): 주소 CAM 0에 우리 주소(SMA)와 BSSID(TMA), BSSID CAM 0.
    fn addr_cam(&self, sma: &[u8; 6], bssid: &[u8; 6]) -> io::Result<()> {
        self.addr_cam_entry(0, MACID_SELF as u8, sma, bssid, bssid, true)
    }

    /// 우리에게 보내는 상대 하나를 MACID macid(1부터)로 둡니다 (rtw89가 AP 모드에서 접속한 기기마다
    /// 하는 것): 주소 CAM의 같은 자리에 그 주소를 보내는 이(TMA)로 적고 DMAC 표를 비웁니다. 이 칩은
    /// 주소 CAM에 없는 상대의 A-MPDU에는 아무것도 답하지 않습니다. 등록하면 하드웨어가 그 상대의
    /// A-MPDU에 Block Ack을 보냅니다 (항목은 하드웨어가 스스로 만들고, [Rtl8922a::ba_cam]이 합의대로
    /// 고정합니다).
    ///
    /// 상대의 CMAC 표는 주소 CAM보다 먼저 기본값으로 씁니다 ([Rtl8922a::peer_cmac_tbl]). 상대에게는
    /// MACID 0으로 보냅니다: MACID 0으로 보낸 A-MPDU에도 하드웨어는 상대의 Block Ack을 바르게
    /// 짝짓습니다.
    pub fn add_peer(&self, macid: u8, peer: &[u8; 6], ours: &[u8; 6], bssid: &[u8; 6]) -> io::Result<()> {
        self.peer_cmac_tbl(macid)?;
        self.addr_cam_entry(macid, macid, ours, peer, bssid, true)?;
        self.default_dmac_tbl(macid)
    }

    /// [Rtl8922a::add_peer]로 둔 상대를 지웁니다.
    pub fn remove_peer(&self, macid: u8, peer: &[u8; 6], ours: &[u8; 6], bssid: &[u8; 6]) -> io::Result<()> {
        self.addr_cam_entry(macid, macid, ours, peer, bssid, false)
    }

    /// rtw89_fw_h2c_ba_cam_v1: MACID macid의 TID tid에 대한 받는 쪽 Block Ack 합의 (ssn에서 시작,
    /// 창 buf_size). entry는 정적 항목 번호 (0..[BACAM_STATIC_NUM]). valid = false면 지웁니다.
    pub fn ba_cam(&self, entry: u8, macid: u8, tid: u8, ssn: u16, buf_size: u16, valid: bool) -> io::Result<()> {
        let bmap_size = match buf_size {
            513.. => 10,
            257.. => 8,
            65.. => 4,
            _ => 0,
        };
        let w0 = valid as u32
            | 1 << 1 // INIT_REQ
            | put(0, 0xf0, tid as u32)
            | put(0, 0xff00, macid as u32)
            | put(0, 0xf_0000, bmap_size)
            | put(0, 0xfff0_0000, ssn as u32);
        // 정적 항목은 동적 항목 다음, 대역 0.
        let w1 = put(0, 0xff00_0000, (entry + BACAM_DYNAMIC_NUM) as u32) | 1 << 8; // STD_ENTRY_EN
        self.h2c(H2C_CAT_MAC, H2C_CL_BA_CAM, H2C_FUNC_MAC_BA_CAM_V1, false, true, &le_bytes(&[w0, w1]))
    }

    fn addr_cam_entry(&self, idx: u8, macid: u8, sma: &[u8; 6], tma: &[u8; 6], bssid: &[u8; 6], valid: bool) -> io::Result<()> {
        let mut w = [0u32; 15];
        w[1] = put(0, ADDR_CAM_W1_IDX, idx as u32) | put(0, ADDR_CAM_W1_OFFSET, 0) | put(0, ADDR_CAM_W1_LEN, ADDR_CAM_ENT_SHORT_SIZE);
        w[2] = if valid { ADDR_CAM_W2_VALID } else { 0 }
            | put(0, ADDR_CAM_W2_NET_TYPE, NET_TYPE_AD_HOC)
            | put(0, ADDR_CAM_W2_SMA_HASH, addr_hash(sma))
            | put(0, ADDR_CAM_W2_TMA_HASH, addr_hash(tma));
        w[3] = put(0, ADDR_CAM_W3_BSSID_CAM_IDX, 0);
        w[4] = u32::from_le_bytes([sma[0], sma[1], sma[2], sma[3]]);
        w[5] = u32::from_le_bytes([sma[4], sma[5], tma[0], tma[1]]);
        w[6] = u32::from_le_bytes([tma[2], tma[3], tma[4], tma[5]]);
        w[8] = put(0, ADDR_CAM_W8_MACID, macid as u32);
        w[9] = put(0, ADDR_CAM_W9_SEC_ENT_MODE, ADDR_CAM_SEC_NORMAL);
        w[12] = put(0, ADDR_CAM_W12_BSSID_IDX, 0) | put(0, ADDR_CAM_W12_BSSID_OFFSET, 0) | put(0, ADDR_CAM_W12_BSSID_LEN, BSSID_CAM_ENT_SIZE);
        w[13] = ADDR_CAM_W13_BSSID_VALID
            | put(0, ADDR_CAM_W13_BSSID_MASK, RTW89_BSSID_MATCH_ALL)
            | put(0, ADDR_CAM_W13_BSSID_BSSID0, bssid[0] as u32)
            | put(0, ADDR_CAM_W13_BSSID_BSSID1, bssid[1] as u32);
        w[14] = u32::from_le_bytes([bssid[2], bssid[3], bssid[4], bssid[5]]);
        self.h2c(
            H2C_CAT_MAC,
            H2C_CL_MAC_ADDR_CAM_UPDATE,
            H2C_FUNC_MAC_ADDR_CAM_UPD,
            false,
            true,
            &le_bytes(&w),
        )
    }

    /// rtw89_fw_h2c_default_cmac_tbl_g7: MACID의 송신 기본값 (기본 속도 OFDM 6, RTS 24 Mb/s 등).
    fn default_cmac_tbl(&self, macid: u8) -> io::Result<()> {
        self.cmac_tbl(macid, false)
    }

    /// 상대 MACID의 CMAC 표를 기본값으로 씁니다. 이 표는 칩의 SRAM이라 쓰지 않으면 올릴 때마다
    /// 무작위 값이 남습니다. 그중 W8의 ALL_ACK_SUPPORT가 1로 남으면, 칩은 그 상대의 A-MPDU를 모두
    /// 받았을 때 HE의 Multi-STA Block Ack(All Ack)으로 답합니다. VHT로 보내는 iPhone은 이것을
    /// Block Ack으로 보지 않아 같은 데이터를 계속 다시 보냅니다 (받기의 긴 '대기 중'과 느린 속도가
    /// 올릴 때마다 반반으로 나타난 원인). 펌웨어가 주소 CAM을 쓸 때 채우는 W5의 ADDR_CAM_INDEX는
    /// 건드리지 않습니다: 0으로 덮으면 칩이 그 상대에게 ACK도 Block Ack도 보내지 않습니다.
    fn peer_cmac_tbl(&self, macid: u8) -> io::Result<()> {
        self.cmac_tbl(macid, true)
    }

    /// 기본값 CMAC 표를 씁니다. keep_addr_cam_idx면 W5의 ADDR_CAM_INDEX는 그대로 둡니다.
    fn cmac_tbl(&self, macid: u8, keep_addr_cam_idx: bool) -> io::Result<()> {
        let mut w = [0u32; 33];
        w[0] = put(0, CCTLINFO_G7_C0_MACID, macid as u32) | CCTLINFO_G7_C0_OP;
        let (v, m) = w.split_at_mut(17);
        let v = &mut v[1..];
        v[0] = put(0, CCTLINFO_G7_W0_DATARATE, 4) | CCTLINFO_G7_W0_MGQ_RPT_EN;
        v[1] = put(0, CCTLINFO_G7_W1_DATA_RTY_LOWEST_RATE, 4) | put(0, CCTLINFO_G7_W1_RTSRATE, 0xa) | put(0, CCTLINFO_G7_W1_RTS_RTY_LOWEST_RATE, 4);
        v[4] = put(0, CCTLINFO_G7_W4_ACT_SUBCH_CBW, 0xffff);
        v[5] = put(0, CCTLINFO_G7_W5_NOMINAL_PKT_PADDING0, 2)
            | put(0, CCTLINFO_G7_W5_NOMINAL_PKT_PADDING1, 2)
            | put(0, CCTLINFO_G7_W5_NOMINAL_PKT_PADDING2, 2)
            | put(0, CCTLINFO_G7_W5_NOMINAL_PKT_PADDING3, 2)
            | put(0, CCTLINFO_G7_W5_NOMINAL_PKT_PADDING4, 2);
        v[6] = put(0, CCTLINFO_G7_W6_RESP_REF_RATE, 0xb);
        v[7] = put(0, CCTLINFO_G7_W7_NC, 1)
            | put(0, CCTLINFO_G7_W7_NR, 1)
            | put(0, CCTLINFO_G7_W7_CB, 1)
            | put(0, CCTLINFO_G7_W7_CSI_PARA_EN, 1)
            | put(0, CCTLINFO_G7_W7_CSI_FIX_RATE, 0xb);
        let all = [
            CCTLINFO_G7_W0_ALL,
            CCTLINFO_G7_W1_ALL,
            CCTLINFO_G7_W2_ALL,
            CCTLINFO_G7_W3_ALL,
            CCTLINFO_G7_W4_ALL,
            CCTLINFO_G7_W5_ALL,
            CCTLINFO_G7_W6_ALL,
            CCTLINFO_G7_W7_ALL,
            CCTLINFO_G7_W8_ALL,
            0,
            0,
            0,
            0,
            0,
            CCTLINFO_G7_W14_ALL,
            CCTLINFO_G7_W15_ALL,
        ];
        m.copy_from_slice(&all);
        if keep_addr_cam_idx {
            m[5] &= !CCTLINFO_G7_W5_ADDR_CAM_INDEX;
        }
        self.h2c(H2C_CAT_MAC, H2C_CL_MAC_FR_EXCHG, H2C_FUNC_MAC_CCTLINFO_UD_G7, false, true, &le_bytes(&w))
    }

    /// rtw89_fw_h2c_default_dmac_tbl_v2: MACID의 DMAC 표를 모두 0으로.
    fn default_dmac_tbl(&self, macid: u8) -> io::Result<()> {
        let mut w = [0u32; 33];
        w[0] = put(0, DCTLINFO_V2_C0_MACID, macid as u32) | DCTLINFO_V2_C0_OP;
        let all = [
            DCTLINFO_V2_W0_ALL,
            DCTLINFO_V2_W1_ALL,
            DCTLINFO_V2_W2_ALL,
            DCTLINFO_V2_W3_ALL,
            DCTLINFO_V2_W4_ALL,
            DCTLINFO_V2_W5_ALL,
            DCTLINFO_V2_W6_ALL,
            DCTLINFO_V2_W7_ALL,
            DCTLINFO_V2_W8_ALL,
            DCTLINFO_V2_W9_ALL,
            DCTLINFO_V2_W10_ALL,
            DCTLINFO_V2_W11_ALL,
            DCTLINFO_V2_W12_ALL,
        ];
        w[17..17 + all.len()].copy_from_slice(&all);
        self.h2c(H2C_CAT_MAC, H2C_CL_MAC_FR_EXCHG, H2C_FUNC_MAC_DCTLINFO_UD_V2, false, false, &le_bytes(&w))
    }

    /// 서술자 하나 (rtw89_core_fill_txdesc_v2와 rtw89_core_tx_update_mgmt_info/_data_info).
    pub fn fill_desc(&self, d: &mut [u8], f: &TxFrame) {
        let fc0 = f.data[0];
        let data = fc0 & 0x0c == 0x08;
        let group = f.data.len() > 4 && f.data[4] & 1 != 0;
        let seq = if f.data.len() >= 24 {
            (u16::from_le_bytes([f.data[22], f.data[23]]) >> 4) as u32
        } else {
            0
        };
        // QoS 데이터의 TID (머리말 24바이트 다음, 주소 4가 없으면).
        let tid = if data && fc0 & 0x80 != 0 && f.data.len() >= 26 {
            f.data[24] & 7
        } else {
            0
        };
        let (ch, qsel) = if !data {
            (TXCH_B0MG, QSEL_B0_MGMT)
        } else if f.voice || tid >= 6 {
            (TXCH_ACH0, QSEL_VO_0)
        } else {
            (TXCH_ACH0, QSEL_BE_0)
        };
        // rtw89_core_get_tid_indicate: 같은 접근 범주의 두 번째 TID.
        let tid_ind = matches!(tid, 3 | 4 | 5 | 7) as u32;
        let r = &f.rate;
        let mut w = [0u32; 16];
        w[0] = BE_TXD_BODY0_WDINFO_EN | put(0, BE_TXD_BODY0_CH_DMA, ch) | BE_TXD_BODY0_WD_PAGE | BE_TXD_BODY0_STF_MODE;
        w[2] = put(0, BE_TXD_BODY2_TXPKTSIZE, f.data.len() as u32)
            | put(0, BE_TXD_BODY2_QSEL, qsel)
            | put(0, BE_TXD_BODY2_MACID, MACID_SELF)
            | if data { put(0, BE_TXD_BODY2_TID_IND, tid_ind) } else { 0 }
            | if f.ampdu && data { BE_TXD_BODY2_AGG_EN } else { 0 };
        w[3] = put(0, BE_TXD_BODY3_WIFI_SEQ, seq) | if data { 0 } else { BE_TXD_BODY3_IS_MLD_SW_EN };
        w[6] = if f.no_ack && !group { BE_TXD_BODY6_NO_ACK } else { 0 } | if group { BE_TXD_BODY6_BMC } else { 0 };
        w[7] = BE_TXD_BODY7_USERATE_SEL
            | put(0, BE_TXD_BODY7_DATA_BW, r.bw as u32)
            | put(0, BE_TXD_BODY7_GI_LTF, r.gi_ltf as u32)
            | put(0, BE_TXD_BODY7_DATARATE, r.code as u32);
        // 정보 0..7
        w[8] = BE_TXD_INFO0_DISDATAFB
            | if r.stbc { BE_TXD_INFO0_DATA_STBC } else { 0 }
            | if r.ldpc { BE_TXD_INFO0_DATA_LDPC } else { 0 }
            | match f.retry {
                Some(n) => BE_TXD_INFO0_DATA_TXCNT_LMT_SEL | put(0, BE_TXD_INFO0_DATA_TXCNT_LMT, n as u32),
                None => 0,
            };
        w[9] = put(0, BE_TXD_INFO1_DATA_RTY_LOWEST_RATE, r.code as u32)
            | put(0, BE_TXD_INFO1_SW_DEFINE, f.report.unwrap_or(0) as u32)
            | if f.ampdu {
                put(0, BE_TXD_INFO1_MAX_AGG_NUM, f.ampdu_max.saturating_sub(1) as u32)
            } else {
                0
            };
        w[10] = put(0, BE_TXD_INFO2_AMPDU_DENSITY, f.ampdu_density as u32 & 7) | if f.report.is_some() { BE_TXD_INFO2_SPE_RPT_V1 } else { 0 };
        put_le(d, &w);
    }

    /// 프레임 하나를 보냅니다 (USB 전송 하나에 하나, rtw89_usb_ops_tx_write). 전송이 끝나기를
    /// 기다리지 않습니다 ([Usb::bulk_write_queued]).
    pub fn send(&self, f: &TxFrame) -> io::Result<()> {
        self.send_many(std::slice::from_ref(f), &TxAgg::OFF).map(drop)
    }

    /// 프레임들을 보내며, 같은 벌크 파이프로 가는 이웃 프레임은 USB 전송 하나에 담습니다. 칩은
    /// USB 전송마다 고정된 처리 시간(약 57 us)이 들어, 프레임마다 전송 하나면 초당 약 17,600
    /// 프레임(24 MB/s)이 송신의 한계였습니다. 비동기 전송을 16개까지 걸어 두어도 같았습니다.
    /// Realtek 공식 Windows 드라이버처럼 서술자+프레임을 8바이트 경계에 이어 붙이고 첫 서술자에
    /// 담은 수를 적습니다 (BE_TXD_BODY1_DMA_TXAGG_NUM). 짧은 프레임은 건너뜁니다. 나간 수를
    /// 돌려줍니다.
    pub fn send_many(&self, frames: &[TxFrame], agg: &TxAgg) -> io::Result<usize> {
        let mut sent = 0;
        let mut buf: Vec<u8> = Vec::new();
        let (mut ep, mut n) = (0u8, 0usize);
        for f in frames.iter().filter(|f| f.data.len() >= 10) {
            let data = f.data[0] & 0x0c == 0x08;
            let fep = self.usb.bulk_out[if data { BULKOUT_ACH0 } else { BULKOUT_B0MG }];
            let at = buf.len().next_multiple_of(8);
            if n > 0 && (fep != ep || n >= agg.frames || at + TX_DESC_SIZE + f.data.len() > agg.bytes) {
                sent += self.flush_agg(ep, &mut buf, n)?;
                n = 0;
            }
            let at = buf.len().next_multiple_of(8);
            buf.resize(at + TX_DESC_SIZE, 0);
            self.fill_desc(&mut buf[at..at + TX_DESC_SIZE], f);
            buf.extend_from_slice(f.data);
            ep = fep;
            n += 1;
        }
        if n > 0 {
            sent += self.flush_agg(ep, &mut buf, n)?;
        }
        Ok(sent)
    }

    /// 모은 n개를 USB 전송 하나로 내보냅니다.
    fn flush_agg(&self, ep: u8, buf: &mut Vec<u8>, n: usize) -> io::Result<usize> {
        if n > 1 {
            let w1 = u32::from_le_bytes(buf[4..8].try_into().unwrap());
            let w1 = (w1 & !BE_TXD_BODY1_DMA_TXAGG_NUM) | put(0, BE_TXD_BODY1_DMA_TXAGG_NUM, n as u32);
            buf[4..8].copy_from_slice(&w1.to_le_bytes());
        }
        self.usb.bulk_write_queued(ep, std::mem::take(buf))?;
        Ok(n)
    }

    /// 음성 큐의 채널 접근 (rtw89_fw_h2c_set_edca): 매체가 aifs_us만큼 비면 2^ecw_min - 1 슬롯의
    /// 백오프 창, 실패하면 2^ecw_max - 1까지.
    pub fn set_edca_vo(&self, aifs_us: u8, ecw_min: u8, ecw_max: u8) -> io::Result<()> {
        let w0 = FW_AC_VO << 5;
        let w1 = put(0, FW_EDCA_PARAM_CWMAX_MSK, ecw_max as u32)
            | put(0, FW_EDCA_PARAM_CWMIN_MSK, ecw_min as u32)
            | put(0, FW_EDCA_PARAM_AIFS_MSK, aifs_us as u32);
        self.h2c(H2C_CAT_MAC, H2C_CL_MAC_FW_OFLD, H2C_FUNC_USR_EDCA, false, true, &le_bytes(&[w0, w1, 0]))
    }

    /// 칩을 놓습니다: 수신을 멈추고 전원을 아날로그까지 모두 끕니다 (다음 [Rtl8922a::start]는 꺼진
    /// 칩에서 시작합니다).
    pub fn power_off(&self) {
        self.usb.abort_in();
        if let Err(e) = self.pwr_off() {
            tracing::debug!("power off: {e}");
        }
    }
}

const _: () = assert!(WD_BODY_SIZE * 2 == TX_DESC_SIZE);
