//! 알림 영역 앱: 주 스레드에서 트레이 아이콘을 가진 숨은 창, WinUI 스레드의 메뉴, 그 옆 tokio
//! 런타임의 허브. WirDrop에는 자체 창이 없습니다. 메뉴, 탐색기 메뉴, 아일랜드가 전부이고, 필요한
//! 곳에서만 Windows 기본 대화상자(파일, 폴더)를 씁니다.
mod cards;
pub mod hub;
mod menu;
pub mod rate;
mod text;
mod view;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{self, UnboundedSender};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoIncrementMTAUsage, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, HICON, KillTimer,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    SetMenuDefaultItem, SetTimer, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_CONTEXTMENU, WM_DEVICECHANGE, WM_NULL, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCSTR, PCWSTR, w};

use crate::ipc::{Conn, Msg, Role, TargetRef};
use hub::{HubEvent, MenuCmd};
use view::{Change, Device, Snapshot};

/// 트레이 아이콘과 그 메뉴가 보여 주는 것. 허브가 채웁니다.
#[derive(Clone, Default)]
pub struct TrayView {
    pub tooltip: String,
    pub status: String,
    pub ready: bool,
    pub receive: bool,
    pub autostart: bool,
    /// 시작에 사용자가 필요합니다: 상태 줄을 고르면 엔진을 시작합니다.
    pub startable: bool,
    /// 메뉴가 내보이는 모든 것.
    pub snapshot: Option<Box<Snapshot>>,
}

/// 허브가 창 스레드에 닿는 길.
pub struct UiHandle {
    hwnd: isize,
    view: Arc<Mutex<TrayView>>,
    share: ShareRequest,
    notice: NoticeRequest,
}

/// 기기 메뉴에 넘길 파일: 허브에서 창 스레드로.
type ShareRequest = Arc<Mutex<Option<Vec<std::path::PathBuf>>>>;

/// 아이콘에 혼자 여는 메뉴 (아일랜드가 없을 때 받기 요청의 질문이나 일어난 일)와 머무는 시간:
/// 허브에서 창 스레드로.
type NoticeRequest = Arc<Mutex<Option<(Vec<wirdrop_menu::Entry>, std::time::Duration)>>>;

impl UiHandle {
    pub fn update(&self, view: TrayView) {
        *self.view.lock().unwrap() = view;
        // SAFETY: 우리 창에 보냅니다.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_REFRESH, WPARAM(0), LPARAM(0)) };
    }

    pub fn quit(&self) {
        // SAFETY: 위와 같음.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_QUIT_APP, WPARAM(0), LPARAM(0)) };
    }

    /// 트레이 아이콘에서 메뉴를 엽니다.
    pub fn show_menu(&self) {
        // SAFETY: 위와 같음.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_SHOW_MENU, WPARAM(0), LPARAM(0)) };
    }

    /// 파일을 보낼 기기 메뉴를 마우스 자리에 엽니다 (AirDrop을 고른 탐색기 메뉴는 닫혔으니 어느
    /// 기기든 조금만 움직이면 됩니다): 주변 기기들이 오가는 대로 바뀝니다.
    pub fn share(&self, files: Vec<std::path::PathBuf>) {
        *self.share.lock().unwrap() = Some(files);
        // SAFETY: 위와 같음.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_SHARE, WPARAM(0), LPARAM(0)) };
    }

    /// entries를 트레이 아이콘에 메뉴로 혼자, 길어야 stay 동안 엽니다: 아일랜드가 없을 때 카드가
    /// 했을 말. 키는 주 메뉴의 것입니다 (카드 버튼은 press:…). 사용자가 하던 일에서 포커스를 빼앗지
    /// 않고 열리므로 다른 곳을 눌러도 안 닫힐 수 있어 stay가 닫습니다.
    pub fn show_entries(&self, entries: Vec<wirdrop_menu::Entry>, stay: std::time::Duration) {
        *self.notice.lock().unwrap() = Some((entries, stay));
        // SAFETY: 위와 같음.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_NOTICE, WPARAM(0), LPARAM(0)) };
    }

