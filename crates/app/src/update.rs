//! 업데이트: WirDrop이 GitHub에서 새 릴리스를 찾아 UAC 창 없이 스스로 갈아 끼웁니다.
//!
//! 릴리스마다 릴리스 키(Ed25519, 공개 절반은 [PUBLIC_KEY])로 서명한 update.json이 있습니다: 버전,
//! WirDrop 파일을 담은 zip의 위치, zip의 SHA-256과 크기. 트레이 앱이 몇 시간마다 읽고, 새 버전이
//! 있고 보내기·받기·묻기가 없으면 엔진을 내려놓고 엔진 서비스를 업데이트용으로 한 번 더 띄웁니다
//! (service.rs). 서비스는 늘 그렇듯 SYSTEM으로 돌며 update.json을 다시 읽고, 서명과 더 새 버전인지
//! 확인하고, zip을 받아 확인한 뒤 Program Files의 파일을 바꿉니다. 바꾸지 못하는 파일이 있으면 바꾼
//! 것을 되돌립니다. 로그인한 누구나 서비스를 띄울 수 있지만, 업데이트 실행이 할 수 있는 일은 서명된
//! 더 새 릴리스를 놓는 것뿐입니다.
use std::ffi::c_void;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// 이 빌드의 버전.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// 최신 릴리스의 update.json.
const FEED: &str = "https://github.com/JungyoKim/WirDrop/releases/latest/download/update.json";
/// 릴리스 키의 공개 절반 (비밀 키는 examples/update_sign.rs가 만들고 씁니다).
const PUBLIC_KEY: [u8; 32] = [
    0x05, 0x1e, 0xef, 0x3c, 0x24, 0x4e, 0x75, 0x16, 0xf5, 0x41, 0x8a, 0x6a, 0x49, 0x34, 0xfb, 0x79, 0x8a, 0xf8, 0x29, 0x79, 0x15, 0x74, 0x83, 0x0a,
    0xfe, 0x73, 0xa1, 0x57, 0x69, 0x27, 0x3f, 0x7c,
];
/// 서비스가 업데이트를 받아 푸는 곳 (Program Files\WirDrop 안).
const WORK: &str = ".update";
/// 업데이트가 바꾼 파일은 아무도 잡고 있지 않을 때까지 새 파일 옆에 이 이름으로 바꿔 둡니다.
const ASIDE: &str = ".old-";

/// update.json이 설명하는 릴리스.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub version: String,
    /// WirDrop 파일을 담은 zip (https).
    pub url: String,
    /// zip의 SHA-256 (16진수).
    pub sha256: String,
    pub size: u64,
    /// [Manifest::message]에 대한 Ed25519 서명 (16진수).
    pub signature: String,
}

impl Manifest {
    /// 서명이 덮는 내용. examples/update_sign.rs도 같은 것을 씁니다.
    pub fn message(&self) -> String {
        format!("WirDrop update\n{}\n{}\n{}\n{}\n", self.version, self.url, self.sha256, self.size)
    }

    fn signed_by(&self, key: &[u8]) -> bool {
        let Some(signature) = unhex(&self.signature) else { return false };
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(self.message().as_bytes(), &signature)
            .is_ok()
    }
}

/// 최신 릴리스가 이것보다 새로우면 그 릴리스. 릴리스 키로 서명되지 않은 update.json은 오류입니다.
pub fn check() -> io::Result<Option<Manifest>> {
    let mut json = Vec::new();
    get(FEED, 64 * 1024, &mut json)?;
    let manifest: Manifest = serde_json::from_slice(&json).map_err(|e| io::Error::other(format!("update.json: {e}")))?;
    if !manifest.signed_by(&PUBLIC_KEY) {
        return Err(io::Error::other("update.json isn't signed with the release key"));
    }
    Ok(newer(&manifest.version, CURRENT).then_some(manifest))
}

