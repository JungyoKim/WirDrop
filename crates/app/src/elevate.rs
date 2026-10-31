//! 관리자 권한이 필요한 일과, Windows 로그인 때 시작하기.
//!
//! 엔진(Wintun의 awdl0, 방화벽 규칙)은 관리자 권한으로 돕니다. 보통은 설치 때 만든 WirDropEngine
//! 서비스로 띄우고, 서비스가 없으면 트레이 앱이 UAC로 띄워 트레이 앱이 끝날 때까지 둡니다.
//! WirDrop은 세션 사이에 관리자 권한을 쥐고 있지 않습니다 (가장 높은 권한의 예약 작업 같은 것):
//! 서명 없는 프로그램이 그러면 Defender가 악성 지속 장치로 보는 것이 옳습니다.
//!
//! 탐색기 메뉴는 서명 없는 스파스 패키지 WirDrop.ContextMenu.msix입니다. 관리자가 -AllowUnsigned로
//! 등록하면 Windows가 받아 주므로 (COM 서버가 실행 파일 활성화라서) 인증서를 만들거나 신뢰하거나
//! 어디에 두지 않습니다.
use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

use crate::win::hidden;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const PACKAGE: &str = "WirDrop.ContextMenu";
pub const MSIX: &str = "WirDrop.ContextMenu.msix";
/// 사용자가 UAC 창에서 거절했습니다.
pub const ERROR_CANCELLED: i32 = 1223;

/// WirDrop.exe를 args와 함께 UAC로 실행합니다. wait면 끝날 때까지 기다려 종료 코드를 돌려줍니다.
/// 사용자가 거절하면 ERROR_CANCELLED로 실패합니다.
pub fn run_elevated(args: &str, wait: bool) -> io::Result<u32> {
    let exe = HSTRING::from(crate::paths::exe().as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: info와 그 문자열은 호출보다 오래 살고, 프로세스 핸들은 아래에서 닫습니다.
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))?;
        let mut code = 0;
        if wait && !info.hProcess.is_invalid() && WaitForSingleObject(info.hProcess, INFINITE) == WAIT_OBJECT_0 {
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
        if !info.hProcess.is_invalid() {
            let _ = CloseHandle(info.hProcess);
        }
        Ok(code)
    }
}

/// 엔진을 관리자 권한으로 띄웁니다. 사용자가 창에 답하면 돌아옵니다.
pub fn start_engine() -> io::Result<()> {
    // 서비스를 설정하는 데 UAC 창이 한 번(보통 설치 프로그램의 것) 필요하고, 아직 WinUSB가 아닌
    // 어댑터(처음 꽂은 것)도 한 번 필요합니다. 그 뒤로는 없습니다.
    let mut setup = Vec::new();
    if !crate::service::current() {
        setup.push("--service on");
    }
    if crate::driver::needs_binding() {
        setup.push("--driver on");
    }
    if !setup.is_empty() {
        match run_elevated(&format!("--setup {}", setup.join(" ")), true)? {
            0 => {}
            code => {
                return Err(io::Error::other(tf!(
                    "서비스 설치가 {code} 코드로 끝났습니다 (setup.log)",
                    "Service setup ended with code {code} (setup.log)"
                )));
            }
        }
    }
    crate::service::start(crate::ipc::current_session()).or_else(|e| {
        tracing::warn!("can't start the service, so the engine directly: {e}");
        run_elevated("--engine", false).map(drop)
    })
}