    /// [UiHandle::show_entries]가 연 메뉴가 아직 열려 있으면 닫습니다.
    pub fn close_entries(&self) {
        // SAFETY: 위와 같음.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_TIMER, WPARAM(NOTICE_TIMER), LPARAM(0)) };
    }
}

const WM_TRAY: u32 = WM_APP + 1;
const WM_REFRESH: u32 = WM_APP + 2;
const WM_QUIT_APP: u32 = WM_APP + 3;
/// Fluent 메뉴 항목을 골랐습니다: 키는 Ctx::picks에 있습니다.
const WM_MENU_PICK: u32 = WM_APP + 4;
const WM_SHOW_MENU: u32 = WM_APP + 5;
/// 공유할 파일: 요청은 Ctx::share_request에 있습니다.
const WM_SHARE: u32 = WM_APP + 6;
/// 아이콘에 혼자 열 메뉴: 항목은 Ctx::notice_request에 있습니다.
const WM_NOTICE: u32 = WM_APP + 8;
/// 그런 메뉴를 닫는 타이머.
const NOTICE_TIMER: usize = 1;
const DBT_DEVNODES_CHANGED: usize = 7;
/// NIN_SELECT | NINF_KEY: 키보드로 아이콘을 골랐습니다.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

const ID_OPEN_DIR: usize = 2;
const ID_RECEIVE: usize = 3;
const ID_AUTOSTART: usize = 4;
const ID_RECONNECT: usize = 6;
const ID_EXIT: usize = 8;

struct Ctx {
    events: UnboundedSender<HubEvent>,
    view: Arc<Mutex<TrayView>>,
    taskbar_created: u32,
    icons: [HICON; 2],
    /// Fluent 메뉴. WinUI가 시작하지 못하면 None이고 고전 메뉴를 씁니다.
    menu: Option<wirdrop_menu::Menu>,
    /// 열린 Fluent 메뉴를 만든 상태: 메뉴의 키가 이것을 가리킵니다.
    shown: TrayView,
    picks: Arc<Mutex<VecDeque<String>>>,
    /// Windows가 알려 주지 못할 때 (Shell_NotifyIconGetRect가 가끔 실패합니다) 쓸, 아이콘을
    /// 마지막으로 찾은 자리.
    icon_at: Option<(i32, i32)>,
    share_request: ShareRequest,
    notice_request: NoticeRequest,
    /// [UiHandle::show_entries]가 마지막으로 연 메뉴의 태그.
    notice_tag: Option<u64>,
    /// 열려 있을 수도 있는 기기 메뉴.
    share: Option<Share>,
    /// 마지막 기기 메뉴의 태그 (wirdrop_menu::Menu::show_tagged 참고).
    share_tag: u64,
}

