//! RTL8922A 계열(RTL8912AU 포함) 펌웨어 파일: linux-firmware의 rtw8922a_fw-4.bin. 여러 펌웨어를 담은
//! MFW 머리말, 펌웨어마다 v1 머리말과 구간들, 그 뒤에 요소들(BB MCU 펌웨어, BB/RF 레지스터 표, 송신
//! 출력 표)이 이어집니다. rtw89의 fw.c에서 옮겼습니다 (BSD-3-Clause, NOTICE.md).
use std::io;

/// MFW 안의 펌웨어 종류 (rtw89_fw_type).
pub const TYPE_NORMAL: u8 = 1;

/// 요소 번호 (rtw89_fw_element_id).
pub const ELM_BBMCU0: u32 = 0;
pub const ELM_BB_REG: u32 = 2;
pub const ELM_BB_GAIN: u32 = 3;
pub const ELM_RADIO_A: u32 = 4;
pub const ELM_RADIO_B: u32 = 5;
pub const ELM_RF_NCTL: u32 = 8;

const MFW_SIG: u8 = 0xff;
const ELEMENT_ALIGN: usize = 16;
const ELEMENT_HDR: usize = 32;
const SECTION_CHKSUM_LEN: usize = 8;
const SECURITY_SECTION: u8 = 9;
const FORMATTED_MSSC: u8 = 0xff;
const SECURITY_SIGLEN: usize = 512;
const SECURITY_CHKSUM_LEN: usize = 8;
const MSS_SIGNATURE: &[u8; 8] = b"MSSKPOOL";
const MSS_POOL_HDR: usize = 32;
const MSS_POOL_DEFKEYSETS_SIZE: usize = 8;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("firmware: {what}"))
}

/// MFW의 펌웨어 하나 (rtw89_mfw_info).
struct MfwInfo {
    cv: u8,
    kind: u8,
    mp: u8,
    shift: usize,
    size: usize,
}

fn mfw(file: &[u8]) -> io::Result<Vec<MfwInfo>> {
    if file.len() < 16 || file[0] != MFW_SIG {
        return Err(bad("not an MFW file"));
    }
    let n = file[1] as usize;
    if n == 0 || 16 + n * 16 > file.len() {
        return Err(bad("bad MFW header"));
    }
    let infos: Vec<MfwInfo> = (0..n)
        .map(|i| {
            let o = 16 + i * 16;
            MfwInfo {
                cv: file[o],
                kind: file[o + 1],
                mp: file[o + 2],
                shift: le32(file, o + 4) as usize,
                size: le32(file, o + 8) as usize,
            }
        })
        .collect();
    if infos.iter().any(|f| f.shift + f.size > file.len()) {
        return Err(bad("MFW entry out of the file"));
    }
    Ok(infos)
}

/// 칩 버전 cv에 맞는 펌웨어: 같거나 작은 것 중 가장 가까운 것 (rtw89_mfw_recognize).
pub fn suit(file: &[u8], kind: u8, cv: u8) -> io::Result<&[u8]> {
    let best = mfw(file)?
        .into_iter()
        .filter(|f| f.kind == kind && f.cv <= cv && f.mp == 0)
        .max_by_key(|f| f.cv)
        .ok_or_else(|| bad(&format!("no type {kind} firmware for cv {cv}")))?;
    Ok(&file[best.shift..best.shift + best.size])
}

/// 펌웨어 뒤의 요소 하나 (rtw89_fw_element_hdr). head는 머리말 끝의 8바이트(요소마다 뜻이 다름).
pub struct Element<'a> {
    pub id: u32,
    pub ver: [u8; 4],
    /// 이 요소가 맞는 A-die (0이면 모두; rtw89_hal::aid).
    pub aid: u16,
    pub head: [u8; 8],
    pub body: &'a [u8],
}

pub fn elements(file: &[u8]) -> io::Result<Vec<Element<'_>>> {
    let end = mfw(file)?.iter().map(|f| f.shift + f.size).max().unwrap_or(0);
    let mut o = end.next_multiple_of(ELEMENT_ALIGN);
    let mut out = Vec::new();
    while o + ELEMENT_HDR < file.len() {
        let size = le32(file, o + 4) as usize;
        if o + ELEMENT_HDR + size > file.len() {
            break;
        }
        out.push(Element {
            id: le32(file, o),
            ver: file[o + 8..o + 12].try_into().unwrap(),
            aid: u16::from_le_bytes([file[o + 12], file[o + 13]]),
            head: file[o + 24..o + 32].try_into().unwrap(),
            body: &file[o + ELEMENT_HDR..o + ELEMENT_HDR + size],
        });
        o = (o + ELEMENT_HDR + size).next_multiple_of(ELEMENT_ALIGN);
    }
    Ok(out)
}

