//! 펌웨어가 돈 뒤의 MAC 초기화: rtw89_mac_init의 나머지(rtw8922a_mac_enable_bb_rf, sys_init_be,
//! trx_init_be)와 USB RX 묶음 설정. rtw89는 Wi-Fi 7 칩을 두 대역 동시(DBCC)로 올리지만, 우리는 한
//! 채널만 쓰므로 단일 대역(SCC) 할당(rtw8922a_dle_mem_usb3[SCC], rtw8922a_hfc_param_ini_usb3[SCC])을
//! 씁니다.
use std::io;
use std::thread::sleep;
use std::time::Duration;

use super::generated::regs::*;
use super::{Rtl8922a, get, put};

// wde_size4_v1 = {64바이트 쪽, 연결 892, 미연결 4, 시작 0}
const WDE_LNK_PGE: u32 = 892;
// ple_size4_v1 = {256바이트 쪽, 연결 1963, 미연결 109, 시작 57344}
const PLE_PG_SIZE: u32 = 256;
const PLE_LNK_PGE: u32 = 1963;
const PLE_SRT_OFST: u32 = 57_344;
// wde_qt4_v1 = {hif, wcpu, pkt_in, cpu_io} (최소 = 최대)
const WDE_QT: [u32; 4] = [876, 6, 0, 10];
// ple_qt6_v1(최소), ple_qt7_v1(최대): cma0_tx, cma1_tx, c2h, h2c, wcpu, mpdu_proc, cma0_dma, cma1_dma,
// bb_rpt, wd_rel, cpu_io, tx_rpt, h2d
const PLE_MIN: [u32; 13] = [1806, 0, 16, 24, 7, 14, 57, 0, 12, 9, 1, 4, 0];
const PLE_MAX: [u32; 13] = [1806, 0, 16, 24, 20, 27, 70, 0, 25, 9, 1, 17, 0];
// ple_rsvd_qt2의 mpdu_info_tbl, b0_csi
const RSVD_MPDU_INFO_TBL: u32 = 1;
const RSVD_B0_CSI: u32 = 56;
// rtw8922a_hfc_chcfg_ch4: 짝수 채널(ACH0/2/4/6, B0MGQ, B1MGQ)은 {54, 606}, 홀수는 {0, 0}, 모두 0번 묶음.
const HFC_CH_MIN: u32 = 54;
const HFC_CH_MAX: u32 = 606;
// rtw8922a_hfc_pubcfg_p4, hfc_prec_cfg_c3, RTW89_HCIFC_STF
const HFC_PUB_G0: u32 = 876;
const HFC_PUB_MAX: u32 = 876;
const HFC_CH011_PREC: u32 = 18;
const HFC_H2C_PREC: u32 = 32;
const HFC_WP_PREC: u32 = 148;
const HFC_MODE_STF: u32 = 1;
const RPR_MODE_STF: u32 = 1;
/// fw.h의 enum 값.
const H2C_FUNC_OFLD_CFG: u32 = 0x14;

/// rmac_init_be의 RX 최대 길이.
const PLD_RLS_MAX_PG: u32 = 127;
const RX_MAX_LEN_UNIT: u32 = 512;
const RX_SPEC_MAX_LEN: u32 = 11_454 + RX_MAX_LEN_UNIT;

/// DEFAULT_BE_RX_FLTR (reg.h)
pub(super) fn default_rx_fltr() -> u32 {
    B_BE_A_BC_CAM_MATCH
        | B_BE_A_UC_CAM_MATCH
        | B_BE_A_MC
        | B_BE_A_BC
        | B_BE_A_A1_MATCH
        | put(0, B_BE_UID_FILTER_MASK, 15)
        | B_BE_A_FTM_REQ
        | B_BE_A_BCN_CHK_EN
}

