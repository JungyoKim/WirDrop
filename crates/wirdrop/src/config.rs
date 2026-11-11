//! AWDL 노드 설정: 송신, ACK, Block Ack을 하드웨어로 하는 무선을 위한 owly-awdl [Config]. 값들은
//! 실제 iPhone으로 재서 가장 빨랐던 것입니다.
use std::io;

use owly_awdl::{Channel, Config, MacAddr, Presence, RadioConstraints, TxRate};

/// 5 GHz에서 알리는 VHT Capabilities 요소 본문: 80 MHz, 짧은 GI, 양방향 2 공간 스트림 MCS 0-9.
/// 이것이 없으면 iPhone은 5 GHz 채널에서 우리와 데이터를 주고받지 않습니다. 최대 MPDU 길이(첫
/// 바이트의 0-1비트)는 [VhtMaxMpdu]가 정합니다.
const VHT_CAPS: [u8; 12] = [0x30, 0x00, 0x80, 0x03, 0xfa, 0xff, 0x00, 0x00, 0xfa, 0xff, 0x00, 0x00];

/// 상대가 우리에게 보내도 된다고 알리는 가장 큰 MPDU. 클수록 A-MPDU의 MPDU마다 A-MSDU 서브프레임을
/// 더 담을 수 있습니다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhtMaxMpdu {
    B3895 = 0,
    B7991 = 1,
    B11454 = 2,
}

/// 처리량 설정. [Tuning::usb_adapter]의 기본값으로 79.75 MB 시험 영상을 iPhone에서 8.7초에
/// 받았습니다 (이 설정 없이는 22초).
#[derive(Debug, Clone)]
pub struct Tuning {
    /// 상대의 Block Ack 세션(ADDBA)을 받아들입니다. RTL8812BU처럼 우리 주소의 Block Ack을
    /// 하드웨어가 보내는 무선에서만 켭니다.
    pub accept_addba: bool,
    /// Block Ack 세션 안에서 A-MSDU를 허용해, 상대가 A-MPDU의 MPDU마다 패킷 여러 개를 담게 합니다.
    pub addba_amsdu: bool,
    /// 허락하는 가장 큰 Block Ack 창 (1-64).
    pub addba_window: u16,
    /// 한 상대에게 보내는 우리 패킷을 이 바이트까지의 프레임에 A-MSDU로 묶어 보냅니다.
    pub amsdu_max_frame: Option<usize>,
    pub vht_max_mpdu: VhtMaxMpdu,
    /// AWDL 채널을 우리 인프라 채널로 알립니다. 재 보니 더 느려서 끕니다.
    pub infra_channel: bool,
    /// 이보다 약하게 들리는(dBm) 상대는 받지 않습니다. None이면 모두 받습니다.
    pub rssi_threshold: Option<i8>,
    /// Data Path State TLV의 국가 코드.
    pub country: [u8; 2],
    /// 상대에게 Block Ack 세션을 청하고 A-MPDU로 보냅니다: 채널을 한 번 잡을 때 프레임 하나가
    /// 아니라 여럿. 묶어 보내고 Block Ack을 하드웨어로 받는 무선(usbfwd)이 필요합니다. 시험 영상을
    /// 보내는 데 켜면 4.9초, 끄면 14.1초 걸렸고 받기는 그대로입니다.
    pub tx_ampdu: bool,
    /// VHT 능력에 STBC 수신(1 스트림)을 알려, 안테나 2개인 상대가 한 스트림을 두 안테나로 보내
    /// 신호를 튼튼하게 하도록 합니다.
    pub rx_stbc: bool,
    /// VHT 능력에 SU 빔포밍 수신(시공간 스트림 2)을 알리고, 상대의 채널 측정에 무선이 하드웨어로
    /// 답하게 해(usbfwd USBFWD_BFEE) 안테나 2개인 상대가 우리 쪽으로 빔포밍하게 합니다.
    pub beamformee: bool,
    /// Data Path State TLV의 확장 플래그 (owly_awdl::tlv::ext_flag). HE면 iPhone이 우리에게
    /// Wi-Fi 6(HE)으로 보냅니다: HE를 받는 무선(RTL8912AU)에서만 켭니다.
    pub awdl_ext_flags: u16,
    /// 전송 중인 상대가 Wi-Fi 때문에 우리 80 MHz 안의 다른 채널(149면 153, 157, 161)에 머무는 슬롯에
    /// 무선도 그 주 채널로 옮겨 따라갑니다 (owly_awdl::Config::follow). 80 MHz일 때만.
    pub follow_channels: bool,
}

