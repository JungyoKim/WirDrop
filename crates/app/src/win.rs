//! 트레이 앱이 쓰는 작은 Win32 도우미들.
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// usbfwd가 다룰 수 있는 USB 어댑터: 제조사, 제품, 이름. 둘 다 꽂혀 있으면 앞의 것을 씁니다
/// (usbfwd도 RTL8912AU를 먼저 고릅니다).
pub const ADAPTERS: &[(u16, u16, &str)] = &[
    (0x0bda, 0x8912, "Realtek RTL8912AU (ipTIME BE6500UA)"),
    (0x2357, 0x012e, "TP-Link Archer T3U Nano"),
];

/// 방해 금지(또는 집중 세션)가 켜져 있습니다: Windows는 알림을 배너 없이 알림 센터에 넣습니다.
pub fn do_not_disturb() -> bool {
    use windows::Win32::UI::Shell::{QUNS_QUIET_TIME, SHQueryUserNotificationState};
    // SAFETY: 평범한 조회.
    quiet_hours().is_some_and(|profile| profile != 0) || unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_QUIET_TIME)
}

/// 무언가 화면을 차지하고 있습니다: 전체 화면 앱이나 게임, 또는 발표. 그때는 Windows도 배너를
/// 미루고, 메뉴가 튀어나오면 방해가 됩니다.
pub fn full_screen() -> bool {
    // 아일랜드의 판단 (Windows의 "바쁨"은 그렇지 않은 창에도 그렇다고 합니다).
    wirdrop_island::full_screen()
}

/// 방해 금지가 정하는 셸의 조용한 시간 프로필: 0 꺼짐, 1 우선순위만, 2 알람만. Windows에 API가 없어
/// 셸이 WNF 상태로 내놓는 것을 읽습니다.
fn quiet_hours() -> Option<u32> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryWnfStateData(
            name: *const u64,
            type_id: *const std::ffi::c_void,
            scope: *const std::ffi::c_void,
            change_stamp: *mut u32,
            buffer: *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    /// WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED.
    const PROFILE: u64 = 0x0D83_063E_A3BF_1C75;
    let (mut stamp, mut profile, mut size) = (0u32, 0u32, 4u32);
    // SAFETY: 이름과 출력 값은 호출보다 오래 살고, 버퍼는 size가 말한 대로 4바이트입니다.
    let status = unsafe {
        NtQueryWnfStateData(
            &PROFILE,
            std::ptr::null(),
            std::ptr::null(),
            &mut stamp,
            (&raw mut profile).cast(),
            &mut size,
        )
    };
    (status == 0 && size == 4).then_some(profile)
}

/// 고른 모델이 없을 때 iPhone에 보일 모델 (settings::MODELS 참고): 배터리가 있는 PC는 MacBook Pro,
/// 없으면 iMac.
pub fn auto_model() -> &'static str {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    const NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;
    let mut power = SYSTEM_POWER_STATUS::default();
    // SAFETY: 출력 인자 하나.
    let known = unsafe { GetSystemPowerStatus(&mut power) }.is_ok();
    let battery = known && power.BatteryFlag != UNKNOWN && power.BatteryFlag & NO_BATTERY == 0;
    if battery { "Mac17,2" } else { "Mac16,3" }
}

/// 이 사용자에게서 Windows가 HEVC 동영상과 HEIF 사진을 열 수 있는지: 스토어의 HEVC 비디오
/// 확장(제조사 제공이나 유료)과 HEIF 이미지 확장이 설치되어 있습니다.
pub fn media_extensions() -> (bool, bool) {
    let packages = windows_registry::CURRENT_USER
        .open(r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages")
        .and_then(|key| key.keys().map(|names| names.collect::<Vec<String>>()))
        .unwrap_or_default();
    let has = |prefix: &str| packages.iter().any(|p| p.starts_with(prefix));
    (has("Microsoft.HEVCVideoExtension"), has("Microsoft.HEIFImageExtension"))
}

/// 꽂혀 있는 지원 어댑터 가운데 [ADAPTERS]에서 가장 앞의 것.
pub fn find_adapter() -> Option<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CR_SUCCESS,
    };
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    // SAFETY: 버퍼는 크기 호출이 말한 길이입니다.
    let ids = unsafe {
        if CM_Get_Device_ID_List_SizeW(&mut len, w!("USB"), flags) != CR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(w!("USB"), &mut buf, flags) != CR_SUCCESS {
            return None;
        }
        String::from_utf16_lossy(&buf)
    };
    let ids = ids.to_ascii_uppercase();
    ADAPTERS
        .iter()
        .find(|(vid, pid, _)| ids.contains(&format!("VID_{vid:04X}&PID_{pid:04X}")))
        .map(|(_, _, name)| name.to_string())
}

/// 사용자가 Windows 설정에서 고른 국가 또는 지역 (ISO 3166 두 글자, 예: "KR"). RTL8912AU의 송신
/// 출력 한도를 그 나라 규제에 맞춥니다.
pub fn country() -> Option<String> {
    use windows::Win32::Globalization::GetUserDefaultGeoName;
    let mut buf = [0u16; 16];
    // SAFETY: 크기를 준 지역 버퍼.
    let n = unsafe { GetUserDefaultGeoName(&mut buf) };
    let name = String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1).min(buf.len())]);
    (name.len() == 2 && name.bytes().all(|b| b.is_ascii_alphabetic())).then(|| name.to_ascii_uppercase())
}