impl Rtl8922a {
    /// 펌웨어가 돈 뒤의 MAC 초기화. [Rtl8922a::download_firmware] 다음에.
    pub fn mac_init(&mut self) -> io::Result<()> {
        // rtw8922a_mac_enable_bb_rf
        self.set8(R_BE_FEN_RST_ENABLE, B_BE_FEN_BBPLAT_RSTB | B_BE_FEN_BB_IP_RSTN)?;
        self.w32(R_BE_DMAC_SYS_CR32B, 0x7ff9_7ff9)?;
        // sys_init_be: 8922A는 dmac/cmac_share 켜기가 전원 켤 때 끝났고, CMAC 0만 켭니다.
        self.set32(R_BE_AFE_CTRL1, B_BE_R_SYM_WLCMAC0_ALL_EN)?;
        self.clr32(R_BE_FEN_RST_ENABLE, B_BE_R_SYM_ISO_CMAC02PP)?;
        self.set32(R_BE_FEN_RST_ENABLE, B_BE_CMAC0_FEN)?;
        self.set32(R_BE_CK_EN, B_BE_CK_EN_SET)?;
        self.set32(R_BE_CMAC_FUNC_EN, B_BE_CMAC_FUNC_EN_SET)?;
        self.dmac_init()?;
        self.cmac_init()?;
        // err_imr_ctrl_be(true)
        self.w32(R_BE_DMAC_ERR_IMR, DMAC_ERR_IMR_EN & !B_BE_DMAC_NOTX_ERR_INT_EN)?;
        self.w32(R_BE_CMAC_ERR_IMR, CMAC0_ERR_IMR_EN)?;
        // set_host_rpr_be (USB = STF)
        self.w32_mask(R_BE_WDRLS_CFG, B_BE_WDRLS_MODE_MASK, RPR_MODE_STF)?;
        self.w32_mask(R_BE_RLSRPT0_CFG0, B_BE_RLSRPT0_QID_MASK, WDRLS_DEST_QID_STF)?;
        let mut v = self.r32(R_BE_RLSRPT0_CFG1)?;
        v = put(
            v,
            B_BE_RLSRPT0_FLTR_MAP_MASK,
            S_BE_WDRLS_FLTR_TXOK | S_BE_WDRLS_FLTR_RTYLMT | S_BE_WDRLS_FLTR_LIFTIM | S_BE_WDRLS_FLTR_MACID,
        );
        v = put(v, B_BE_RLSRPT0_AGGNUM_MASK, 30);
        v = put(v, B_BE_RLSRPT0_TO_MASK, 255);
        self.w32(R_BE_RLSRPT0_CFG1, v)?;
        self.clr32(R_BE_RSP_CHK_SIG, B_BE_RSP_STATIC_RTS_CHK_SERV_BW_EN)?;
        // rtw89_mac_feat_init (RTW89_BACAM_V1): 대역마다 Block Ack CAM 사용자 하나.
        for (offset, band) in [(0u32, 0u32), (4, 1)] {
            let w0 = put(0, RTW89_H2C_BA_CAM_INIT_USERS_MASK, 1)
                | put(0, RTW89_H2C_BA_CAM_INIT_OFFSET_MASK, offset)
                | put(0, RTW89_H2C_BA_CAM_INIT_BAND_SEL, band);
            self.h2c(H2C_CAT_MAC, H2C_CL_BA_CAM, H2C_FUNC_MAC_BA_CAM_INIT, false, true, &w0.to_le_bytes())?;
        }
        // rtw89_usb_rx_agg_cfg_v3
        let time = if self.usb3 { 1 } else { 32 };
        let agg = B_BE_RXAGG_0_EN | put(0, B_BE_RXAGG_0_NUM_TH, 255) | put(0, B_BE_RXAGG_0_BUF_SZ_1K, 20) | put(0, B_BE_RXAGG_0_TIME_32US_TH, time);
        self.w32(R_BE_RXAGG_0_V1, agg)?;
        self.w32(R_BE_RXAGG_1_V1, 0x1f)?;
        // rtw89_fw_h2c_set_ofld_cfg
        self.h2c(
            H2C_CAT_MAC,
            H2C_CL_MAC_FW_OFLD,
            H2C_FUNC_OFLD_CFG,
            false,
            true,
            &[0x09, 0, 0, 0, 0x5e, 0, 0, 0],
        )?;
        Ok(())
    }