impl Tuning {
    /// usbfwd로 구동하는 Realtek USB 어댑터 (TP-Link Archer T3U Nano, ipTIME BE6500UA).
    pub fn usb_adapter() -> Self {
        Tuning {
            accept_addba: true,
            addba_amsdu: true,
            addba_window: 64,
            amsdu_max_frame: Some(3895),
            vht_max_mpdu: VhtMaxMpdu::B7991,
            infra_channel: false,
            // owly의 -65 dBm은 한 방 건너의 폰이나 iPad(-65 ~ -75 dBm로 들림)를 빼고 그쪽이 보낸
            // 것을 버려, 서로 찾지 못했습니다.
            rssi_threshold: Some(-80),
            country: *b"KR",
            tx_ampdu: true,
            rx_stbc: false,
            beamformee: false,
            awdl_ext_flags: 0,
            follow_channels: true,
        }
    }
}

impl Default for Tuning {
    fn default() -> Self {
        Self::usb_adapter()
    }
}

/// 우리 AWDL 주소, 이름, 채널로 만든 owly-awdl 설정.
pub fn awdl_config(mac: [u8; 6], name: &str, channel: u8, tuning: &Tuning) -> io::Result<Config> {
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(invalid("the AWDL address must be a unicast MAC"));
    }
    if name.is_empty() || name.len() > 63 || name.chars().any(char::is_control) {
        return Err(invalid("the AWDL name must be 1-63 UTF-8 bytes without control characters"));
    }
    if !matches!(channel, 1..=14 | 36..=165) {
        return Err(invalid("unsupported AWDL channel"));
    }
    let radio = RadioConstraints {
        channel: Channel(channel),
        presence: Presence::Fixed,
    };
    let mut c = Config::new(MacAddr(mac), name, radio);
    // 어댑터가 상대의 ACK를 받을 때까지 유니캐스트를 하드웨어로 다시 보냅니다.
    c.no_ack = false;
    c.max_frame = 2304;
    c.rate = if channel > 14 { TxRate::Legacy(108) } else { TxRate::BASIC };
    if channel > 14 {
        let mut vht = VHT_CAPS;
        vht[0] |= tuning.vht_max_mpdu as u8;
        if tuning.rx_stbc {
            vht[1] |= 0x01; // RX STBC: 1 공간 스트림 (8-10비트)
        }
        if tuning.beamformee {
            vht[1] |= 0x30; // SU 빔포밍 수신 (12비트), 빔포밍 수신 STS 2 (13-15비트 = 1)
        }
        c.vht_caps = Some(vht);
    }
    c.accept_addba = tuning.accept_addba;
    c.addba_amsdu = tuning.accept_addba && tuning.addba_amsdu;
    c.addba_window = tuning.addba_window.clamp(1, 64);
    if let Some(n) = tuning.amsdu_max_frame.filter(|&n| n > 1600) {
        c.amsdu = true;
        c.max_frame = n.min(7935);
    }
    c.infra_channel = tuning.infra_channel.then_some(channel);
    c.rssi_threshold = tuning.rssi_threshold;
    c.country = tuning.country;
    c.tx_addba = tuning.tx_ampdu;
    c.ext_flags = tuning.awdl_ext_flags;
    Ok(c)
}

/// 5 GHz channel이 든 80 MHz 묶음의 다른 주 채널들 (149면 153, 157, 161). 묶음은 36과 149에서
/// 16씩 시작하고, 묶음이 없는 채널(165 등)과 2.4 GHz는 빈 목록.
pub fn same_80mhz(channel: u8) -> Vec<Channel> {
    let start = match channel {
        36..=64 | 100..=144 => 36 + (channel - 36) / 16 * 16,
        149..=161 => 149,
        _ => return Vec::new(),
    };
    (0..4).map(|i| start + 4 * i).filter(|&c| c != channel).map(Channel).collect()
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_tuning_matches_the_measured_bridge() {
        let c = awdl_config([2, 1, 2, 3, 4, 5], "PC", 149, &Tuning::usb_adapter()).unwrap();
        assert!(c.accept_addba && c.addba_amsdu && c.amsdu && !c.no_ack);
        assert_eq!(c.max_frame, 3895);
        assert_eq!(c.vht_caps.unwrap()[0], 0x31);
        assert_eq!(c.country, *b"KR");
        assert!(awdl_config([3, 0, 0, 0, 0, 1], "PC", 149, &Tuning::default()).is_err());
    }

    #[test]
    fn the_80mhz_blocks() {
        let chans = |c| same_80mhz(c).iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(chans(149), [153, 157, 161]);
        assert_eq!(chans(157), [149, 153, 161]);
        assert_eq!(chans(44), [36, 40, 48]);
        assert_eq!(chans(112), [100, 104, 108]);
        assert!(chans(165).is_empty() && chans(6).is_empty());
    }
}
