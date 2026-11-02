//! Anisette: Apple 서버가 요청이 어느 기기에서 왔는지 아는 헤더(X-Apple-I-MD-M)와 그 기기의 일회용
//! 암호(X-Apple-I-MD), 그리고 기기 ID와 로컬 사용자 ID. Mac에서는 시스템이 구하고, Windows에서는
//! iCloud for Windows의 AOSKit이 구합니다: Apple에서 한 번 받아 설치하지 않고 풀어 두고, AOSKit처럼
//! 32비트인 appleid-anisette.exe를 거쳐 묻습니다.
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use plist::{Dictionary, Value};
use sha2::{Digest, Sha256};

/// 32비트 AOSKit이 있는 마지막 iCloud for Windows 7.21을 Apple이 내려주는 곳과 그 SHA-256.
const ICLOUD_SETUP: &str = "https://secure-appldnld.apple.com/windows/061-91601-20200323-974a39d0-41fc-4761-b571-318b7d9205ed/iCloudSetup.exe";
const ICLOUD_SETUP_SHA256: &str = "1100052f27c8ebf6eea57b05682eb4a5a75efd50d7ed1c943bd0108acdc5e692";
/// 풀어 둔 사본 안의 Apple Application Support (32비트)와 Internet Services (AOSKit).
const SUPPORT: &str = r"a\AAS32\Common Files\Apple\Apple Application Support";
const SERVICES: &str = r"a\iCloud64\Common Files\Apple\Internet Services";
/// Apple이 함께 넣어 주는 Microsoft 런타임 둘. AOSKit은 Visual C++ 2010(msvcr100)을, 나머지는 2015
/// 이후(vcruntime140)를 씁니다. PC에 그것이 깔려 있지 않아도 되도록 둘 다 PATH에 둡니다: 2010 것이
/// 없는 PC에서는 AOSKit이 "모듈을 찾을 수 없습니다"로 열리지 않았습니다.
const RUNTIME_2010: &str = r"a\AAS32\Win\System";
const RUNTIME_2015: &str = r"a\AAS32\System";
/// appleid-anisette가 AOSKit의 HKLM\SOFTWARE\Apple Inc. 조회를 돌리는 곳.
const REDIRECTED: &str = r"Software\WirDrop\HKLM\SOFTWARE\Apple Inc.";
/// 이 PC의 다른 사람 Apple ID를 위한 로그인 자체의 로컬 사용자.
const LOCAL_USER: &str = r"anisette\local-user.txt";
/// Apple이 기기 등록 주소를 알려 주는 곳.
const LOOKUP: &str = "https://gsa.apple.com/grandslam/GsService2/lookup";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// AOSKit이 한 번 답하는 데 걸려도 되는 시간: 첫 답은 불러오기도 하고 Apple에 기기 등록을 청할 수도
/// 있습니다.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// 한 실행 동안의 헤더 출처: 도우미, 그리고 Apple에 대해 이 PC와 사용자가 누구인지.
pub struct Anisette {
    helper: Child,
    input: ChildStdin,
    /// 도우미의 답 (한 줄씩). 멈춘 도우미가 로그인까지 멈추지 않도록 따로 스레드에서 읽습니다.
    answers: Receiver<String>,
    device: String,
    local_user: String,
    locale: String,
}

impl Drop for Anisette {
    fn drop(&mut self) {
        let _ = self.helper.kill();
        let _ = self.helper.wait();
    }
}

