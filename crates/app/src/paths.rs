//! WirDrop이 파일과 로그를 두는 곳.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DATA: OnceLock<PathBuf> = OnceLock::new();

/// 파일들의 크기. 그 가운데 폴더는 안의 모든 것까지 더합니다 (링크는 따라가지 않습니다).
pub fn total_size(paths: &[PathBuf]) -> u64 {
    fn size(path: &Path) -> u64 {
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => std::fs::read_dir(path).map(|d| d.flatten().map(|e| size(&e.path())).sum()).unwrap_or(0),
            Ok(m) if m.is_file() => m.len(),
            _ => 0,
        }
    }
    paths.iter().map(|p| size(p)).sum()
}

/// %LOCALAPPDATA%\WirDrop. wirdrop 라이브러리의 상태(AWDL 주소, 서비스 ID)와 함께 씁니다. 엔진
/// 서비스에서는 [set_data_dir]가 준 사용자의 폴더입니다.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = DATA.get() {
        return dir.clone();
    }
    wirdrop::State::default_dir().unwrap_or_else(|| std::env::temp_dir().join("WirDrop"))
}

/// 엔진 서비스는 SYSTEM 프로필로 SYSTEM 권한에서 돌므로 트레이 사용자의 폴더를 씁니다.
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA.set(dir);
}

pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

/// 들어온 요청의 미리보기 같은 버려도 되는 파일.
pub fn cache_dir() -> PathBuf {
    data_dir().join("cache")
}

pub fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("WirDrop.exe"))
}

/// WirDrop.exe 옆의 파일: usbfwd.exe, wintun.dll, appleid.exe.
pub fn beside(name: &str) -> PathBuf {
    exe().with_file_name(name)
}

/// appleid.exe로 첫 Apple ID에 로그인한 결과가 가는 곳 (APPLEID_OUT). 신원은 그 아래 identity\에
/// 생깁니다.
pub fn apple_id_root() -> PathBuf {
    data_dir().join("apple-id")
}

/// 이 PC의 다른 사람 Apple ID들: apple-ids\<n>. 저마다 [apple_id_root]와 같은 구성이고 자기 로컬
/// 사용자(appleid의 anisette\local-user.txt)가 있습니다.
fn more_apple_ids() -> PathBuf {
    data_dir().join("apple-ids")
}

/// 로그인 폴더의 신원: 그 아래 identity\, 또는 폴더 자체.
pub fn identity_dir(root: &Path) -> Option<PathBuf> {
    [root.join("identity"), root.to_path_buf()]
        .into_iter()
        .find(|d| d.join("validation_record.cms").is_file())
}

/// 신원이나 저장된 토큰(세션만 있으면 로그인은 했고 인증서는 아직)이 있는 모든 Apple ID 로그인
/// 폴더. 첫 Apple ID가 먼저이고, 나머지는 추가한 순서대로.
pub fn apple_id_roots() -> Vec<PathBuf> {
    let signed_in = |root: &Path| identity_dir(root).is_some() || root.join("gs-token.txt").is_file() || root.join("session.plist").is_file();
    let mut more: Vec<(u32, PathBuf)> = std::fs::read_dir(more_apple_ids())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
        .filter(|(_, p)| signed_in(p))
        .collect();
    more.sort();
    let first = apple_id_root();
    signed_in(&first)
        .then_some(first)
        .into_iter()
        .chain(more.into_iter().map(|(_, p)| p))
        .collect()
}

/// Apple ID를 하나 더 둘 폴더: 로그인을 마친 적 없는 첫 apple-ids\<n>이라, 실패한 시도의 폴더를
/// 로컬 사용자까지 다시 씁니다. 아직 아무것도 없으면 첫 Apple ID의 폴더.
pub fn new_apple_id_root() -> PathBuf {
    let roots = apple_id_roots();
    if roots.is_empty() {
        return apple_id_root();
    }
    (2..)
        .map(|n: u32| more_apple_ids().join(n.to_string()))
        .find(|p| !roots.contains(p))
        .expect("a free number")
}

/// 다른 사람의 Apple ID: 자기 로컬 사용자를 받습니다 (APPLEID_SEPARATE_USER).
pub fn is_more_apple_id(root: &Path) -> bool {
    root.parent() == Some(more_apple_ids().as_path())
}

