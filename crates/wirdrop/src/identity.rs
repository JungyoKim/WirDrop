//! AirDrop 상대에게 이 PC가 누구인지: TLS 신원(Apple ID 신원이나 자체 서명 신원), 그리고 실행이
//! 바뀌어도 같아야 하는 것(AWDL 주소와 mDNS 서비스 ID).
//!
//! 한 PC에 Apple ID 여럿이 로그인할 수 있습니다. 사람마다 하나씩, 사용자 계정마다 Apple ID가 있는
//! Mac처럼. 하나마다 자기 신원과, 그 받는 쪽과 보내는 쪽이 함께 쓰는 [Account]가 있습니다.
use std::io;
use std::path::{Path, PathBuf};

use owly_airdrop::driver::tls;
use owly_airdrop::proto::apple_id::{self, AppleIdentity};
use owly_awdl::MacAddr;

pub use owly_airdrop::proto::apple_id::Account;

/// 받는 쪽의 TLS 신원.
#[derive(Debug, Clone)]
pub struct Identity {
    tls: tls::Identity,
    account: Option<Account>,
}

impl Identity {
    /// dir에 있는 Apple ID AirDrop 신원: certificate.der, key.pk8.der (PKCS#8),
    /// validation_record.cms, 있으면 intermediate.der와 hashes.txt ("email <sha256 hex>", "phone
    /// <sha256 hex>" 줄). 그러면 같은 Apple ID의 iPhone은 이 PC의 파일을 묻지 않고 받고, 연락처는
    /// "연락처만" 모드에서도 이 PC를 봅니다.
    ///
    /// 기록을 [Account::global]에 넣습니다: owly가 /Discover와 /Ask에 보내고, BLE 광고에 해시
    /// 앞부분을 싣습니다. 중간 인증서는 TLS 체인에 들어갑니다.
    pub fn apple_id(dir: &Path) -> io::Result<Identity> {
        Self::apple_id_as(dir, Account::global())
    }

    /// [Identity::apple_id]와 같되 account로: 이 PC의 여러 Apple ID 가운데 하나.
    pub fn apple_id_as(dir: &Path, account: Account) -> io::Result<Identity> {
        let read = |name: &str| std::fs::read(dir.join(name));
        let cert = read("certificate.der")?;
        let key = read("key.pk8.der")?;
        let (emails, phones) = install_record(dir, &account)?;
        let contacts = load_contacts(dir, &account);
        tracing::info!(
            "Apple ID AirDrop identity from {} ({} email, {} phone hashes, {} contact hashes)",
            dir.display(),
            emails,
            phones,
            contacts.unwrap_or(0)
        );
        let intermediates = account.get().map(|i| i.intermediates.clone()).unwrap_or_default();
        let tls = tls::Identity::from_der(cert, key).with_intermediates(intermediates);
        Ok(Identity { tls, account: Some(account) })
    }

    /// dir에 두는 자체 서명 신원. 처음 쓸 때 만듭니다. 폰에는 "모든 사람" 모드에서 보입니다.
    pub fn anonymous(dir: &Path) -> io::Result<Identity> {
        Ok(Identity {
            tls: tls::Identity::load_or_generate(dir)?,
            account: None,
        })
    }

    /// apple_dir에 Apple ID 신원이 있으면 그것, 없으면 state_dir의 익명 신원.
    pub fn load(apple_dir: Option<&Path>, state_dir: &Path) -> io::Result<Identity> {
        match apple_dir {
            Some(dir) => match std::fs::metadata(dir.join("validation_record.cms")) {
                Ok(m) if m.is_file() => Self::apple_id(dir),
                // 엔진은 LocalSystem 서비스입니다: 그 사용자만 읽을 수 있는 폴더는 비어 보입니다.
                result => {
                    let why = result.err().map_or_else(|| "not a file".to_string(), |e| e.to_string());
                    tracing::warn!("can't read the Apple ID identity in {} ({why}), so going on without it", dir.display());
                    Self::anonymous(state_dir)
                }
            },
            _ => Self::anonymous(state_dir),
        }
    }
}

/// appleid가 iCloud에서 읽은 사용자의 연락처: 신원 폴더의 contact_hashes.txt, 한 줄에 전화번호나
/// 이메일 주소 하나의 SHA-256 해시(hex). account에 넣으면 그 받는 쪽은 그 사람들의 기기에만 이 PC를
/// 연락처 카드로 보여 줍니다. 개수를 돌려주고, 파일이 없으면 None.
pub fn load_contacts(dir: &Path, account: &Account) -> Option<usize> {
    let text = std::fs::read_to_string(dir.join(CONTACTS_FILE)).ok()?;
    let set: std::collections::HashSet<[u8; 32]> = text.lines().filter_map(|l| apple_id::hex32(l.trim())).collect();
    let n = set.len();
    account.set_contacts(set);
    Some(n)
}

/// contact_hashes.txt가 마지막으로 바뀐 때. appleid가 새로 고친 뒤 다시 읽으려고 씁니다.
pub fn contacts_changed(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir.join(CONTACTS_FILE)).and_then(|m| m.modified()).ok()
}

const CONTACTS_FILE: &str = "contact_hashes.txt";