impl Anisette {
    /// out의 로그인을 위해 도우미를 시작합니다. 이 PC에 iCloud for Windows가 없으면 먼저 받아서
    /// 풉니다.
    pub async fn start(out: &Path) -> Result<Anisette, String> {
        say!("starting Apple's anisette");
        let home = icloud_home();
        if !unpacked(&home) {
            unpack_icloud(&home).await?;
        }
        let (support, services) = (home.join(SUPPORT), home.join(SERVICES));
        for (name, dir) in [
            ("Apple Application Support", &support),
            ("CoreADI", &support),
            ("Internet Services", &services),
            (r"Internet Services\AAS", &support),
        ] {
            set_install_dir(&format!(r"{REDIRECTED}\{name}"), &format!("{}\\", dir.display()))?;
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("appleid-anisette.exe");
        let path = [
            support.display().to_string(),
            services.display().to_string(),
            home.join(RUNTIME_2010).display().to_string(),
            home.join(RUNTIME_2015).display().to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ]
        .join(";");
        let mut helper = Command::new(&exe)
            .env("PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        let input = helper.stdin.take().ok_or("the helper has no input")?;
        let output = BufReader::new(helper.stdout.take().ok_or("the helper has no output")?);
        let (send, answers) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let (device, pc_user) = device()?;
        let local_user = sign_in_user(out)?.unwrap_or(pc_user);
        let mut anisette = Anisette {
            helper,
            input,
            answers,
            device,
            local_user,
            locale: locale(),
        };
        // 이 PC가 Apple에 기기로 등록된 적이 없으면 (iCloud나 iTunes를 켠 적 없는 PC) CoreADI는
        // 일회용 암호를 만들지 못합니다. 그때 한 번 등록하고 다시 묻습니다.
        if let Err(first) = anisette.otp() {
            crate::log_line(&format!("no one-time password yet ({first}); registering this PC"));
            say!("registering this PC with Apple… (once)");
            anisette.provision().await?;
            anisette.otp()?;
            say!("this PC is registered with Apple");
        }
        Ok(anisette)
    }

    /// 도우미에게 한 줄 묻고 받은 JSON 답.
    fn ask(&mut self, request: &str) -> Result<serde_json::Value, String> {
        writeln!(self.input, "{request}")
            .and_then(|()| self.input.flush())
            .map_err(|e| format!("anisette helper: {e}"))?;
        let line = match self.answers.recv_timeout(ANSWER_WITHIN) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => return Err("Apple's anisette didn't answer".into()),
            Err(RecvTimeoutError::Disconnected) => return Err("the anisette helper stopped".into()),
        };
        serde_json::from_str(line.trim()).map_err(|_| "the anisette helper stopped".to_string())
    }

    /// AOSKit의 새 일회용 암호와 기기 ID.
    fn otp(&mut self) -> Result<(String, String), String> {
        let answer = self.ask("otp")?;
        match (answer["md"].as_str(), answer["mdm"].as_str()) {
            (Some(md), Some(mdm)) => Ok((md.to_string(), mdm.to_string())),
            _ => Err(format!("anisette: {}", answer["error"].as_str().unwrap_or("no answer"))),
        }
    }

    /// 이 PC를 Apple에 기기로 등록합니다. Mac과 iPhone은 공장에서 된 채로 오고, Windows에서는
    /// iCloud for Windows나 iTunes를 처음 켤 때 일어나는 일입니다. 둘 다 없는 PC에서는 로그인이
    /// 일회용 암호를 만들지 못하므로 WirDrop이 같은 것을 합니다: Apple이 준 spim을 CoreADI에
    /// 넣어 cpim을 만들어 보내고, Apple이 돌려준 ptm과 tk로 끝냅니다. 암호는 쓰지 않습니다.
    async fn provision(&mut self) -> Result<(), String> {
        let (start, finish) = self.provisioning_urls().await?;
        let spim = text(&self.call(&start, Dictionary::new()).await?, "spim")?;
        let answer = self.ask(&format!("provision-start {spim}"))?;
        let (Some(cpim), Some(session)) = (answer["cpim"].as_str(), answer["session"].as_u64()) else {
            return Err(format!("anisette: {}", answer["error"].as_str().unwrap_or("no answer")));
        };
        let (cpim, session) = (cpim.to_string(), session);
        let mut request = Dictionary::new();
        request.insert("cpim".into(), Value::String(cpim));
        let done = self.call(&finish, request).await?;
        let (ptm, tk) = (text(&done, "ptm")?, text(&done, "tk")?);
        let answer = self.ask(&format!("provision-end {session} {ptm} {tk}"))?;
        if answer["ok"].as_bool() != Some(true) {
            return Err(format!("anisette: {}", answer["error"].as_str().unwrap_or("no answer")));
        }
        Ok(())
    }

    /// 등록을 시작하고 끝내는 Apple의 주소. 그때그때 Apple에 물어봅니다.
    async fn provisioning_urls(&self) -> Result<(String, String), String> {
        let mut get = crate::http::client().get(LOOKUP);
        for (name, value) in self.plain_headers() {
            get = get.header(name, value);
        }
        let bytes = get
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let reply: Value = plist::from_bytes(&bytes).map_err(|e| e.to_string())?;
        let urls = reply
            .as_dictionary()
            .and_then(|d| d.get("urls"))
            .and_then(Value::as_dictionary)
            .ok_or("Apple didn't say where to register this PC")?;
        Ok((text(urls, "midStartProvisioning")?, text(urls, "midFinishProvisioning")?))
    }

    /// 등록 요청 하나: plist를 보내고 Apple의 "Response"를 받습니다.
    async fn call(&self, url: &str, request: Dictionary) -> Result<Dictionary, String> {
        let mut envelope = Dictionary::new();
        envelope.insert("Header".into(), Value::Dictionary(Dictionary::new()));
        envelope.insert("Request".into(), Value::Dictionary(request));
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, &Value::Dictionary(envelope)).map_err(|e| e.to_string())?;
        let mut post = crate::http::client().post(url).header("Content-Type", "text/x-xml-plist").body(body);
        for (name, value) in self.plain_headers() {
            post = post.header(name, value);
        }
        let bytes = post
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let reply: Value = plist::from_bytes(&bytes).map_err(|e| e.to_string())?;
        let response = reply
            .as_dictionary()
            .and_then(|d| d.get("Response"))
            .and_then(Value::as_dictionary)
            .ok_or("Apple's answer has no Response")?;
        let status = response.get("Status").and_then(Value::as_dictionary).unwrap_or(response);
        match status.get("ec").and_then(Value::as_signed_integer) {
            Some(0) | None => Ok(response.clone()),
            Some(code) => Err(format!(
                "{} ({code})",
                status.get("em").and_then(Value::as_string).unwrap_or("Apple refused to register this PC")
            )),
        }
    }