/// BB MCU 펌웨어: 칩 cv와 같거나 작은 첫 BBMCU0 요소 (파일에는 cv가 줄어드는 순서로 있습니다).
pub fn bbmcu(file: &[u8], cv: u8) -> io::Result<&[u8]> {
    elements(file)?
        .into_iter()
        .find(|e| e.id == ELM_BBMCU0 && e.head[0] <= cv)
        .map(|e| e.body)
        .ok_or_else(|| bad(&format!("no BB MCU firmware for cv {cv}")))
}

/// 보안 부팅 칩의 서명 키 선택 (eFuse에서; rtw89_fw_secure).
#[derive(Debug, Clone, Copy, Default)]
pub struct Secure {
    pub sb_sel_mgn: u32,
    pub dev_type: u8,
    pub cust_idx: u8,
    pub key_num: u8,
}

/// 내려받을 구간 하나.
pub struct Section<'a> {
    pub kind: u8,
    pub dl_addr: u32,
    /// 보낼 내용 (체크섬 포함, 보안 풀 제외).
    pub data: &'a [u8],
    /// 보안 부팅 칩이면 마지막 조각 끝에 덮어쓸 서명.
    pub key: Option<&'a [u8]>,
}

/// 해석한 펌웨어 하나: 보낼 머리말(무시할 구간을 뺀 것), 조각 크기, 구간들.
pub struct Image<'a> {
    pub version: [u8; 4],
    pub header: Vec<u8>,
    pub part_size: usize,
    pub idmem_share_mode: u8,
    pub sections: Vec<Section<'a>>,
}

/// v1 머리말의 펌웨어를 해석합니다 (rtw89_fw_hdr_parser_v1, __parse_security_section,
/// __parse_formatted_mssc, __rtw89_fw_download_tweak_hdr_v1).
pub fn parse<'a>(fw: &'a [u8], secure: Option<&Secure>) -> io::Result<Image<'a>> {
    if fw.len() < 48 {
        return Err(bad("short firmware"));
    }
    let w = |i: usize| le32(fw, i * 4);
    if (w(3) >> 24) & 0xff != 1 {
        return Err(bad("not a v1 header"));
    }
    let version = w(1).to_le_bytes();
    let section_num = ((w(6) >> 8) & 0xff) as usize;
    let dsp_checksum = (w(6) >> 24) & 1 == 1;
    let base_hdr_len = 48 + 16 * section_num;
    let dynamic = (w(7) >> 16) & 1 == 1;
    let idmem_share_mode = ((w(7) >> 18) & 0xf) as u8;
    let part_size = (w(7) & 0xffff) as usize;
    let hdr_len = if dynamic { ((w(5) >> 16) & 0xffff) as usize } else { base_hdr_len };
    if hdr_len < base_hdr_len || hdr_len > fw.len() || part_size == 0 {
        return Err(bad("bad header length"));
    }
    if dynamic && le32(fw, base_hdr_len) as usize != hdr_len - base_hdr_len {
        return Err(bad("bad dynamic header length"));
    }

    let mut bin = hdr_len;
    let mut sections = Vec::new();
    let mut kept = Vec::new();
    let mut secure_section_exist = false;
    for i in 0..section_num {
        let s = 48 + 16 * i;
        let w0 = le32(fw, s);
        let w1 = le32(fw, s + 4);
        let w2 = le32(fw, s + 8);
        let kind = ((w1 >> 24) & 0xf) as u8;
        let mut len = (w1 & 0xff_ffff) as usize;
        if (w1 >> 28) & 1 == 1 {
            len += SECTION_CHKSUM_LEN;
        }
        if bin + len > fw.len() {
            return Err(bad("section out of the file"));
        }
        let content = &fw[bin..];
        let mut ignore = false;
        let mut key = None;
        let mut mssc_len = 0;
        if kind == SECURITY_SECTION {
            let mssc = (w2 & 0xff) as u8;
            if mssc == FORMATTED_MSSC {
                let pool = &content[len..];
                if pool.len() < MSS_POOL_HDR || &pool[..8] != MSS_SIGNATURE {
                    return Err(bad("wrong MSS signature"));
                }
                let key_raw_offset = le32(pool, 12) as usize;
                let defen = pool[16] != 0;
                let rmpfmt = pool[20];
                let dev_max = pool[21] as usize;
                let keypair_num = le16(pool, 22) as usize;
                let cust_max = le16(pool, 24) as usize;
                let key_num_max = le16(pool, 26) as usize;
                if rmpfmt != 0 {
                    return Err(bad("unsupported MSS remap table"));
                }
                let mut rmp_tbl_size = (key_num_max * cust_max * dev_max) >> 3;
                if defen {
                    rmp_tbl_size += MSS_POOL_DEFKEYSETS_SIZE;
                }
                if rmp_tbl_size + MSS_POOL_HDR != key_raw_offset {
                    return Err(bad("MSS key pool format"));
                }
                let mut key_sign_len = (le16(content, 60) >> 2) as usize;
                if key_sign_len == 0 {
                    key_sign_len = 512;
                }
                if dsp_checksum {
                    key_sign_len += SECURITY_CHKSUM_LEN;
                }
                mssc_len = MSS_POOL_HDR + rmp_tbl_size + keypair_num * key_sign_len;
                match secure {
                    None => ignore = secure_section_exist,
                    Some(sec) => {
                        let sb_sel_ver = le32(content, 58);
                        let idx = key_index(pool, sec, defen, rmp_tbl_size, key_num_max, cust_max);
                        match idx {
                            Some(idx) if sb_sel_ver == 0 || sb_sel_ver == sec.sb_sel_mgn => {
                                let at = len + key_raw_offset + key_sign_len * idx;
                                key = Some(&content[at..at + key_sign_len]);
                                ignore = secure_section_exist;
                            }
                            _ => ignore = true,
                        }
                    }
                }
            } else {
                mssc_len = mssc as usize * SECURITY_SIGLEN;
                if dsp_checksum {
                    mssc_len += mssc as usize * SECURITY_CHKSUM_LEN;
                }
                if secure.is_some() {
                    return Err(bad("unformatted MSS with secure boot is not supported"));
                }
            }
            if !ignore {
                secure_section_exist = true;
            }
        }
        if !ignore {
            kept.push(i);
            sections.push(Section {
                kind,
                dl_addr: w0,
                data: &content[..len],
                key,
            });
        }
        bin += len + mssc_len;
    }
    if bin != fw.len() {
        return Err(bad("sections don't add up to the firmware size"));
    }

    // 보낼 머리말: 동적 머리말 앞까지, 무시한 구간은 빼고 구간 수를 고칩니다.
    let mut header = fw[..48].to_vec();
    for &i in &kept {
        header.extend_from_slice(&fw[48 + 16 * i..64 + 16 * i]);
    }
    let w6 = (le32(&header, 24) & !0xff00) | ((kept.len() as u32) << 8);
    header[24..28].copy_from_slice(&w6.to_le_bytes());
    let w7 = (le32(&header, 28) & !0xffff) | part_size as u32;
    header[28..32].copy_from_slice(&w7.to_le_bytes());
    Ok(Image {
        version,
        header,
        part_size,
        idmem_share_mode,
        sections,
    })
}