    /// dmac_init_be
    fn dmac_init(&self) -> io::Result<()> {
        self.dle_init_scc()?;
        // preload_init_be (8922A, band 0)
        let mut v = self.r32(R_BE_TXPKTCTL_B0_PRELD_CFG1)?;
        v = put(v, B_BE_B0_PRELD_NXT_TXENDWIN_MASK, PRELD_NEXT_WND);
        v = put(v, B_BE_B0_PRELD_NXT_RSVMINSZ_MASK, PRELD_NEXT_MIN_SIZE);
        self.w32(R_BE_TXPKTCTL_B0_PRELD_CFG1, v)?;
        let mut v = self.r32(R_BE_TXPKTCTL_B0_PRELD_CFG0)?;
        v = put(v, B_BE_B0_PRELD_CAM_G0ENTNUM_MASK, PRELD_B0_ACQ_ENT_NUM_8922A);
        v = put(v, B_BE_B0_PRELD_CAM_G1ENTNUM_MASK, PRELD_MISCQ_ENT_NUM_8922A);
        v = put(v, B_BE_B0_PRELD_USEMAXSZ_MASK, PRELD_B0_ENT_NUM * PRELD_AMSDU_SIZE);
        self.w32(R_BE_TXPKTCTL_B0_PRELD_CFG0, v | B_BE_B0_PRELD_FEN)?;
        self.hfc_init_full()?;
        // sta_sch_init_be
        self.set8(R_BE_SS_CTRL, B_BE_SS_EN)?;
        self.poll(R_BE_SS_CTRL, 100, "STA scheduler", |v| v & B_BE_SS_INIT_DONE != 0)?;
        self.set32(R_BE_SS_CTRL, B_BE_WARM_INIT)?;
        self.clr32(R_BE_SS_CTRL, B_BE_BAND_TRIG_EN | B_BE_BAND1_TRIG_EN)?;
        // mpdu_proc_init_be
        self.set32(R_BE_MPDU_PROC, B_BE_APPEND_FCS)?;
        self.w32(R_BE_CUT_AMSDU_CTRL, TRXCFG_MPDU_PROC_CUT_CTRL | B_BE_CA_CHK_ADDRCAM_EN)?;
        let mut v = self.r32(R_BE_HDR_SHCUT_SETTING)?;
        v |= B_BE_TX_HW_SEQ_EN | B_BE_TX_HW_ACK_POLICY_EN | B_BE_TX_MAC_MPDU_PROC_EN;
        v &= !B_BE_TX_ADDR_MLD_TO_LIK;
        self.set32(R_BE_HDR_SHCUT_SETTING, v)?;
        self.w32(R_BE_RX_HDRTRNS, TRXCFG_MPDU_PROC_RX_HDR_CONV | B_BE_HC_ADDR_HIT_EN)?;
        let mut v = self.r32(R_BE_DISP_FWD_WLAN_0)?;
        for m in [
            B_BE_FWD_WLAN_CPU_TYPE_0_DATA_MASK,
            B_BE_FWD_WLAN_CPU_TYPE_0_MNG_MASK,
            B_BE_FWD_WLAN_CPU_TYPE_0_CTL_MASK,
            B_BE_FWD_WLAN_CPU_TYPE_1_MASK,
        ] {
            v = put(v, m, 1);
        }
        self.w32(R_BE_DISP_FWD_WLAN_0, v)?;
        // sec_eng_init_be
        let v = self.r32(R_BE_SEC_ENG_CTRL)?
            | B_BE_CLK_EN_CGCMP
            | B_BE_CLK_EN_WAPI
            | B_BE_CLK_EN_WEP_TKIP
            | B_BE_SEC_TX_ENC
            | B_BE_SEC_RX_DEC
            | B_BE_MC_DEC
            | B_BE_BC_DEC
            | B_BE_BMC_MGNT_DEC
            | B_BE_UC_MGNT_DEC
            | B_BE_SEC_PRE_ENQUE_TX;
        self.w32(R_BE_SEC_ENG_CTRL, v)?;
        self.set32(R_BE_SEC_MPDU_PROC, B_BE_APPEND_ICV | B_BE_APPEND_MIC)?;
        // txpktctrl_init_be: MPDU 정보 표는 PLE 빈 쪽 바로 뒤.
        let mut v = self.r32(R_BE_TXPKTCTL_MPDUINFO_CFG)?;
        v = put(v, B_BE_MPDUINFO_PKTID_MASK, PLE_LNK_PGE);
        v = put(v, B_BE_MPDUINFO_B1_BADDR_MASK, MPDU_INFO_B1_OFST);
        self.w32(R_BE_TXPKTCTL_MPDUINFO_CFG, v | B_BE_MPDUINFO_FEN)?;
        // mlo_init_be
        let v = self.r32(R_BE_MLO_INIT_CTL)?;
        self.w32(R_BE_MLO_INIT_CTL, v | B_BE_MLO_TABLE_REINIT)?;
        self.w32(R_BE_MLO_INIT_CTL, v & !B_BE_MLO_TABLE_REINIT)?;
        self.poll(R_BE_MLO_INIT_CTL, 10, "MLO table", |v| v & B_BE_MLO_TABLE_INIT_DONE != 0)?;
        self.set32(R_BE_SS_CTRL, B_BE_MLO_HW_CHGLINK_EN)?;
        self.set32(R_BE_CMAC_SHARE_ACQCHK_CFG_0, B_BE_R_MACID_ACQ_CHK_EN)?;
        Ok(())
    }