    /// 일회용 암호 없이도 되는, 이 PC가 누구인지 말하는 헤더들 (등록할 때 씁니다). Apple은 이것들이
    /// 다 있어야 답합니다: 빠지면 401입니다.
    fn plain_headers(&self) -> [(&'static str, String); 9] {
        [
            ("Accept", "*/*".into()),
            ("User-Agent", crate::gsa::AGENT.to_string()),
            ("X-Apple-I-Client-Time", utc_now()),
            ("X-Apple-I-MD-LU", self.local_user.clone()),
            ("X-Apple-I-SRL-NO", "0".into()),
            ("X-Apple-I-TimeZone", time_zone()),
            ("X-Apple-Locale", self.locale.clone()),
            ("X-Mme-Device-Id", self.device.clone()),
            ("X-MMe-Client-Info", crate::gsa::CLIENT_INFO.to_string()),
        ]
    }

    /// 새 일회용 암호를 담은 anisette 헤더.
    pub fn headers(&mut self) -> Result<Vec<(&'static str, String)>, String> {
        let (md, mdm) = self.otp()?;
        Ok(vec![
            ("X-Apple-I-Client-Time", utc_now()),
            ("X-Apple-I-MD", md),
            ("X-Apple-I-MD-M", mdm),
            ("X-Apple-I-MD-LU", self.local_user.clone()),
            ("X-Apple-I-MD-RINFO", "17106176".into()),
            ("X-Apple-I-SRL-NO", "0".into()),
            ("X-Apple-I-TimeZone", time_zone()),
            ("X-Apple-Locale", self.locale.clone()),
            ("X-Mme-Device-Id", self.device.clone()),
        ])
    }

    /// GrandSlam 요청의 클라이언트 데이터(cpd)로 만든 헤더, iCloud for Windows의 로그인이 자신에
    /// 대해 말하는 것과 함께.
    pub fn client_data(&mut self) -> Result<plist::Dictionary, String> {
        let mut cpd: plist::Dictionary = self
            .headers()?
            .into_iter()
            .map(|(k, v)| (k.to_string(), plist::Value::String(v)))
            .collect();
        for (k, v) in [
            ("bootstrap", "true"),
            ("icscrec", "true"),
            ("loc", "en_GB"),
            ("pbe", "false"),
            ("prkgen", "true"),
            ("svct", "iCloud"),
        ] {
            cpd.insert(k.into(), v.into());
        }
        Ok(cpd)
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }
}

fn local_app_data() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
}