/// 기기 메뉴: 보낼 파일, 그리고 키의 번호로 가리키는 기기들.
struct Share {
    files: Vec<std::path::PathBuf>,
    devices: Vec<Device>,
    /// 그 밖에 보여 줄 것: 준비되지 않았을 때의 상태 줄.
    note: String,
    tag: u64,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

/// 모드 없는 WirDrop.exe, 또는 --pick FILE...: 트레이 앱. 파일을 주면 보낼 기기를 고르게 합니다. 두
/// 번째 실행은 첫 실행에 요청을 넘깁니다.
pub fn main(args: &[String]) -> io::Result<()> {
    let quiet = args.iter().any(|a| a == "--autostart");
    let pick: Option<Vec<std::path::PathBuf>> =
        (args.first().map(String::as_str) == Some("--pick")).then(|| args[1..].iter().map(std::path::PathBuf::from).collect());
    // 업데이트 뒤 새 버전은 옛 버전이 끝나길 기다립니다 (--after PID).
    if let Some(pid) = args
        .iter()
        .position(|a| a == "--after")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u32>().ok())
    {
        wait_for(pid);
    }
    // SAFETY: 프로세스가 사는 동안 쥐고 있는 이름 있는 뮤텍스.
    unsafe {
        let mutex = CreateMutexW(None, false, w!("Local\\WirDrop.Tray"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            match pick {
                Some(files) => forward(Msg::Pick { files }),
                None if !quiet => forward(Msg::OpenSettings),
                None => {}
            }
            return Ok(());
        }
        // 닫지 않습니다: 끝날 때까지 이 프로세스가 트레이 앱임을 표시합니다.
        let _ = mutex;
    }
    crate::paths::init_log("tray");
    crate::win::prefer_power_saving_gpu();
    // SAFETY: 이 프로세스의 COM: tokio 스레드의 WinRT 호출용 MTA, 여기는 STA.
    unsafe {
        CoIncrementMTAUsage()?;
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    allow_dark_menus();

    let view = Arc::new(Mutex::new(TrayView::default()));
    let hwnd = create_window()?;
    let (tx, rx) = mpsc::unbounded_channel();
    // SAFETY: 우리 리소스의 아이콘.
    let icons = unsafe {
        let module = GetModuleHandleW(None)?;
        [
            LoadIconMetric(Some(module.into()), PCWSTR(2 as _), LIM_SMALL)?,
            LoadIconMetric(Some(module.into()), PCWSTR(1 as _), LIM_SMALL)?,
        ]
    };
    // SAFETY: 상수 문자열로 RegisterWindowMessageW.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    let picks = Arc::new(Mutex::new(VecDeque::new()));
    let share_request = Arc::new(Mutex::new(None));
    let notice_request: NoticeRequest = Arc::new(Mutex::new(None));
    let menu = {
        let (picks, hwnd) = (picks.clone(), hwnd.0 as isize);
        wirdrop_menu::Menu::start(move |key| {
            picks.lock().unwrap().push_back(key);
            // SAFETY: 우리 창에 보냅니다.
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_MENU_PICK, WPARAM(0), LPARAM(0)) };
        })
    };
    let menu = match menu {
        Ok(menu) => Some(menu),
        Err(e) => {
            tracing::warn!("no Fluent menu, so the classic one: {e}");
            None
        }
    };
    CTX.with_borrow_mut(|c| {
        *c = Some(Ctx {
            events: tx.clone(),
            view: view.clone(),
            taskbar_created,
            icons,
            menu,
            shown: TrayView::default(),
            picks,
            icon_at: None,
            share_request: share_request.clone(),
            notice_request: notice_request.clone(),
            notice_tag: None,
            share: None,
            share_tag: 0,
        })
    });
    add_icon(hwnd);

    let ui = UiHandle {
        hwnd: hwnd.0 as isize,
        view,
        share: share_request,
        notice: notice_request,
    };
    if let Some(files) = pick {
        let _ = tx.send(HubEvent::Menu(MenuCmd::SendHere(files)));
    }
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio");
        rt.block_on(hub::run(tx, rx, ui, !quiet));
    });

    let mut msg = MSG::default();
    // SAFETY: 표준 메시지 반복문.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    remove_icon(hwnd);
    Ok(())
}

/// 프로세스 pid가 끝나길 최대 15초 기다립니다.
fn wait_for(pid: u32) {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: 여기서 연 핸들을 기다리고 닫습니다.
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, 15_000);
            let _ = windows::Win32::Foundation::CloseHandle(process);
        }
    }
}

/// 도는 트레이 앱에 요청을 넘깁니다.
pub fn forward(msg: Msg) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
    };
    rt.block_on(async {
        if let Ok(mut conn) = Conn::connect(Role::Cli, std::time::Duration::from_secs(3)).await {
            let _ = conn.send(&msg).await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), conn.recv()).await;
        }
    });
}

/// Windows가 어두운 모드면 어두운 상황 메뉴 (uxtheme의 SetPreferredAppMode와 FlushMenuThemes를
/// 탐색기처럼 서수로 부릅니다).
fn allow_dark_menus() {
    // SAFETY: Windows 10 1903 이후 이 서수들은 이 시그니처입니다.
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135 as _)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(1);
        }
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136 as _)) {
            let flush: extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

fn create_window() -> io::Result<HWND> {
    // SAFETY: 평범한 창 클래스와 보이지 않는 창.
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: w!("WirDropTray"),
            ..Default::default()
        };
        RegisterClassW(&class);
        Ok(CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("WirDropTray"),
            w!("WirDrop"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        )?)
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let (tip, icon) = CTX.with_borrow(|c| {
        let c = c.as_ref().expect("tray context");
        let v = c.view.lock().unwrap();
        let tip = if v.tooltip.is_empty() {
            "WirDrop".to_string()
        } else {
            v.tooltip.clone()
        };
        (tip, c.icons[usize::from(v.ready)])
    });
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    nid
}