    /// rtw89_mac_dle_init(SCC)
    fn dle_init_scc(&self) -> io::Result<()> {
        self.clr32(R_BE_DMAC_FUNC_EN, B_BE_DLE_WDE_EN | B_BE_DLE_PLE_EN)?;
        self.set32(R_BE_DMAC_CLK_EN, B_BE_DLE_WDE_CLK_EN | B_BE_DLE_PLE_CLK_EN)?;
        let mut v = self.r32(R_BE_WDE_PKTBUF_CFG)?;
        v = put(v, B_BE_WDE_PAGE_SEL_MASK, S_AX_WDE_PAGE_SEL_64);
        v = put(v, B_BE_WDE_START_BOUND_MASK, 0);
        v = put(v, B_BE_WDE_FREE_PAGE_NUM_MASK, WDE_LNK_PGE);
        self.w32(R_BE_WDE_PKTBUF_CFG, v)?;
        let mut v = self.r32(R_BE_PLE_PKTBUF_CFG)?;
        v = put(v, B_BE_PLE_PAGE_SEL_MASK, S_AX_PLE_PAGE_SEL_256);
        v = put(v, B_BE_PLE_START_BOUND_MASK, PLE_SRT_OFST / DLE_BOUND_UNIT);
        v = put(v, B_BE_PLE_FREE_PAGE_NUM_MASK, PLE_LNK_PGE);
        self.w32(R_BE_PLE_PKTBUF_CFG, v)?;
        // wde_quota_cfg_be: Q0 hif, Q1 wcpu, Q2 0, Q3 pkt_in, Q4 cpu_io
        let wde = [WDE_QT[0], WDE_QT[1], 0, WDE_QT[2], WDE_QT[3]];
        for (i, q) in wde.into_iter().enumerate() {
            self.w32(
                R_BE_WDE_QTA0_CFG + 4 * i as u32,
                put(0, B_BE_WDE_Q0_MIN_SIZE_MASK, q) | put(0, B_BE_WDE_Q0_MAX_SIZE_MASK, q),
            )?;
        }
        for i in 0..13 {
            self.w32(
                R_BE_PLE_QTA0_CFG + 4 * i as u32,
                put(0, B_BE_PLE_Q0_MIN_SIZE_MASK, PLE_MIN[i]) | put(0, B_BE_PLE_Q0_MAX_SIZE_MASK, PLE_MAX[i]),
            )?;
        }
        self.set32(R_BE_DMAC_FUNC_EN, B_BE_DLE_WDE_EN | B_BE_DLE_PLE_EN)?;
        self.poll(R_AX_WDE_INI_STATUS, 50, "WDE ready", |v| v & WDE_MGN_INI_RDY == WDE_MGN_INI_RDY)?;
        self.poll(R_AX_PLE_INI_STATUS, 50, "PLE ready", |v| v & PLE_MGN_INI_RDY == PLE_MGN_INI_RDY)?;
        Ok(())
    }