/// 사전에서 key 아래의 글.
fn text(dictionary: &Dictionary, key: &str) -> Result<String, String> {
    dictionary
        .get(key)
        .and_then(Value::as_string)
        .map(str::to_string)
        .ok_or(format!("Apple's answer has no {key}"))
}

/// iCloud for Windows를 풀어 둔 곳: APPLEID_ICLOUD, 없으면 %LOCALAPPDATA%\WirDrop\icloud.
fn icloud_home() -> PathBuf {
    if let Some(home) = std::env::var_os("APPLEID_ICLOUD") {
        return home.into();
    }
    local_app_data().join(r"WirDrop\icloud")
}

fn unpacked(home: &Path) -> bool {
    home.join(SUPPORT).join("objc.dll").is_file()
        && home.join(SERVICES).join("AOSKit.dll").is_file()
        && home.join(RUNTIME_2010).join("msvcr100.dll").is_file()
}

/// Apple에서 iCloudSetup.exe를 받아 확인하고, 그 안의 MSI 두 개를 Windows 자체의 expand와 msiexec
/// /a로 home에 풉니다 (파일만: 아무것도 설치하지 않고 관리자도 필요 없습니다).
async fn unpack_icloud(home: &Path) -> Result<(), String> {
    say!("downloading Apple iCloud for Windows 7.21 (about 160 MB, once)");
    std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
    let setup = crate::http::client()
        .get(ICLOUD_SETUP)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let digest: String = Sha256::digest(&setup).iter().map(|b| format!("{b:02x}")).collect();
    if digest != ICLOUD_SETUP_SHA256 {
        return Err("iCloudSetup.exe isn't the one expected".into());
    }
    // 설치 프로그램은 뒤에 캐비닛이 붙은 껍데기이고, 캐비닛 크기는 그 헤더에 있습니다.
    let at = setup
        .windows(8)
        .position(|w| w == b"MSCF\0\0\0\0")
        .ok_or("no cabinet in iCloudSetup.exe")?;
    let size = setup
        .get(at + 8..at + 12)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or("cabinet header")?;
    let cabinet = home.join("setup.cab");
    std::fs::write(&cabinet, setup.get(at..at + size).ok_or("short cabinet")?).map_err(|e| e.to_string())?;
    say!("unpacking iCloud for Windows");
    for (msi, into) in [("AppleApplicationSupport.msi", r"a\AAS32"), ("iCloud64.msi", r"a\iCloud64")] {
        let expanded = Command::new("expand.exe")
            .arg(format!("-F:{msi}"))
            .arg(&cabinet)
            .arg(home)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success());
        if !expanded || !home.join(msi).is_file() {
            return Err(format!("{msi} didn't come out of the cabinet"));
        }
        let status = Command::new("msiexec.exe")
            .arg("/a")
            .arg(home.join(msi))
            .arg("/qn")
            .raw_arg(format!("TARGETDIR=\"{}\"", home.join(into).display()))
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(home.join(msi));
        if !status.success() {
            return Err(format!("msiexec /a {msi}: {status}"));
        }
    }
    let _ = std::fs::remove_file(&cabinet);
    if unpacked(home) {
        Ok(())
    } else {
        Err("iCloud for Windows' files are missing after unpacking".into())
    }
}

