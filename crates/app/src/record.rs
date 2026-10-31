//! 누구의 것인지 알 만큼만 읽는 Apple ID 확인 기록 (CMS 서명된 XML plist).

pub use wirdrop::owly_airdrop::proto::apple_id::alt_dsid;

/// /Ask를 보낸 사람의 고정된 키: 기록을 보냈으면 그 Apple ID, 아니면 기기 이름과 모델.
/// settings.json에 남지 않도록 Apple ID 부분은 해시합니다.
pub fn person_key(record: Option<&[u8]>, name: &str, model: &str) -> String {
    match record.and_then(alt_dsid) {
        Some(id) => format!("apple:{:016x}", fnv1a(id.as_bytes())),
        None => format!("device:{name}|{model}"),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_alt_dsid() {
        let rec = b"\x30\x82junk<?xml version=\"1.0\"?><plist><dict><key>Version</key><integer>2</integer><key>altDsID</key>\n\t<string> 001234-05-abcd </string></dict></plist>\x00";
        assert_eq!(alt_dsid(rec).as_deref(), Some("001234-05-abcd"));
        assert_eq!(alt_dsid(b"<key>altDsID</key><integer>1</integer><string>x</string>"), None);
        assert!(person_key(Some(rec), "a", "b").starts_with("apple:"));
        assert_eq!(person_key(None, "Min's iPhone", "iPhone"), "device:Min's iPhone|iPhone");
    }
}