/// 보안 부팅 칩이 쓸 서명의 번호 (__get_mssc_key_idx).
fn key_index(pool: &[u8], sec: &Secure, defen: bool, rmp_tbl_size: usize, key_num_max: usize, cust_max: usize) -> Option<usize> {
    const DEV_TYPE_FWSEC_DEF: u8 = 0xf;
    let mut sel = if sec.dev_type == DEV_TYPE_FWSEC_DEF {
        if !defen {
            return None;
        }
        sec.cust_idx as usize * key_num_max + sec.key_num as usize
    } else {
        let base = if defen { MSS_POOL_DEFKEYSETS_SIZE << 3 } else { 0 };
        base + sec.dev_type as usize * key_num_max * cust_max + sec.cust_idx as usize * key_num_max + sec.key_num as usize
    };
    let tbl = &pool[MSS_POOL_HDR..MSS_POOL_HDR + rmp_tbl_size];
    let (byte, bit) = (sel >> 3, sel & 7);
    if byte >= tbl.len() || tbl[byte] & (1 << bit) == 0 {
        return None;
    }
    sel = (tbl[byte] & ((1u8 << bit) - 1)).count_ones() as usize;
    Some(sel + tbl[..byte].iter().map(|b| b.count_ones() as usize).sum::<usize>())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &[u8] = include_bytes!("../../firmware/rtw8922a_fw-4.bin");

    #[test]
    fn normal_firmware_for_cut_c() {
        let fw = suit(FILE, TYPE_NORMAL, 2).unwrap();
        let img = parse(fw, None).unwrap();
        assert_eq!(img.version, [0, 35, 80, 3]);
        assert_eq!(img.part_size, 2020);
        // 두 번째 보안 구간은 보안 부팅이 아닌 칩에서 무시합니다.
        assert_eq!(img.sections.len(), 3);
        assert_eq!(img.header.len(), 48 + 3 * 16);
        assert_eq!((le32(&img.header, 24) >> 8) & 0xff, 3);
    }

    #[test]
    fn bb_mcu_firmware() {
        let fw = bbmcu(FILE, 2).unwrap();
        let img = parse(fw, None).unwrap();
        assert_eq!(img.sections.len(), 2);
        assert_eq!(img.sections[0].data.len(), 32768);
    }

    #[test]
    fn tables_are_present() {
        let ids: Vec<u32> = elements(FILE).unwrap().iter().map(|e| e.id).collect();
        for id in [ELM_BB_REG, ELM_BB_GAIN, ELM_RADIO_A, ELM_RADIO_B, ELM_RF_NCTL] {
            assert!(ids.contains(&id), "element {id}");
        }
    }
}