    /// rtw89_mac_hfc_init(reset, en, h2c_en)
    fn hfc_init_full(&self) -> io::Result<()> {
        self.clr32(R_BE_HCI_FC_CTRL, B_BE_HCI_FC_EN | B_BE_HCI_FC_CH12_EN)?;
        for ch in 0..12u32 {
            let (min, max) = if ch % 2 == 0 { (HFC_CH_MIN, HFC_CH_MAX) } else { (0, 0) };
            self.w32(R_BE_CH0_PAGE_CTRL + ch * 4, put(0, B_AX_MIN_PG_MASK, min) | put(0, B_AX_MAX_PG_MASK, max))?;
        }
        self.w32(
            R_BE_PUB_PAGE_CTRL1,
            put(0, B_AX_PUBPG_G0_MASK, HFC_PUB_G0) | put(0, B_AX_PUBPG_G1_MASK, 0),
        )?;
        self.w32(R_BE_WP_PAGE_CTRL2, put(0, B_AX_WP_THRD_MASK, 0))?;
        // hfc_mix_cfg_be
        self.w32(
            R_BE_CH_PAGE_CTRL,
            put(0, B_BE_PREC_PAGE_CH011_V1_MASK, HFC_CH011_PREC) | put(0, B_BE_PREC_PAGE_CH12_V1_MASK, HFC_H2C_PREC),
        )?;
        self.w32(R_BE_PUB_PAGE_CTRL2, put(0, B_BE_PUBPG_ALL_MASK, HFC_PUB_MAX))?;
        self.w32(
            R_BE_WP_PAGE_CTRL1,
            put(0, B_BE_PREC_PAGE_WP_CH07_MASK, HFC_WP_PREC) | put(0, B_BE_PREC_PAGE_WP_CH811_MASK, HFC_WP_PREC),
        )?;
        let mut v = self.r32(R_BE_HCI_FC_CTRL)?;
        v = put(v, B_BE_HCI_FC_MODE_MASK, HFC_MODE_STF);
        for m in [
            B_BE_HCI_FC_WD_FULL_COND_MASK,
            B_BE_HCI_FC_CH12_FULL_COND_MASK,
            B_BE_HCI_FC_WP_CH07_FULL_COND_MASK,
            B_BE_HCI_FC_WP_CH811_FULL_COND_MASK,
        ] {
            v = put(v, m, 1);
        }
        self.w32(R_BE_HCI_FC_CTRL, v)?;
        self.set32(R_BE_HCI_FC_CTRL, B_BE_HCI_FC_EN | B_BE_HCI_FC_CH12_EN)?;
        sleep(Duration::from_micros(10));
        // hfc_upd_mix_info / hfc_pub_info_chk
        let info1 = self.r32(R_BE_PUB_PAGE_INFO1)?;
        let used = get(info1, B_BE_G0_USE_PG_MASK) + get(info1, B_BE_G1_USE_PG_MASK);
        let aval = get(self.r32(R_BE_PUB_PAGE_INFO2)?, B_BE_PUB_AVAL_PG_MASK);
        if used + aval != HFC_PUB_MAX {
            return Err(io::Error::other(format!(
                "HCI flow control: used {used} + available {aval} != {HFC_PUB_MAX}"
            )));
        }
        Ok(())
    }

