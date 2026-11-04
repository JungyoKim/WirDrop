//! USB 벌크 IN으로 오는 묶음을 패킷으로 나눕니다 (rtw89_usb_rx_handler, rtw89_core_query_rxdesc_v2).
//! 한 번의 읽기에 여러 패킷이 16바이트 정렬로 이어지고, 패킷마다 24바이트(짧은) 또는 40바이트(긴)
//! 서술자가 앞에 붙습니다.

/// 패킷 종류 (rtw89_core_rx_type).
pub const RX_TYPE_WIFI: u8 = 0;
pub const RX_TYPE_PPDU_STAT: u8 = 1;
pub const RX_TYPE_C2H: u8 = 10;

const AGG_ALIGN: usize = 16;
const RXD_SHORT: usize = 24;
const RXD_LONG: usize = 40;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn bits(v: u32, hi: u32, lo: u32) -> u32 {
    (v >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}

/// RX 서술자에서 쓰는 값들.
#[derive(Debug, Clone, Copy, Default)]
pub struct RxDesc {
    pub kind: u8,
    pub crc_err: bool,
    pub icv_err: bool,
    pub a1_match: bool,
    pub ampdu: bool,
    /// 802.11 프레임 종류 (0 관리, 1 제어, 2 데이터).
    pub frame_type: u8,
    /// 0 = 20 MHz, 1 = 40, 2 = 80, 3 = 160, 4 = 320.
    pub bw: u8,
    /// rtw89 속도 부호 (BE_RXD_RX_DATARATE: 위 비트가 방식, 아래가 MCS/NSS).
    pub rate: u16,
    pub gi_ltf: u8,
    pub ppdu_type: u8,
    pub ppdu_cnt: u8,
    pub bb_sel: bool,
    pub free_run: u32,
}

/// 묶음 하나의 패킷들을 차례로 f(서술자, 내용)에 넘깁니다. 잘린 끝은 버립니다.
pub fn packets(buf: &[u8], mut f: impl FnMut(&RxDesc, &[u8])) {
    let mut at = 0;
    while at + RXD_SHORT <= buf.len() {
        let d = &buf[at..];
        let w0 = le32(d, 0);
        let w2 = le32(d, 8);
        let w3 = le32(d, 12);
        let w4 = le32(d, 16);
        let len = bits(w0, 13, 0) as usize;
        let shift = bits(w0, 15, 14) as usize * 2;
        let drv_info = bits(w0, 19, 18) as usize * 8;
        let hdr_cnv = bits(w0, 21, 20) as usize * 16;
        let phy_rpt = bits(w0, 23, 22) as usize * 8;
        let rxd = if w0 >> 31 != 0 { RXD_LONG } else { RXD_SHORT };
        let start = rxd + shift + drv_info + phy_rpt + hdr_cnv;
        if start + len > d.len() {
            return;
        }
        let desc = RxDesc {
            kind: bits(w0, 29, 24) as u8,
            bb_sel: w0 & (1 << 30) != 0,
            frame_type: bits(w2, 11, 10) as u8,
            crc_err: w3 & (1 << 6) != 0,
            icv_err: w3 & (1 << 5) != 0,
            a1_match: w3 & (1 << 10) != 0,
            ampdu: w3 & (1 << 11) != 0,
            ppdu_type: bits(w4, 7, 0) as u8,
            ppdu_cnt: bits(w4, 10, 8) as u8,
            bw: bits(w4, 14, 12) as u8,
            gi_ltf: bits(w4, 18, 16) as u8,
            rate: bits(w4, 31, 20) as u16,
            free_run: le32(d, 20),
        };
        f(&desc, &d[start..start + len]);
        at += (start + len).next_multiple_of(AGG_ALIGN);
    }
}

/// 펌웨어 메시지 (C2H) 머리말: 분류, 묶음, 기능, 내용.
#[derive(Debug)]
pub struct C2h<'a> {
    pub cat: u8,
    pub class: u8,
    pub func: u8,
    pub payload: &'a [u8],
}

pub fn c2h(data: &[u8]) -> Option<C2h<'_>> {
    if data.len() < 8 {
        return None;
    }
    let w0 = le32(data, 0);
    let len = (bits(le32(data, 4), 13, 0) as usize).clamp(8, data.len());
    Some(C2h {
        cat: bits(w0, 1, 0) as u8,
        class: bits(w0, 7, 2) as u8,
        func: bits(w0, 15, 8) as u8,
        payload: &data[8..len],
    })
}