fn add_icon(hwnd: HWND) {
    let mut nid = icon_data(hwnd);
    // SAFETY: nid를 모두 초기화했습니다.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

fn remove_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    // SAFETY: 위와 같음.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// Fluent 메뉴의 선택을 여기 창 스레드에서 처리합니다. 파일이나 폴더 대화상자가 필요한 선택은 따로
/// 스레드에서 띄웁니다 (open_dialog): 이 스레드는 트레이 아이콘을 돌리며 계속 응답해야 합니다.
fn on_pick() {
    loop {
        let Some((key, view)) = CTX.with_borrow_mut(|c| {
            let c = c.as_mut()?;
            let key = c.picks.lock().unwrap().pop_front()?;
            Some((key, c.shown.clone()))
        }) else {
            return;
        };
        if key == "rename" {
            rename(&view);
            continue;
        }
        if let Some(i) = key.strip_prefix("share:").and_then(|i| i.parse::<usize>().ok()) {
            let chosen = CTX.with_borrow_mut(|c| {
                let share = c.as_mut()?.share.take()?;
                let d = share.devices.get(i)?.clone();
                Some(MenuCmd::SendTo(TargetRef { id: d.id, name: d.name }, share.files))
            });
            if let Some(cmd) = chosen {
                send(HubEvent::Menu(cmd));
            }
            continue;
        }
        match menu::pick(&key, &view) {
            Some(menu::Picked::Now(cmd)) => send(HubEvent::Menu(cmd)),
            Some(menu::Picked::Dialog(run)) => open_dialog(run),
            None => {}
        }
    }
}

/// 파일이나 폴더 대화상자를 (한 번에 하나) 따로 스레드에서 띄우고 고른 것을 허브에 넘깁니다. 창
/// 스레드에서는 절대 안 됩니다: 트레이 창에 모달인 대화상자를 그 스레드가 기다리는 동안 띄웠더니 둘
/// 다 멈췄고 메뉴가 다시 열리지 않았습니다.
fn open_dialog(run: Box<dyn FnOnce() -> Option<MenuCmd> + Send>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static OPEN: AtomicBool = AtomicBool::new(false);
    if OPEN.swap(true, Ordering::SeqCst) {
        tracing::info!("a file dialog is open already");
        return;
    }
    let Some(events) = CTX.with_borrow(|c| c.as_ref().map(|c| c.events.clone())) else {
        OPEN.store(false, Ordering::SeqCst);
        return;
    };
    std::thread::spawn(move || {
        let cmd = run();
        OPEN.store(false, Ordering::SeqCst);
        if let Some(cmd) = cmd {
            let _ = events.send(HubEvent::Menu(cmd));
        }
    });
}

/// PC의 새 이름을 한 줄로 입력받습니다. 마우스 자리(방금 누른 메뉴 항목)를 가운데로 엽니다.
fn rename(view: &TrayView) {
    let Some((menu, events)) = CTX.with_borrow(|c| c.as_ref().and_then(|c| Some((c.menu.clone()?, c.events.clone())))) else {
        return;
    };
    let name = view.snapshot.as_ref().map(|s| s.settings.device_name.clone()).unwrap_or_default();
    let (x, y) = cursor();
    let (title, hint) = (
        t!("AirDrop 이름 설정", "AirDrop name"),
        t!("Enter: 저장 · Esc: 취소", "Enter: Save · Esc: Cancel"),
    );
    menu.ask_text(x, y, title, &name, hint, move |typed| {
        let typed = typed.trim().to_string();
        if !typed.is_empty() {
            let _ = events.send(HubEvent::Menu(MenuCmd::Change(Change::DeviceName(typed))));
        }
    });
}

/// 허브가 요청한 메뉴([UiHandle::show_entries])를 트레이 아이콘에 엽니다. 키는 연 때의 상태로 주
/// 메뉴의 것처럼 고릅니다.
fn show_notice(hwnd: HWND) {
    let Some((menu, entries, stay, tag)) = CTX.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        let (entries, stay) = c.notice_request.lock().unwrap().take()?;
        let menu = c.menu.clone()?;
        c.share = None;
        c.shown = c.view.lock().unwrap().clone();
        // 태그는 기기 메뉴의 것과 함께 세어 서로 헷갈리지 않습니다.
        c.share_tag += 1;
        c.notice_tag = Some(c.share_tag);
        Some((menu, entries, stay, c.share_tag))
    }) else {
        return;
    };
    let (x, y) = icon_point(hwnd).unwrap_or_else(cursor);
    menu.show_tagged(x, y, entries, tag);
    // SAFETY: 우리 창의 타이머.
    unsafe { SetTimer(Some(hwnd), NOTICE_TIMER, stay.as_millis().min(u128::from(u32::MAX)) as u32, None) };
}