/// HKCU\key의 InstallDir.
fn set_install_dir(key: &str, dir: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegSetValueExW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key_name, value, name) = (wide(key), wide(dir), wide("InstallDir"));
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: NUL로 끝나는 문자열. 키는 설정한 뒤 한 번 닫습니다.
    unsafe {
        let made = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        );
        if made != 0 {
            return Err(format!("registry {key}: error {made}"));
        }
        let set = RegSetValueExW(handle, name.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32);
        RegCloseKey(handle);
        if set != 0 {
            return Err(format!("registry {key}: error {set}"));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// PC의 기기 ID와 자기 로컬 사용자. 한 번 만들어 %LOCALAPPDATA%\WirDrop\device.txt에 두므로 PC는
/// Apple이 아는 같은 기기로 남습니다.
fn device() -> Result<(String, String), String> {
    let ours = local_app_data().join(r"WirDrop\device.txt");
    if let Ok(text) = std::fs::read_to_string(&ours) {
        let mut fields = text.split_whitespace();
        if let (Some(device), Some(user)) = (fields.next(), fields.next()) {
            return Ok((device.to_string(), user.to_string()));
        }
    }
    let id = hex(&rand::random::<[u8; 16]>());
    let device = format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..]);
    let user = hex(&rand::random::<[u8; 32]>());
    if let Some(dir) = ours.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&ours, format!("{device} {user}\n")).map_err(|e| e.to_string())?;
    Ok((device, user))
}

/// 이 로그인 자체의 로컬 사용자 (있으면): Mac에서처럼 이 PC의 다른 사람 Apple ID는 이 PC의 다른
/// 사용자입니다. APPLEID_SEPARATE_USER면 처음에 하나 만듭니다.
fn sign_in_user(out: &Path) -> Result<Option<String>, String> {
    let path = out.join(LOCAL_USER);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let user = text.trim().to_ascii_uppercase();
        if user.len() >= 32 && user.len() % 2 == 0 && user.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Some(user));
        }
    }
    if std::env::var_os("APPLEID_SEPARATE_USER").is_none() {
        return Ok(None);
    }
    let user = hex(&rand::random::<[u8; 32]>());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &user).map_err(|e| e.to_string())?;
    say!("this Apple ID gets a local user of its own on this PC");
    Ok(Some(user))
}

/// 이 사용자의 Windows 로캘, Apple이 쓰는 꼴로 ("ko_KR").
fn locale() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut name = [0u16; 85];
    // SAFETY: 버퍼는 말한 길이입니다.
    let n = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    let name = String::from_utf16_lossy(&name[..(n.max(1) - 1) as usize]).replace('-', "_");
    if name.is_empty() { "en_US".into() } else { name }
}

/// 모두가 쓰는 약어가 있는 시간대 오프셋이면 그 약어.
fn time_zone() -> String {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // SAFETY: 지역 변수로 하는 평범한 조회.
    let bias = unsafe {
        let mut info: TIME_ZONE_INFORMATION = std::mem::zeroed();
        GetTimeZoneInformation(&mut info);
        info.Bias
    };
    match bias {
        -540 => "KST".into(),
        0 => "UTC".into(),
        b => format!("GMT{}{:02}:{:02}", if b <= 0 { '+' } else { '-' }, b.abs() / 60, b.abs() % 60),
    }
}

/// 지금, "2026-10-28T07:20:31Z" 꼴로.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // 1970년부터의 일 수를 날짜로 (H. Hinnant의 알고리즘).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_times_as_apple_does() {
        let now = super::utc_now();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T');
    }
}