/// 버전 a가 b보다 뒤인지 ("1.2.10" > "1.2.9"). 숫자가 같으면 '-' 뒤가 붙은 시험판이 정식판보다 앞입니다
/// ("0.2.13" > "0.2.13-beta.1"): 시험판을 깐 PC도 그 정식판이 나오면 받습니다.
pub fn newer(a: &str, b: &str) -> bool {
    let pre = |v: &str| v.split_once('-').map(|(_, p)| p.to_string());
    let (pre_a, pre_b) = (pre(a), pre(b));
    let parts = |v: &str| -> Vec<u64> {
        v.split('-')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    let n = a.len().max(b.len());
    let at = |v: &Vec<u64>, i: usize| v.get(i).copied().unwrap_or(0);
    match (0..n).map(|i| (at(&a, i), at(&b, i))).find(|(x, y)| x != y) {
        Some((x, y)) => x > y,
        None => match (pre_a, pre_b) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
    }
}

// ---- 서비스 안에서

/// 최신 릴리스가 더 새로우면 dir(Program Files\WirDrop)에 놓습니다. 놓았으면 true.
pub fn apply(dir: &Path) -> io::Result<bool> {
    let Some(manifest) = check()? else {
        tracing::info!("update: {CURRENT} is the latest");
        return Ok(false);
    };
    tracing::info!("update: {CURRENT} to {}", manifest.version);
    let work = dir.join(WORK);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let zip = work.join("update.zip");
    let mut file = io::BufWriter::new(fs::File::create(&zip)?);
    let mut hash = Hashing {
        out: &mut file,
        digest: ring::digest::Context::new(&ring::digest::SHA256),
    };
    let size = get(&manifest.url, manifest.size, &mut hash)?;
    let digest = hex(hash.digest.finish().as_ref());
    file.flush()?;
    drop(file);
    if size != manifest.size || !digest.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(io::Error::other(format!("the download isn't the release's zip ({size} bytes, {digest})")));
    }
    let new = work.join("new");
    fs::create_dir_all(&new)?;
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join(r"System32\tar.exe");
    let out = crate::win::hidden(&tar.to_string_lossy())
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&new)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    if !new.join("WirDrop.exe").is_file() {
        return Err(io::Error::other("the zip has no WirDrop.exe"));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut done = Vec::new();
    if let Err(e) = swap(&new, dir, stamp, &mut done) {
        // 원래대로: 새 파일을 빼고 옛 파일을 넣습니다.
        for (dest, aside) in done.iter().rev() {
            let _ = fs::remove_file(dest);
            if let Some(aside) = aside {
                let _ = fs::rename(aside, dest);
            }
        }
        return Err(e);
    }
    tracing::info!("update: {} files replaced; now {}", done.len(), manifest.version);
    let _ = fs::remove_dir_all(&work);
    Ok(true)
}

/// from 아래의 파일을 to 아래로 옮깁니다. 바꿀 파일은 먼저 옆으로 이름을 바꿉니다 (돌고 있는
/// 프로그램이나 로드된 DLL은 덮어쓸 수 없어도 이름은 바꿀 수 있습니다). done: 넣은 파일과, 바뀐
/// 파일이 간 곳.
fn swap(from: &Path, to: &Path, stamp: u128, done: &mut Vec<(PathBuf, Option<PathBuf>)>) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (src, dest) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            swap(&src, &dest, stamp, done)?;
            continue;
        }
        if same(&src, &dest) {
            continue;
        }
        let aside = if dest.exists() {
            let mut name = dest.file_name().unwrap_or_default().to_os_string();
            name.push(format!("{ASIDE}{stamp}"));
            let aside = dest.with_file_name(name);
            fs::rename(&dest, &aside).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dest.display())))?;
            Some(aside)
        } else {
            None
        };
        let moved = fs::rename(&src, &dest).or_else(|_| fs::copy(&src, &dest).map(drop));
        done.push((dest.clone(), aside));
        moved.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dest.display())))?;
    }
    Ok(())
}

/// 앞선 업데이트가 dir에 남긴 것을 지웁니다: 옆으로 비켜 둔 파일(아무도 잡고 있지 않을 때)과 끝나지
/// 않은 다운로드.
pub fn tidy(dir: &Path) {
    fn walk(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path);
            } else if entry.file_name().to_string_lossy().contains(ASIDE) {
                let _ = fs::remove_file(&path);
            }
        }
    }
    walk(dir);
    let _ = fs::remove_dir_all(dir.join(WORK));
}

fn same(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => fs::read(a).ok() == fs::read(b).ok(),
        _ => false,
    }
}

/// 지나가는 내용을 해시하며 씁니다.
struct Hashing<'a, W: Write> {
    out: &'a mut W,
    digest: ring::digest::Context,
}

impl<W: Write> Write for Hashing<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.digest.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

// ---- WinHTTP로 하는 HTTPS (Windows의 프록시 설정과 인증서 저장소)

struct Internet(*mut c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: WinHTTP가 준 핸들을 한 번 닫습니다.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn opened(handle: *mut c_void) -> io::Result<Internet> {
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Internet(handle))
    }
}

/// https URL을 out으로 GET합니다 (리디렉션을 따릅니다). limit 바이트를 넘으면 오류.
fn get(url: &str, limit: u64, out: &mut dyn Write) -> io::Result<u64> {
    get_raw(url, limit, out).map_err(readable)
}

/// WinHTTP 오류를 읽을 수 있는 말로: Windows의 오류 문구 표에는 WinHTTP 것이 없어 "FormatMessageW() returned
/// error 317"로만 찍힙니다.
fn readable(e: io::Error) -> io::Error {
    let Some(hr) = e.raw_os_error() else { return e };
    // HRESULT_FROM_WIN32(12xxx) 또는 그 번호 그대로.
    let code = if (hr as u32) & 0xffff_0000 == 0x8007_0000 {
        (hr as u32) & 0xffff
    } else {
        hr as u32
    };
    let what = match code {
        12002 => "timed out",
        12007 => "no internet (the name didn't resolve)",
        12029 => "no internet (can't connect)",
        12030 => "the connection broke",
        12175 => "the secure connection failed",
        12000..=12200 => "WinHTTP failed",
        _ => return e,
    };
    io::Error::new(e.kind(), format!("{what} (WinHTTP {code})"))
}