/// [show_notice]가 연 메뉴가 아직 열린 그 메뉴면 닫습니다.
fn close_notice(hwnd: HWND) {
    // SAFETY: 우리 창의 타이머. 이미 없어도 괜찮습니다.
    let _ = unsafe { KillTimer(Some(hwnd), NOTICE_TIMER) };
    let Some((menu, tag)) = CTX.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        Some((c.menu.clone()?, c.notice_tag.take()?))
    }) else {
        return;
    };
    menu.close(tag);
}

/// 메뉴가 열리는 아이콘의 위쪽 가운데. Windows가 알려 주지 못하면 마지막 자리.
fn icon_point(hwnd: HWND) -> Option<(i32, i32)> {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    // SAFETY: 평범한 조회.
    match unsafe { Shell_NotifyIconGetRect(&id) } {
        Ok(rect) => {
            let at = ((rect.left + rect.right) / 2, rect.top);
            CTX.with_borrow_mut(|c| c.as_mut().map(|c| c.icon_at = Some(at)));
            Some(at)
        }
        Err(e) => {
            tracing::warn!("can't find the tray icon: {e}");
            CTX.with_borrow(|c| c.as_ref().and_then(|c| c.icon_at))
        }
    }
}

fn cursor() -> (i32, i32) {
    let mut point = Default::default();
    // SAFETY: 평범한 조회.
    let _ = unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

fn send(event: HubEvent) {
    CTX.with_borrow(|c| {
        if let Some(c) = c {
            let _ = c.events.send(event);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lparam.0 & 0xffff) as u32;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_KEYSELECT {
                // TranslucentTB처럼 어느 버튼이든 메뉴를 열고, 아이콘 어디를 눌렀든 늘 아이콘에서
                // 열어 한자리에 뜹니다.
                let click = ((wparam.0 & 0xffff) as i16 as i32, ((wparam.0 >> 16) & 0xffff) as i16 as i32);
                let (x, y) = icon_point(hwnd).unwrap_or(click);
                show_menu(hwnd, x, y);
            }
            LRESULT(0)
        }
        WM_MENU_PICK => {
            on_pick();
            LRESULT(0)
        }
        WM_SHOW_MENU => {
            let (x, y) = icon_point(hwnd).unwrap_or_else(cursor);
            show_menu(hwnd, x, y);
            LRESULT(0)
        }
        WM_SHARE => {
            open_share();
            LRESULT(0)
        }
        WM_NOTICE => {
            show_notice(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == NOTICE_TIMER => {
            close_notice(hwnd);
            LRESULT(0)
        }
        WM_REFRESH => {
            let nid = icon_data(hwnd);
            // SAFETY: nid를 모두 초기화했습니다.
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
            refresh_share();
            LRESULT(0)
        }
        WM_QUIT_APP => {
            remove_icon(hwnd);
            // SAFETY: 이 스레드의 메시지 반복문을 끝냅니다.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            if wparam.0 == DBT_DEVNODES_CHANGED {
                send(HubEvent::DevicesChanged);
            }
            LRESULT(1)
        }
        _ if CTX.with_borrow(|c| c.as_ref().is_some_and(|c| c.taskbar_created == msg)) => {
            // 탐색기가 다시 시작했습니다: 아이콘이 사라졌습니다.
            add_icon(hwnd);
            LRESULT(0)
        }
        // SAFETY: 기본 처리.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let fluent = CTX.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        // 트레이 메뉴가 기기 메뉴 자리를 차지합니다.
        c.share = None;
        let menu = c.menu.clone()?;
        c.shown = c.view.lock().unwrap().clone();
        Some(menu.show(x, y, menu::entries(&c.shown)))
    });
    if fluent == Some(true) {
        return;
    }
    let Some(view) = CTX.with_borrow(|c| Some(c.as_ref()?.view.lock().unwrap().clone())) else {
        return;
    };
    // SAFETY: 여기서 만들고 띄우고 없애는 메뉴. 문자열은 호출보다 오래 삽니다.
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let item = |flags, id: usize, text: &str| {
            let text = HSTRING::from(text);
            let _ = AppendMenuW(menu, flags, id, &text);
        };
        // Fluent 메뉴처럼: 사용자가 필요할 때 엔진을 시작하는 상태 줄.
        item(if view.startable { MF_STRING } else { MF_STRING | MF_GRAYED }, ID_RECONNECT, &view.status);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, ID_OPEN_DIR, t!("폴더", "Folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(
            MF_STRING | if view.receive { MF_CHECKED } else { MF_STRING },
            ID_RECEIVE,
            t!("받기", "Receive"),
        );
        item(
            MF_STRING | if view.autostart { MF_CHECKED } else { MF_STRING },
            ID_AUTOSTART,
            t!("부팅 시 켜기", "Open at boot"),
        );
        let _ = SetMenuDefaultItem(menu, ID_OPEN_DIR as u32, 0);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, ID_EXIT, t!("종료", "Quit"));
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN).0, x, y, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    let event = match cmd {
        ID_OPEN_DIR => MenuCmd::OpenSaveDir,
        ID_RECEIVE => MenuCmd::ToggleReceive,
        ID_AUTOSTART => MenuCmd::ToggleAutostart,
        ID_RECONNECT => MenuCmd::Reconnect,
        ID_EXIT => MenuCmd::Exit,
        _ => return,
    };
    send(HubEvent::Menu(event));
}