    /// cmac_init_be(0)
    fn cmac_init(&self) -> io::Result<()> {
        // scheduler_init_be
        self.set32(R_BE_MISC_1, B_BE_EN_TX_FINISH_PRD_RESP)?;
        self.w32(
            R_BE_HE_CTN_CHK_CCA_NAV,
            B_BE_HE_CTN_CHK_CCA_P20
                | B_BE_HE_CTN_CHK_EDCCA_P20
                | B_BE_HE_CTN_CHK_CCA_BITMAP
                | B_BE_HE_CTN_CHK_EDCCA_BITMAP
                | B_BE_HE_CTN_CHK_NO_GNT_WL
                | B_BE_HE_CTN_CHK_BASIC_NAV
                | B_BE_HE_CTN_CHK_INTRA_NAV
                | B_BE_HE_CTN_CHK_TX_NAV,
        )?;
        self.w32(
            R_BE_HE_SIFS_CHK_CCA_NAV,
            B_BE_HE_SIFS_CHK_EDCCA_P20 | B_BE_HE_SIFS_CHK_EDCCA_BITMAP | B_BE_HE_SIFS_CHK_NO_GNT_WL,
        )?;
        self.w32(
            R_BE_TB_CHK_CCA_NAV,
            B_BE_TB_CHK_EDCCA_BITMAP | B_BE_TB_CHK_NO_GNT_WL | B_BE_TB_CHK_BASIC_NAV,
        )?;
        self.clr32(R_BE_CCA_CFG_0, B_BE_NO_GNT_WL_EN)?;
        self.w32_mask(R_BE_EDCA_BCNQ_PARAM, B_BE_BCNQ_CW_MASK, 0x32)?;
        self.w32_mask(R_BE_EDCA_BCNQ_PARAM, B_BE_BCNQ_AIFS_MASK, BCN_IFS_25US)?;
        // addr_cam_init_be
        let mut v = self.r32(R_BE_ADDR_CAM_CTRL)?;
        v = put(v, B_BE_ADDR_CAM_RANGE_MASK, ADDR_CAM_SERCH_RANGE) | B_BE_ADDR_CAM_EN | B_BE_ADDR_CAM_CLR;
        self.w32(R_BE_ADDR_CAM_CTRL, v)?;
        self.poll(R_BE_ADDR_CAM_CTRL, 100, "address CAM reset", |v| v & B_BE_ADDR_CAM_CLR == 0)?;
        // rx_fltr_init_be: 관리/제어/데이터 프레임 모두 호스트로.
        self.w32(R_BE_MGNT_FLTR, RX_FLTR_FRAME_ACCEPT_BE)?;
        self.w32(R_BE_CTRL_FLTR, RX_FLTR_FRAME_ACCEPT_BE)?;
        self.w32(R_BE_DATA_FLTR, RX_FLTR_FRAME_ACCEPT_BE)?;
        self.w32(R_BE_RX_FLTR_OPT, default_rx_fltr())?;
        let plcp = B_BE_HE_SIGB_CRC_CHK
            | B_BE_VHT_MU_SIGB_CRC_CHK
            | B_BE_VHT_SU_SIGB_CRC_CHK
            | B_BE_SIGA_CRC_CHK
            | B_BE_LSIG_PARITY_CHK_EN
            | B_BE_CCK_SIG_CHK
            | B_BE_CCK_CRC_CHK;
        self.w16(R_BE_PLCP_HDR_FLTR, plcp as u16)?;
        // nav_ctrl_init_be
        let mut v = self.r32(R_BE_WMAC_NAV_CTL)?;
        v &= !B_BE_WMAC_PLCP_UP_NAV_EN;
        v |= B_BE_WMAC_TF_UP_NAV_EN | B_BE_WMAC_NAV_UPPER_EN;
        v = put(v, B_BE_WMAC_NAV_UPPER_MASK, NAV_25MS);
        self.w32(R_BE_WMAC_NAV_CTL, v)?;
        self.clr32(R_BE_SPECIAL_TX_SETTING, B_BE_BMC_NAV_PROTECT)?;
        self.set32(R_BE_TRXPTCL_RESP_0, B_BE_WMAC_MBA_DUR_FORCE)?;
        // spatial_reuse_init_be
        self.clr8(R_BE_RX_SR_CTRL, B_BE_SR_EN | B_BE_SR_CTRL_PLCP_EN)?;
        self.set8(R_BE_BSSID_SRC_CTRL, B_BE_PLCP_SRC_EN)?;
        // tmac_init_be
        self.clr32(R_BE_TB_PPDU_CTRL, B_BE_QOSNULL_UPD_MUEDCA_EN)?;
        self.w32_mask(R_BE_WMTX_TCR_BE_4, B_BE_EHT_HE_PPDU_4XLTF_ZLD_USTIMER_MASK, 0x12)?;
        self.w32_mask(R_BE_WMTX_TCR_BE_4, B_BE_EHT_HE_PPDU_2XLTF_ZLD_USTIMER_MASK, 0xe)?;
        // trxptcl_init_be
        let mut v = self.r32(R_BE_MAC_LOOPBACK)?;
        v = put(v, B_BE_MACLBK_PLCP_DLY_MASK, S_BE_MACLBK_PLCP_DLY_DEF) & !B_BE_MACLBK_EN;
        self.w32(R_BE_MAC_LOOPBACK, v)?;
        self.set32(R_BE_CMAC_FUNC_EN, B_BE_PHYINTF_EN)?;
        let mut v = self.r32(R_BE_TRXPTCL_RESP_0)?;
        v = put(v, B_BE_WMAC_SPEC_SIFS_CCK_MASK, WMAC_SPEC_SIFS_CCK);
        v = put(v, B_BE_WMAC_SPEC_SIFS_OFDM_MASK, WMAC_SPEC_SIFS_OFDM_1115E);
        self.w32(R_BE_TRXPTCL_RESP_0, v)?;
        self.clr32(R_BE_WMAC_ACK_BA_RESP_LEGACY, B_BE_ACK_BA_RESP_LEGACY_CHK_EDCCA)?;
        self.clr32(R_BE_WMAC_ACK_BA_RESP_HE, B_BE_ACK_BA_RESP_HE_CHK_EDCCA)?;
        self.clr32(R_BE_WMAC_ACK_BA_RESP_EHT_LEG_PUNC, B_BE_ACK_BA_EHT_LEG_PUNC_CHK_EDCCA)?;
        self.set32(R_BE_RXTRIG_TEST_USER_2, B_BE_RXTRIG_FCSCHK_EN)?;
        let v = self.r32(R_BE_TRXPTCL_RESP_1)?
            & (B_BE_FTM_RRSR_RATE_EN_MASK | B_BE_WMAC_RESP_DOPPLEB_BE_EN | B_BE_WMAC_RESP_DCM_EN | B_BE_WMAC_RESP_REF_RATE_MASK);
        self.w32(R_BE_TRXPTCL_RESP_1, v)?;
        self.w32_mask(R_BE_TRXPTCL_RESP_1, B_BE_WMAC_RESP_REF_RATE_SEL, 0)?;
        let v = self.r32(R_BE_PTCL_RRSR1)? & (B_BE_RRSR_RATE_EN_MASK | B_BE_RRSR_CCK_MASK | B_BE_RSC_MASK);
        self.w32(R_BE_PTCL_RRSR1, v)?;
        let v = self.r32(R_BE_PTCL_RRSR0)? & (B_BE_RRSR_OFDM_MASK | B_BE_RRSR_HT_MASK | B_BE_RRSR_VHT_MASK | B_BE_RRSR_HE_MASK);
        self.w32(R_BE_PTCL_RRSR0, v)?;
        // rmac_init_be: Block Ack CAM을 비우고 RX 최대 길이.
        self.w32_mask(R_BE_RESPBA_CAM_CTRL, B_BE_BACAM_RST_MASK, S_BE_BACAM_RST_ALL)?;
        self.poll(R_BE_RESPBA_CAM_CTRL, 10, "BA CAM reset", |v| {
            get(v, B_BE_BACAM_RST_MASK) == S_BE_BACAM_RST_DONE
        })?;
        let mut v = self.r16(R_BE_DLK_PROTECT_CTL)? as u32;
        v = put(v, B_BE_RX_DLK_DATA_TIME_MASK, TRXCFG_RMAC_DATA_TO);
        v = put(v, B_BE_RX_DLK_CCA_TIME_MASK, TRXCFG_RMAC_CCA_TO);
        self.w16(R_BE_DLK_PROTECT_CTL, (v | B_BE_RX_DLK_RST_EN) as u16)?;
        let rx_max_len = (PLE_MIN[6].min(PLD_RLS_MAX_PG) * PLE_PG_SIZE).min(RX_SPEC_MAX_LEN) / RX_MAX_LEN_UNIT;
        self.w32_mask(R_BE_RX_FLTR_OPT, B_BE_RX_MPDU_MAX_LEN_MASK, rx_max_len)?;
        self.clr8(R_BE_PLCP_HDR_FLTR, B_BE_VHT_SU_SIGB_CRC_CHK)?;
        self.set16(R_BE_RCR, B_BE_BUSY_CHKSN)?;
        self.set16(R_BE_RX_PLCP_EXT_OPTION_1, B_BE_PLCP_SU_PSDU_LEN_SRC)?;
        // resp_pktctl_init_be: 대역 0 CSI 예약 쪽.
        self.w32_mask(
            R_BE_RESP_CSI_RESERVED_PAGE,
            B_BE_CSI_RESERVED_START_PAGE_MASK,
            PLE_LNK_PGE + RSVD_MPDU_INFO_TBL,
        )?;
        self.w32_mask(R_BE_RESP_CSI_RESERVED_PAGE, B_BE_CSI_RESERVED_PAGE_NUM_MASK, RSVD_B0_CSI + 1)?;
        // cmac_com_init_be(0)
        let mut v = self.r32(R_BE_TX_SUB_BAND_VALUE)?;
        v = put(v, B_BE_TXSB_20M_MASK, S_BE_TXSB_20M_8);
        v = put(v, B_BE_TXSB_40M_MASK, S_BE_TXSB_40M_4);
        v = put(v, B_BE_TXSB_80M_MASK, S_BE_TXSB_80M_2);
        v = put(v, B_BE_TXSB_160M_MASK, S_BE_TXSB_160M_1);
        self.w32(R_BE_TX_SUB_BAND_VALUE, v)?;
        // ptcl_init_be (USB: CTS2SELF/preload 설정 없음)
        let mut v = self.r8(R_BE_PTCL_COMMON_SETTING_0)? as u32;
        v |= B_BE_CMAC_TX_MODE_0 | B_BE_CMAC_TX_MODE_1;
        v &= !(B_BE_PTCL_TRIGGER_SS_EN_0 | B_BE_PTCL_TRIGGER_SS_EN_1 | B_BE_PTCL_TRIGGER_SS_EN_UL);
        self.w8(R_BE_PTCL_COMMON_SETTING_0, v as u8)?;
        self.w32_mask(R_BE_AMPDU_AGG_LIMIT, B_BE_AMPDU_MAX_TIME_MASK, AMPDU_MAX_TIME)?;
        // cmac_dma_init_be
        let mut v = self.r32(R_BE_RX_CTRL_1)?;
        v = put(v, B_BE_RXDMA_TXRPT_QUEUE_ID_SW_MASK, WLCPU_RXCH2_QID);
        v = put(v, B_BE_RXDMA_F2PCMDRPT_QUEUE_ID_SW_MASK, WLCPU_RXCH2_QID);
        self.w32(R_BE_RX_CTRL_1, v)?;
        Ok(())
    }
}