fn get_raw(url: &str, limit: u64, out: &mut dyn Write) -> io::Result<u64> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| io::Error::other(format!("not https: {url}")))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    // SAFETY: 함수 끝까지 사는 핸들에 대한 WinHTTP 호출이고, 버퍼는 크기를 준 지역 변수입니다.
    unsafe {
        let session = opened(WinHttpOpen(
            w!("WirDrop"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000)?;
        let connect = opened(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0))?;
        let request = opened(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
        let (mut status, mut len) = (0u32, size_of::<u32>() as u32);
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut len,
            std::ptr::null_mut(),
        )?;
        if status != 200 {
            return Err(io::Error::other(format!("HTTP {status}: {url}")));
        }
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read)?;
            if read == 0 {
                return Ok(total);
            }
            total += u64::from(read);
            if total > limit {
                return Err(io::Error::other(format!("more than {limit} bytes: {url}")));
            }
            out.write_all(&buf[..read as usize])?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0-beta", "0.1.0"));
        assert!(newer("0.1.0", "0.1.0-beta"));
        assert!(newer("0.2.13", "0.2.13-beta.1"));
        assert!(!newer("0.2.12", "0.2.13-beta.1"));
        assert!(newer("0.2.13-beta.1", "0.2.12"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn winhttp_errors_read_plainly() {
        let e = readable(io::Error::from_raw_os_error(0x8007_2ee7_u32 as i32));
        assert_eq!(e.to_string(), "no internet (the name didn't resolve) (WinHTTP 12007)");
        let e = readable(io::Error::other("HTTP 404: x"));
        assert_eq!(e.to_string(), "HTTP 404: x");
    }

    /// examples/update_sign.rs가 쓰는 꼴의 update.json을 버릴 키로 만듭니다: 서명하는 것이
    /// check()가 읽는 것과 같은지 봅니다.
    #[test]
    fn the_signing_tool_writes_what_check_reads() {
        const THROWAWAY: [u8; 32] = [
            0xf0, 0x4d, 0x8d, 0xff, 0x42, 0xf8, 0x74, 0x5b, 0xc7, 0x27, 0x2d, 0xfb, 0x8e, 0x02, 0x2a, 0xcc, 0xb8, 0x2a, 0x3c, 0xab, 0xe6, 0x3d, 0x33,
            0xd3, 0x3f, 0xa0, 0xd9, 0xa4, 0x2c, 0xac, 0xe8, 0x05,
        ];
        let json = r#"{
  "sha256": "f16d05ec6b29248d2c61adb1e9263f78e4f7bace1b955014a2d17872cfe4064d",
  "signature": "2235a317b533d4cea275e3aed09300bc28420f364a8a2adf8b2484bbba78e04a75e7526ddb96992276d37392bc59477c1f7bb2857f0cf7cbf38f9b11f53cb106",
  "size": 7,
  "url": "https://example.com/WirDrop-0.0.1.zip",
  "version": "0.0.1"
}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.signed_by(&THROWAWAY));
        assert!(!m.signed_by(&PUBLIC_KEY));
    }

    #[test]
    fn only_the_key_signature_counts() {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut m = Manifest {
            version: "9.9.9".into(),
            url: "https://example.com/WirDrop-9.9.9.zip".into(),
            sha256: "00".repeat(32),
            size: 1234,
            signature: String::new(),
        };
        m.signature = hex(key.sign(m.message().as_bytes()).as_ref());
        assert!(m.signed_by(key.public_key().as_ref()));
        // 덮는 내용 어느 것이든 바뀌면 더는 맞지 않습니다.
        for change in [
            |m: &mut Manifest| m.version = "9.9.10".into(),
            |m: &mut Manifest| m.url.push('x'),
            |m: &mut Manifest| m.size += 1,
        ] {
            let mut other = m.clone();
            change(&mut other);
            assert!(!other.signed_by(key.public_key().as_ref()));
        }
        assert!(!m.signed_by(&PUBLIC_KEY));
    }

    #[test]
    fn a_swap_moves_files_in_and_the_old_ones_aside() {
        let root = std::env::temp_dir().join(format!("wirdrop-swap-{}", std::process::id()));
        let (from, to) = (root.join("new"), root.join("dir"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(from.join("icons")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(from.join("a.exe"), b"new a").unwrap();
        fs::write(from.join("icons").join("i.ico"), b"new i").unwrap();
        fs::write(to.join("a.exe"), b"old a").unwrap();
        let mut done = Vec::new();
        swap(&from, &to, 1, &mut done).unwrap();
        assert_eq!(fs::read(to.join("a.exe")).unwrap(), b"new a");
        assert_eq!(fs::read(to.join("a.exe.old-1")).unwrap(), b"old a");
        assert_eq!(fs::read(to.join("icons").join("i.ico")).unwrap(), b"new i");
        tidy(&to);
        assert!(!to.join("a.exe.old-1").exists() && to.join("a.exe").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