/// 허브가 요청한 기기 메뉴를 엽니다 (UiHandle::share).
fn open_share() {
    let Some((menu, files, view)) = CTX.with_borrow(|c| {
        let c = c.as_ref()?;
        let files = c.share_request.lock().unwrap().take()?;
        Some((c.menu.clone(), files, c.view.lock().unwrap().clone()))
    }) else {
        return;
    };
    let Some(menu) = menu else {
        // 기기를 띄울 Fluent 메뉴가 없습니다: 고전 메뉴는 열린 동안 바뀔 수 없습니다.
        tracing::warn!("no Fluent menu for the device list");
        return;
    };
    let (entries, devices, note) = menu::share_entries(&view, &files);
    let tag = CTX.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        c.share_tag += 1;
        c.share = Some(Share {
            files,
            devices,
            note,
            tag: c.share_tag,
        });
        Some(c.share_tag)
    });
    let Some(tag) = tag else { return };
    let (x, y) = cursor();
    menu.show_centred(x, y, entries, tag);
}

/// 열린 기기 메뉴에 지금 있는 기기를, 바뀌었으면 넣습니다.
fn refresh_share() {
    let update = CTX.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        let view = c.view.lock().unwrap().clone();
        let share = c.share.as_mut()?;
        let (entries, devices, note) = menu::share_entries(&view, &share.files);
        if devices == share.devices && note == share.note {
            return None;
        }
        share.devices = devices;
        share.note = note;
        Some((c.menu.clone()?, share.tag, entries))
    });
    if let Some((menu, tag, entries)) = update {
        menu.update(tag, entries);
    }
}
