//! Windows 서비스로 도는 엔진. awdl0(Wintun 어댑터)에 필요한 권한을 매번 UAC 없이 얻습니다. 설치가
//! 관리자 권한으로 한 번 등록합니다: 엔진 파일은 관리자만 바꿀 수 있는 Program Files에 두고 (SYSTEM
//! 서비스가 무엇을 돌릴지는 관리자만 정합니다), 로그인한 누구나 시작하고 멈출 수 있습니다. 트레이가
//! 자기 세션과 데이터 폴더로 시작하면 서비스는 그 트레이를 위해 엔진을 평소처럼 돌리고, 트레이가
//! 놓아 주면 멈춥니다.
//!
//! --update로 시작하면 대신 더 새 릴리스를 놓고(update.rs) 멈춥니다: 업데이트에도 Program Files에
//! 대한 같은 권한이 필요하고, 창은 띄우지 않습니다.
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceStartType, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const NAME: &str = "WirDropEngine";
/// 엔진이 함께 쓰는 파일. Program Files에 엔진 옆으로 복사합니다.
const FILES: [&str; 3] = ["WirDrop.exe", "usbfwd.exe", "wintun.dll"];
/// SYSTEM, 관리자: 모든 권한. 대화형 사용자: 조회, 시작, 멈춤.
const SDDL: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPLOCRRC;;;IU)";
/// 업데이트 실행이 끝난 방식(종료 코드): 더 새 릴리스를 놓았거나, 없었거나.
pub const UPDATED: u32 = 0;
pub const UP_TO_DATE: u32 = 3;

// ---- 서비스로 돌 때

define_windows_service!(ffi_main, service_main);

static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();

/// 서비스 제어 관리자가 시작하는 WirDrop.exe --service.
pub fn main() -> io::Result<()> {
    service_dispatcher::start(NAME, ffi_main).map_err(io::Error::other)
}

fn service_main(args: Vec<OsString>) {
    // 트레이가 준 시작 인자 (서비스 이름 뒤): --session N --local-app-data DIR.
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    if let Some(dir) = value("--local-app-data") {
        // 로그, AWDL 주소, Apple ID 신원은 사용자 폴더에 있습니다.
        // SAFETY: 아직 우리 것이 아무것도 돌지 않습니다.
        unsafe { std::env::set_var("LOCALAPPDATA", &dir) };
        crate::paths::set_data_dir(std::path::PathBuf::from(dir).join("WirDrop"));
    }
    if let Some(session) = value("--session").and_then(|s| s.parse().ok()) {
        crate::ipc::set_session(session);
    }
    let handler = |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            // usbfwd는 닫히면 끝나는 작업 개체 안에 있고, awdl0은 프로세스와 함께 사라집니다.
            report(ServiceState::Stopped, 0);
            std::process::exit(0)
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(status) = service_control_handler::register(NAME, handler) else {
        return;
    };
    let _ = STATUS.set(status);
    report(ServiceState::Running, 0);
    // 앞선 업데이트가 남긴 것: 옆으로 비켜 둔 파일들 (아무도 잡고 있지 않을 때).
    crate::update::tidy(&install_dir());
    if args.iter().any(|a| a == "--update") {
        crate::paths::init_log("update");
        let code = match crate::update::apply(&install_dir()) {
            Ok(true) => UPDATED,
            Ok(false) => UP_TO_DATE,
            Err(e) => {
                tracing::error!("update failed: {e}");
                1
            }
        };
        report(ServiceState::Stopped, code);
        return;
    }
    crate::paths::init_log("engine");
    tracing::info!(
        "engine service for session {:?}, data in {}",
        value("--session"),
        crate::paths::data_dir().display()
    );
    let code = match crate::engine::main() {
        Ok(()) => 0,
        Err(_) => 1,
    };
    report(ServiceState::Stopped, code);
}

fn report(state: ServiceState, code: u32) {
    let Some(handle) = STATUS.get() else { return };
    let accept = if state == ServiceState::Running {
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
    } else {
        ServiceControlAccept::empty()
    };
    let _ = handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
}

// ---- 트레이를 위해

/// 서비스가 있고 바로 이 파일들을 돌립니다 (아니면 설치가 최신으로 맞춰야 합니다).
pub fn current() -> bool {
    let installed = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service(NAME, ServiceAccess::QUERY_STATUS))
        .is_ok();
    let here = crate::paths::exe();
    let here = here.parent().unwrap_or(Path::new("."));
    installed && FILES.iter().all(|f| same(&here.join(f), &install_dir().join(f)))
}