/// WirDrop.exe --setup [--menu on|off] [--service on|off] [--driver on|off|probe]을 관리자
/// 권한으로: 탐색기 메뉴, 엔진 서비스, WinUSB 위의 어댑터 (driver.rs).
pub fn setup_main(args: &[String]) -> io::Result<()> {
    crate::paths::init_log("setup");
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str);
    let (menu, service, driver) = (value("--menu"), value("--service"), value("--driver"));
    // 앞의 것이 어찌 되었든 요청한 부분을 모두 합니다 (설치는 셋을 한꺼번에 요청합니다). 결과는 첫
    // 실패입니다.
    let results = [
        match service {
            Some("on") => crate::service::install(),
            Some("off") => crate::service::remove(),
            _ => Ok(()),
        },
        match driver {
            Some("on") => crate::driver::bind().map(drop),
            Some("off") => crate::driver::unbind().map(drop),
            Some("probe") => crate::driver::probe(),
            _ => Ok(()),
        },
        match menu {
            Some("on") => install_menu(),
            Some("off") => remove_menu(),
            _ => Ok(()),
        },
    ];
    for e in results.iter().filter_map(|r| r.as_ref().err()) {
        tracing::error!("setup: {e}");
    }
    let result = results.into_iter().find(Result::is_err).unwrap_or(Ok(()));
    if result.is_ok() {
        tracing::info!("setup done (menu: {menu:?}, service: {service:?}, driver: {driver:?})");
    }
    result
}

fn powershell(script: &str) -> io::Result<()> {
    let out = hidden("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

fn install_dir() -> PathBuf {
    crate::paths::exe().parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Windows 11 탐색기 메뉴에 AirDrop을 넣는 스파스 패키지를 관리자 권한으로 등록합니다. 이 폴더의
/// WirDrop.exe와 wirdrop_shell.dll을 가리키므로 폴더가 옮겨지면 다시 등록해야 합니다.
pub fn install_menu() -> io::Result<()> {
    let dir = install_dir();
    let msix = dir.join(MSIX);
    if !msix.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is missing", msix.display())));
    }
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         Get-AppxPackage -Name {PACKAGE} | Remove-AppxPackage; \
         Add-AppxPackage -Path {} -ExternalLocation {} -AllowUnsigned -ForceApplicationShutdown",
        ps_quote(&msix),
        ps_quote(&dir)
    );
    powershell(&script)?;
    tracing::info!("Explorer menu package registered from {}", dir.display());
    Ok(())
}

pub fn remove_menu() -> io::Result<()> {
    powershell(&format!("Get-AppxPackage -Name {PACKAGE} | Remove-AppxPackage"))
}

/// 메뉴 패키지가 이 사용자에게 등록되어 있습니다.
pub fn menu_installed() -> bool {
    registered_menu().is_some()
}

/// 등록된 메뉴 패키지가 WirDrop.exe 옆의 것과 버전이 다릅니다: 업데이트가 등록 내용(AirDrop이 뜨는
/// 항목)을 바꿨으니 다시 등록해야 합니다. Windows는 등록할 때의 매니페스트를 그대로 둡니다.
pub fn menu_outdated() -> bool {
    let Some(registered) = registered_menu() else { return false };
    let manifest = std::fs::read_to_string(install_dir().join(r"package\AppxManifest.xml")).unwrap_or_default();
    let staged = manifest
        .split("<Identity")
        .nth(1)
        .and_then(|s| s.split("Version=\"").nth(1))
        .and_then(|s| s.split('"').next());
    // 등록된 것의 전체 이름: Name_Version_Architecture__PublisherId.
    staged.is_some_and(|v| registered.split('_').nth(1) != Some(v))
}

/// 이 사용자에게 등록된 메뉴 패키지의 전체 이름.
fn registered_menu() -> Option<String> {
    const REPO: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
    let key = windows_registry::CURRENT_USER.open(REPO).ok()?;
    let mut keys = key.keys().ok()?;
    keys.find(|k| k.starts_with(&format!("{PACKAGE}_")))
}

pub fn autostart() -> bool {
    windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|k| k.get_string("WirDrop"))
        .is_ok_and(|cmd| {
            cmd.to_ascii_lowercase()
                .contains(&crate::paths::exe().display().to_string().to_ascii_lowercase())
        })
}

pub fn set_autostart(on: bool) -> io::Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if on {
        key.set_string("WirDrop", format!("\"{}\" --autostart", crate::paths::exe().display()))?;
    } else {
        let _ = key.remove_value("WirDrop");
    }
    Ok(())
}