const ACCOUNT_FILE: &str = "account.json";
/// appleid.exe가 로그인 폴더의 Apple ID에 대해 알아낸 것: 그 사람의 이름과 로그인한 Apple ID.
const NAME_FILE: &str = "name.json";

/// root의 작은 JSON 파일에서 읽은 문자열 (앞뒤 공백 제거). 있고 비어 있지 않을 때.
fn json_text(root: &Path, file: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(file)).ok()?).ok()?;
    v[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// root의 Apple ID에 로그인할 때 쓴 이메일 주소나 전화번호 (WirDrop의 로그인이나 appleid.exe가
/// 저장해 두었을 때).
pub fn apple_id_email(root: &Path) -> Option<String> {
    json_text(root, ACCOUNT_FILE, "email").or_else(|| json_text(root, NAME_FILE, "account"))
}

/// index번째로 로그인한 root의 Apple ID를 부를 이름: 주인의 이름, 없으면 로그인한 이메일 주소나
/// 전화번호, 그것도 없으면 "Apple ID <n>" (첫 것은 1).
pub fn apple_id_label(root: &Path, index: usize) -> String {
    json_text(root, NAME_FILE, "name")
        .or_else(|| apple_id_email(root))
        .unwrap_or_else(|| format!("Apple ID {}", index + 1))
}

pub fn save_apple_id_email(root: &Path, email: &str) {
    let _ = std::fs::write(root.join(ACCOUNT_FILE), serde_json::json!({ "email": email.trim() }).to_string());
}

/// 쓸 Apple ID 신원들, 첫 Apple ID가 먼저: 신원 폴더와, 받는 쪽의 서비스 ID를 저장하는 이름
/// (첫 Apple ID는 "").
pub fn apple_id_dirs() -> Vec<crate::ipc::AccountDir> {
    apple_id_roots()
        .iter()
        .filter_map(|root| {
            let key = if is_more_apple_id(root) {
                root.file_name()?.to_string_lossy().into_owned()
            } else {
                String::new()
            };
            Some(crate::ipc::AccountDir {
                identity: identity_dir(root)?,
                key,
            })
        })
        .collect()
}

/// Apple ID에 로그인하는 appleid.exe (appleid 크레이트). 있으면.
pub fn appleid() -> Option<PathBuf> {
    Some(beside("appleid.exe")).filter(|p| p.is_file())
}

/// 사용자의 다운로드 폴더.
pub fn downloads() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: 돌려받은 문자열을 복사한 뒤 CoTaskMemFree로 해제합니다.
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let path = p.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(p.0 as _));
            if let Some(path) = path {
                return path;
            }
        }
    }
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Downloads"))
        .unwrap_or_default()
}

/// 로그를 %LOCALAPPDATA%\WirDrop\logs\<role>.log에 남깁니다 (WIRDROP_LOG로 필터, 기본 info). 엔진은
/// 기기가 이 PC를 띄우지 않는 까닭을 보려고 받는 쪽 연결도 모두 남깁니다.
pub fn init_log(role: &str) {
    // 엔진 서비스는 엔진을 돌리기 전에 로그를 설정하고, 엔진이 다시 설정합니다.
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{role}.log"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 << 20) {
        let _ = std::fs::rename(&path, dir.join(format!("{role}.old.log")));
    }
    let Some(file) = open_log(&path, role) else {
        return;
    };
    let default = if role == "engine" {
        "info,owly_airdrop::driver::receiver=debug"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("WIRDROP_LOG").unwrap_or_else(|_| default.into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        previous(info);
    }));
    tracing::info!("WirDrop {} {role} started (pid {})", env!("CARGO_PKG_VERSION"), std::process::id());
}

/// 로그 파일을 엽니다. 로그온 직후에는 백신이나 앞선 WirDrop이 파일을 잠깐 쥐고 있어 열리지 않을 수
/// 있는데, 거기서 그냥 포기하면 그 뒤로 아무것도 남지 않습니다. 그래서 잠깐씩 다시 열어 보고, 그래도
/// 안 되면 이 프로세스만의 이름으로 엽니다.
fn open_log(path: &Path, role: &str) -> Option<std::fs::File> {
    let open = |path: &Path| std::fs::OpenOptions::new().create(true).append(true).open(path);
    for attempt in 0..10 {
        match open(path) {
            Ok(file) => return Some(file),
            Err(_) if attempt < 9 => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(_) => {}
        }
    }
    let mine = path.with_file_name(format!("{role}-{}.log", std::process::id()));
    open(&mine).ok()
}