/// 이 세션의 트레이를 위해 서비스를 시작합니다. 이미 돌고 있으면 접속하는 중이니 할 일이 없습니다.
pub fn start(session: u32) -> io::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(io::Error::other)?;
    let service = manager
        .open_service(NAME, ServiceAccess::START | ServiceAccess::QUERY_STATUS)
        .map_err(io::Error::other)?;
    if service.query_status().map_err(io::Error::other)?.current_state != ServiceState::Stopped {
        return Ok(());
    }
    let local = std::env::var("LOCALAPPDATA").map_err(io::Error::other)?;
    service
        .start(&["--session", &session.to_string(), "--local-app-data", &local])
        .map_err(io::Error::other)
}

/// 이 WirDrop.exe가 Program Files에 설치된 것입니다 (그래서 업데이트가 바꾸는 것).
pub fn installed_here() -> bool {
    let here = crate::paths::exe();
    let here = here.parent().unwrap_or(Path::new("."));
    here.to_string_lossy().eq_ignore_ascii_case(&install_dir().to_string_lossy())
}

/// 엔진이 놓아 준 뒤 WirDrop을 업데이트하려고 서비스를 한 번 더 돌리고 ([main] 참고) 끝날 때까지
/// 기다립니다: 종료 코드 [UPDATED], [UP_TO_DATE], 또는 실패.
pub fn update(session: u32) -> io::Result<u32> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(io::Error::other)?;
    let access = ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS;
    let service = manager.open_service(NAME, access).map_err(io::Error::other)?;
    let stopped = |s: &windows_service::service::Service| s.query_status().is_ok_and(|s| s.current_state == ServiceState::Stopped);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !stopped(&service) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
    if !stopped(&service) {
        stop(&service);
    }
    let local = std::env::var("LOCALAPPDATA").map_err(io::Error::other)?;
    service
        .start(&["--session", &session.to_string(), "--local-app-data", &local, "--update"])
        .map_err(io::Error::other)?;
    // 수십 MB 다운로드와 파일 바꾸기.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let status = service.query_status().map_err(io::Error::other)?;
        if status.current_state == ServiceState::Stopped {
            return Ok(match status.exit_code {
                ServiceExitCode::Win32(code) => code,
                ServiceExitCode::ServiceSpecific(code) => code,
            });
        }
        if Instant::now() > deadline {
            stop(&service);
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the update took too long"));
        }
    }
}

// ---- 설치, 관리자 권한으로

/// Program Files\WirDrop.
pub fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into())).join("WirDrop")
}

/// 엔진 파일을 Program Files로 복사하고 그것을 돌리는 서비스를 등록합니다.
pub fn install() -> io::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(io::Error::other)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;
    // 돌고 있는 엔진이 파일을 잡고 있습니다.
    if let Ok(service) = manager.open_service(NAME, access) {
        stop(&service);
    }
    let from = crate::paths::exe();
    let from = from.parent().unwrap_or(Path::new("."));
    let to = install_dir();
    std::fs::create_dir_all(&to)?;
    // 설치 프로그램으로 설치했으면 이미 거기서 돕니다.
    if !installed_here() {
        for f in FILES {
            std::fs::copy(from.join(f), to.join(f)).map_err(|e| io::Error::new(e.kind(), format!("{f}: {e}")))?;
        }
    }
    let info = ServiceInfo {
        name: NAME.into(),
        display_name: "WirDrop".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::OnDemand,
        error_control: ServiceErrorControl::Normal,
        executable_path: to.join("WirDrop.exe"),
        launch_arguments: vec!["--service".into()],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = match manager.open_service(NAME, access) {
        Ok(service) => {
            service.change_config(&info).map_err(io::Error::other)?;
            service
        }
        Err(_) => manager.create_service(&info, access).map_err(io::Error::other)?,
    };
    let _ = service.set_description("WirDrop의 AirDrop 엔진: USB 무선 어댑터와 가상 네트워크(awdl0)를 다룹니다");
    let out = crate::win::hidden("sc.exe").args(["sdset", NAME, SDDL]).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("sc sdset: {}", String::from_utf8_lossy(&out.stdout).trim())));
    }
    tracing::info!("service installed in {}", to.display());
    Ok(())
}

/// 서비스를 멈추고 지우고, 설치가 그것을 위해 복사한 파일도 지웁니다 (설치 프로그램이 설치한 것은
/// 제거 프로그램이 지웁니다).
pub fn remove() -> io::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(io::Error::other)?;
    if let Ok(service) = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE) {
        stop(&service);
        service.delete().map_err(io::Error::other)?;
    }
    if !installed_here() {
        let _ = std::fs::remove_dir_all(install_dir());
    }
    Ok(())
}

fn stop(service: &windows_service::service::Service) {
    let _ = service.stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.query_status().is_ok_and(|s| s.current_state != ServiceState::Stopped) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 내용이 같은 두 파일.
fn same(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => std::fs::read(a).ok() == std::fs::read(b).ok(),
        _ => false,
    }
}