/// dir의 확인 기록을 account에 넣습니다 (받는 쪽과 보내는 쪽이 보내고, BLE 광고에 해시 앞부분이
/// 실립니다). TLS 키는 건드리지 않으므로 엔진이 도는 중에 appleid가 갱신한 기록에도 씁니다.
/// 이메일과 전화번호 해시가 몇 개인지 돌려줍니다.
pub fn install_record(dir: &Path, account: &Account) -> io::Result<(usize, usize)> {
    let record = std::fs::read(dir.join("validation_record.cms"))?;
    let intermediates = std::fs::read(dir.join("intermediate.der")).map(|c| vec![c]).unwrap_or_default();
    let (mut emails, mut phones) = (Vec::new(), Vec::new());
    if let Ok(text) = std::fs::read_to_string(dir.join("hashes.txt")) {
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(kind), Some(Some(prefix))) = (fields.next(), fields.next().map(hash_prefix)) else {
                continue;
            };
            match kind {
                "email" => emails.push(prefix),
                "phone" => phones.push(prefix),
                _ => {}
            }
        }
    }
    let nth = |v: &[[u8; 2]], i: usize| v.get(i).copied().unwrap_or_default();
    // BLE 해시 칸: Apple ID, 전화번호, 이메일, 두 번째 이메일. Apple ID는 이메일 주소입니다.
    let ble_hashes = [nth(&emails, 0), nth(&phones, 0), nth(&emails, 0), nth(&emails, 1)];
    account.install(AppleIdentity {
        record,
        intermediates,
        ble_hashes,
    });
    Ok((emails.len(), phones.len()))
}

/// 신원 폴더의 파일이 마지막으로 바뀐 때.
pub fn changed(dir: &Path, name: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir.join(name)).and_then(|m| m.modified()).ok()
}

impl Identity {
    pub fn is_apple_id(&self) -> bool {
        self.account.is_some()
    }

    /// 이 신원이 Apple ID 신원이면 그 Apple ID.
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    pub fn tls(&self) -> &tls::Identity {
        &self.tls
    }
}

fn hash_prefix(hex: &str) -> Option<[u8; 2]> {
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?])
}

/// Apple ID 신원 폴더 %LOCALAPPDATA%\WirDrop\apple-id (있으면).
pub fn default_apple_id_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"WirDrop\apple-id");
    dir.join("validation_record.cms").is_file().then_some(dir)
}

/// 실행이 바뀌어도 유지하는 설정. 상대가 둘 다 기억하므로 바꾸면 PC가 새 기기처럼 보입니다.
#[derive(Debug, Clone)]
pub struct State {
    dir: PathBuf,
}

impl State {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<State> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(State { dir })
    }

    /// %LOCALAPPDATA%\WirDrop
    pub fn default_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("WirDrop"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 우리 AWDL 주소 (awdl-mac.txt): 무작위, 유니캐스트, 로컬 관리 주소. 한 번 만듭니다.
    pub fn awdl_mac(&self) -> io::Result<[u8; 6]> {
        let path = self.dir.join("awdl-mac.txt");
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(mac) = text.trim().parse::<MacAddr>()
            && mac.0[0] & 1 == 0
            && mac.0 != [0; 6]
        {
            return Ok(mac.0);
        }
        let mut mac = random::<6>()?;
        mac[0] = (mac[0] & 0xfc) | 0x02;
        std::fs::write(&path, MacAddr(mac).to_string())?;
        Ok(mac)
    }

    /// mDNS 인스턴스 이름이자 AirDrop SenderID (service-id.txt): 16진수 12자리. 한 번 만듭니다.
    pub fn service_id(&self) -> io::Result<String> {
        self.service_id_in("service-id.txt")
    }

    /// 이 PC의 다른 Apple ID 받는 쪽의 서비스 ID (service-id-<key>.txt). key는 그 계정의 폴더
    /// 이름입니다.
    pub fn service_id_for(&self, key: &str) -> io::Result<String> {
        let key: String = key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        self.service_id_in(&format!("service-id-{key}.txt"))
    }

    fn service_id_in(&self, name: &str) -> io::Result<String> {
        let path = self.dir.join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let id = text.trim();
            if id.len() == 12 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(id.to_string());
            }
        }
        let id = owly_airdrop::proto::mdns::service_id(random::<6>()?);
        std::fs::write(&path, &id)?;
        Ok(id)
    }
}

pub(crate) fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_keeps_its_values() {
        let dir = std::env::temp_dir().join(format!("wirdrop-state-{}", std::process::id()));
        let state = State::open(&dir).unwrap();
        let (mac, id) = (state.awdl_mac().unwrap(), state.service_id().unwrap());
        assert_eq!(mac[0] & 0x03, 0x02);
        assert_eq!(id.len(), 12);
        assert_eq!((state.awdl_mac().unwrap(), state.service_id().unwrap()), (mac, id));
        let other = state.service_id_for("2").unwrap();
        assert_ne!(other, state.service_id().unwrap());
        assert_eq!(state.service_id_for("2").unwrap(), other);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hash_prefixes() {
        assert_eq!(hash_prefix("5b64b2d8"), Some([0x5b, 0x64]));
        assert_eq!(hash_prefix("5b"), None);
    }
}