/// 콘솔 창 없는 명령.
pub fn hidden(program: &str) -> Command {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// WinUI는 그리는 그래픽 어댑터와 다른 어댑터가 맡은 화면에서 메뉴의 바뀌는 글자를 잘못 그립니다:
/// 앞의 내용 조각이 비쳐 보입니다 (NVIDIA 카드가 내장 Intel GPU의 화면에 그릴 때 보였고, Intel로
/// 그리면 두 화면 모두 바르게 나옵니다). 화면이 여러 어댑터에 붙어 있으면, 사용자가 직접 고르지
/// 않았다면 (설정: 시스템 > 디스플레이 > 그래픽) WirDrop은 Windows에 절전(내장) 어댑터를 청합니다.
/// Windows는 프로세스가 첫 그래픽 장치를 만들 때 이를 읽으므로 메뉴와 아일랜드보다 먼저 합니다.
pub fn prefer_power_saving_gpu() {
    if display_adapters() < 2 {
        return;
    }
    let Ok(key) = windows_registry::CURRENT_USER.create(r"Software\Microsoft\DirectX\UserGpuPreferences") else {
        return;
    };
    let exe = crate::paths::exe().display().to_string();
    if key.get_string(&exe).is_err() && key.set_string(&exe, "GpuPreference=1;").is_ok() {
        tracing::info!("graphics: the power-saving adapter, for menus that come out right on every screen");
    }
}

/// 데스크톱에 화면이 붙은 그래픽 어댑터 수.
fn display_adapters() -> usize {
    use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, EnumDisplayDevicesW};
    let mut adapters = Vec::new();
    for i in 0.. {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: 호출에 맞는 크기의 구조체.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut device, 0) }.as_bool() {
            break;
        }
        if device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == DISPLAY_DEVICE_ATTACHED_TO_DESKTOP {
            let id = &device.DeviceID;
            adapters.push(String::from_utf16_lossy(&id[..id.iter().position(|&c| c == 0).unwrap_or(id.len())]));
        }
    }
    adapters.sort();
    adapters.dedup();
    adapters.len()
}

/// 파일, 폴더, URL을 기본 앱으로 엽니다.
pub fn open(target: &str) {
    // SAFETY: 평범한 문자열.
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// 탐색기에서 파일을 선택해 보여 줍니다.
pub fn reveal(path: &Path) {
    let _ = Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

pub enum Pick {
    Files,
    Folder,
}

/// 공용 파일 대화상자를 자기 STA 스레드에서 띄우고 닫힐 때까지 막힙니다. owner는 모달로 붙을 창
/// 핸들(isize) 또는 0. owner의 스레드나 계속 응답해야 하는 창을 돌리는 스레드에서는 부르지 마세요:
/// 대화상자는 owner 스레드를 기다리고 그 스레드는 여기서 기다립니다.
pub fn pick(owner: isize, title: &str, kind: Pick) -> Vec<PathBuf> {
    pick_in(owner, title, kind, None)
}

/// [pick]과 같되 folder에서 엽니다.
pub fn pick_in(owner: isize, title: &str, kind: Pick, folder: Option<&Path>) -> Vec<PathBuf> {
    let title = title.to_string();
    let start = folder.map(Path::to_path_buf);
    let folder = matches!(kind, Pick::Folder);
    std::thread::spawn(move || {
        // SAFETY: 이 스레드에서만 쓰고 끝나기 전에 놓는 COM.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let paths = dialog(owner, &title, folder, start.as_deref()).unwrap_or_default();
            CoUninitialize();
            paths
        }
    })
    .join()
    .unwrap_or_default()
}

unsafe fn dialog(owner: isize, title: &str, folder: bool, start: Option<&Path>) -> windows::core::Result<Vec<PathBuf>> {
    // SAFETY: 부르는 쪽이 COM을 초기화했고, 문자열은 호출보다 오래 삽니다.
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let mut options = d.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        if folder {
            options |= FOS_PICKFOLDERS;
        } else {
            options |= FOS_ALLOWMULTISELECT;
        }
        d.SetOptions(options)?;
        d.SetTitle(&HSTRING::from(title))?;
        if let Some(start) = start
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None)
        {
            let _ = d.SetFolder(&item);
        }
        let owner = (owner != 0).then_some(HWND(owner as _));
        if d.Show(owner).is_err() {
            return Ok(Vec::new());
        }
        let items = d.GetResults()?;
        let mut paths = Vec::new();
        for i in 0..items.GetCount()? {
            let name = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            if let Ok(s) = name.to_string() {
                paths.push(PathBuf::from(s));
            }
            CoTaskMemFree(Some(name.0 as _));
        }
        Ok(paths)
    }
}

/// "12.3 MB"
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// exe를 돌리는 프로세스를 모두 끝냅니다: 죽은 엔진이 어댑터를 쥔 채 남긴 usbfwd. 끝낸 수를
/// 돌려줍니다.
pub fn kill_running(exe: &Path) -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
    };
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()) else {
        return 0;
    };
    let want = exe.to_string_lossy().to_ascii_lowercase();
    let mut ended = 0;
    // SAFETY: 크기를 맞춘 항목으로 프로세스 스냅숏을 돌고, 프로세스 핸들은 닫습니다.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return 0;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase() == name
                && let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, entry.th32ProcessID)
            {
                let mut buf = [0u16; 1024];
                let mut n = buf.len() as u32;
                if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n).is_ok()
                    && String::from_utf16_lossy(&buf[..n as usize]).to_ascii_lowercase() == want
                    && TerminateProcess(process, 1).is_ok()
                {
                    ended += 1;
                }
                let _ = CloseHandle(process);
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    ended
}
