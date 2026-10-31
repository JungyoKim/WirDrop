//! 알림 영역 아이콘을 위한 WinUI 3의 Fluent 메뉴: Windows 11 앱들이 띄우는 MenuFlyout를 아이콘,
//! 켜고 끄기, 라디오 묶음, 하위 메뉴와 함께 화면의 한 점에 엽니다. 메뉴로 물을 수 없는 것은 한 줄
//! 입력([Menu::ask_text])을 한 점을 가운데로 엽니다. 태그로 연 메뉴([Menu::show_tagged])는 열린
//! 채로 바꿀 수 있어([Menu::update]) 보는 동안 바뀌는 목록에 씁니다. 첫 줄이
//! 손잡이([Entry::handle])인 메뉴와 한 줄 입력은 제목을 잡고 창처럼 옮길 수 있습니다.
//!
//! 트레이 앱에는 플라이아웃을 열 창이 없어서, 그 점으로 옮긴 숨은 2x2 창 위의 XAML 아일랜드에서
//! 메뉴를 엽니다. TranslucentTB가 하는 방식이고, windows-reactor가 업스트림에 들이는 알림 아이콘
//! 메뉴(MIT OR Apache-2.0)도 그렇게 하며 이것을 따랐습니다. WinUI는 자기 스레드에서 돌고, [Menu]는
//! 어느 스레드에서든 메뉴를 여는 손잡이입니다.

#[allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code, unused_imports, clippy::all)]
#[rustfmt::skip]
// windows-bindgen이 생성한 바인딩: 손으로 고치거나 rustfmt로 정렬하지 않습니다 (tools/bindgen이 다시 만듭니다).
#[rustfmt::skip]
mod bindings;

mod grip;
mod placement;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use bindings::*;
use windows_core::{Error, EventRevoker, HRESULT, HSTRING, Interface, Ref, Result, implement};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows_sys::Win32::Storage::Packaging::Appx::{
    AddPackageDependency, AddPackageDependencyOptions_None, CreatePackageDependencyOptions_None, PACKAGE_VERSION, PACKAGE_VERSION_0,
    PackageDependencyLifetimeKind_Process, PackageDependencyProcessorArchitectures, PackageDependencyProcessorArchitectures_Neutral,
    TryCreatePackageDependency,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapFree};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, EnumThreadWindows, GA_ROOT, GetAncestor, GetClassNameW, GetWindowRect, GetWindowThreadProcessId,
    HC_ACTION, HHOOK, HWND_TOPMOST, IsWindowVisible, LWA_ALPHA, MSLLHOOKSTRUCT, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos, SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WindowFromPoint,
};
use windows_sys::core::BOOL;
use windows_sys::w;

use grip::{Grip, grip_off, grip_on};
use placement::{centre, centred, place, screen_at};

/// 메뉴 한 줄.
#[derive(Clone, Debug)]
pub struct Entry {
    kind: Kind,
    text: String,
    glyph: Option<char>,
    enabled: bool,
    handle: bool,
}

#[derive(Clone, Debug)]
enum Kind {
    Item(String),
    Toggle(String, bool),
    Radio { key: String, checked: bool },
    Sub(Vec<Entry>),
    Separator,
}

impl Entry {
    /// 할 일. 고르면 key를 클릭 처리기에 넘깁니다.
    pub fn item(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(Kind::Item(key.into()), text)
    }

    /// 고르기보다 읽는 줄: 상태, 값.
    pub fn label(text: impl Into<String>) -> Self {
        Self::new(Kind::Item(String::new()), text).enabled(false)
    }

    /// 켜고 끄는 설정: 켜져 있으면 아이콘 열에 체크가 있어 글이 아이콘 줄의 글과 맞습니다
    /// (TranslucentTB의 메뉴도 그렇습니다).
    pub fn toggle(key: impl Into<String>, text: impl Into<String>, checked: bool) -> Self {
        Self::new(Kind::Toggle(key.into(), checked), text)
    }

    /// 여럿 중 하나: 고른 것은 아이콘 열에 점이 있습니다.
    pub fn radio(key: impl Into<String>, text: impl Into<String>, checked: bool) -> Self {
        Self::new(Kind::Radio { key: key.into(), checked }, text)
    }

    pub fn sub(text: impl Into<String>, items: Vec<Entry>) -> Self {
        Self::new(Kind::Sub(items), text)
    }

    pub fn separator() -> Self {
        Self::new(Kind::Separator, "")
    }

    /// 글 옆의 Segoe Fluent Icons 아이콘.
    pub fn glyph(mut self, glyph: char) -> Self {
        self.glyph = Some(glyph);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 이 줄을 잡으면 제목 표시줄이 창을 옮기듯 메뉴가 움직입니다: 메뉴의 첫 줄, 읽기만 하는 줄에
    /// 씁니다 (눌러도 아무 일 없음).
    pub fn handle(mut self) -> Self {
        self.handle = true;
        self
    }

    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Entry {
            kind,
            text: text.into(),
            glyph: None,
            enabled: true,
            handle: false,
        }
    }
}

type OnClick = Box<dyn Fn(String) + Send>;
type OnText = Box<dyn FnOnce(String) + Send>;

/// 메뉴 스레드로 가는 길.
#[derive(Clone)]
pub struct Menu {
    queue: DispatcherQueue,
}

impl Menu {
    /// WinUI를 자기 스레드에서 시작합니다. on_click은 그 스레드에서 고른 항목의 key를 받습니다:
    /// 거기서 막지 말고 넘기세요.
    pub fn start(on_click: impl Fn(String) + Send + 'static) -> Result<Menu> {
        let (ready, started) = mpsc::channel();
        let failed = ready.clone();
        std::thread::Builder::new()
            .name("menu".into())
            .spawn(move || {
                if let Err(e) = run(Box::new(on_click), ready) {
                    let _ = failed.send(Err(e));
                }
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        started
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| Error::new(E_FAIL, "WinUI didn't start"))?
    }

    /// entries를 물리 화면 픽셀 x, y에 메뉴로 엽니다. 열린 메뉴가 있으면 그것을 대신합니다. 자리가
    /// 있는 쪽으로 점의 위나 아래, 왼쪽이나 오른쪽에 열리고, 작업 표시줄 위의 점은 Windows 자체
    /// 플라이아웃처럼 [GAP]만큼 떼어 둡니다. 메뉴 스레드가 사라졌으면 false.
    pub fn show(&self, x: i32, y: i32, entries: Vec<Entry>) -> bool {
        self.show_tagged(x, y, entries, 0)
    }

    /// [Menu::show]와 같되 열린 채로 [Menu::update]가 바꿀 수 있는 메뉴: tag가 그 이름입니다 (0이
    /// 아님).
    pub fn show_tagged(&self, x: i32, y: i32, entries: Vec<Entry>, tag: u64) -> bool {
        self.open_at(At::Point(x, y), entries, tag)
    }

    /// [Menu::show_tagged]와 같되 x, y를 가운데로 엽니다 (안 맞으면 화면 가장자리 안으로 들임):
    /// 닫힌 다른 앱의 메뉴를 이어받는 메뉴라 모든 항목이 마우스에서 가깝게.
    pub fn show_centred(&self, x: i32, y: i32, entries: Vec<Entry>, tag: u64) -> bool {
        self.open_at(At::Centre(x, y), entries, tag)
    }

    fn open_at(&self, at: At, entries: Vec<Entry>, tag: u64) -> bool {
        let job = Mutex::new(Some(entries));
        let handler = DispatcherQueueHandler::new(move || {
            if let Some(entries) = job.lock().unwrap().take()
                && let Err(e) = open(at, entries, tag)
            {
                eprintln!("menu: {e}");
            }
        });
        self.queue
            .TryEnqueueWithPriority(DispatcherQueuePriority::Normal, &handler)
            .unwrap_or(false)
    }

    /// 태그가 tag인 메뉴가 아직 열려 있으면 항목을 바꾸고, 아니면 아무것도 하지 않습니다. 메뉴
    /// 스레드가 사라졌으면 false.
    pub fn update(&self, tag: u64, entries: Vec<Entry>) -> bool {
        let job = Mutex::new(Some(entries));
        let handler = DispatcherQueueHandler::new(move || {
            if let Some(entries) = job.lock().unwrap().take()
                && let Err(e) = refill(tag, entries)
            {
                eprintln!("menu: {e}");
            }
        });
        self.queue
            .TryEnqueueWithPriority(DispatcherQueuePriority::Normal, &handler)
            .unwrap_or(false)
    }

    /// 태그가 tag인 메뉴가 아직 열린 그 메뉴면 닫고, 아니면 아무것도 하지 않습니다. 메뉴 스레드가
    /// 사라졌으면 false.
    pub fn close(&self, tag: u64) -> bool {
        let handler = DispatcherQueueHandler::new(move || {
            let open = HOST.with_borrow(|h| {
                h.as_ref()
                    .and_then(|h| h.open.as_ref())
                    .filter(|o| o.tag == tag && tag != 0)
                    .map(|o| o.flyout.clone())
            });
            if let Some(flyout) = open {
                let _ = flyout.Hide();
            }
        });
        self.queue
            .TryEnqueueWithPriority(DispatcherQueuePriority::Normal, &handler)
            .unwrap_or(false)
    }

    /// x, y를 가운데로 (안 맞으면 화면 가장자리 안으로) 한 줄 입력을 엽니다. 열린 메뉴가 있으면
    /// 대신합니다: 위에 title, initial을 모두 선택해 담은 입력 상자, 아래에 hint. Enter는 입력한
    /// 것을 메뉴 스레드에서 on_done에 넘기고, Esc나 다른 곳을 누르면 넘기지 않고 닫으며 on_done은
    /// 불리지 않고 버려집니다. 메뉴 스레드가 사라졌으면 false.
    pub fn ask_text(&self, x: i32, y: i32, title: &str, initial: &str, hint: &str, on_done: impl FnOnce(String) + Send + 'static) -> bool {
        let job = Mutex::new(Some((
            title.to_string(),
            initial.to_string(),
            hint.to_string(),
            Box::new(on_done) as OnText,
        )));
        let handler = DispatcherQueueHandler::new(move || {
            if let Some((title, initial, hint, on_done)) = job.lock().unwrap().take()
                && let Err(e) = open_text(x, y, &title, &initial, &hint, on_done)
            {
                eprintln!("menu: {e}");
            }
        });
        self.queue
            .TryEnqueueWithPriority(DispatcherQueuePriority::Normal, &handler)
            .unwrap_or(false)
    }
}

const E_FAIL: HRESULT = HRESULT(0x8000_4005_u32 as i32);
/// 숨은 창의 크기: 메뉴의 기준점은 그 왼쪽 위 픽셀입니다.
const HOST_SIZE: i32 = 2;
/// 메뉴와 작업 표시줄 사이 (유효 픽셀): Windows 11의 플라이아웃이 두는 거리.
const GAP: i32 = 12;
/// WinUI가 배치하는 메뉴의 치수(유효 픽셀): 가운데로 여는 메뉴의 크기를 여기서 셈합니다. 기준점에서
/// 열리는 거리, 항목과 구분선의 높이와 그 위아래 여백, 항목 글이 (아이콘을 지나) 시작하는 곳과 그
/// 뒤 여백, 하위 메뉴 꺾쇠나 체크가 차지하는 자리, 메뉴의 최대 폭.
const FLYOUT_OFFSET: i32 = 4;
const ROW: i32 = 32;
const SEPARATOR: i32 = 9;
const MENU_PADDING: i32 = 7;
/// 마우스로 연 메뉴에서 WinUI가 주는 메뉴 줄 안쪽 여백 (MenuFlyoutItemThemePaddingNarrow). 마우스,
/// 펜, 키보드를 본 적이 없으면 WinUI는 줄을 터치용(11,8,11,9)으로 띄웁니다: 시작 직후에는 본 것이
/// 없어 첫 메뉴가 길쭉하게 떴습니다. 줄마다 정해 두면 늘 이 값입니다.
const MOUSE_PADDING: Thickness = Thickness {
    left: 11.0,
    top: 4.0,
    right: 11.0,
    bottom: 5.0,
};
const TEXT_START: i32 = 43;
const TEXT_END: i32 = 16;
const MARK: i32 = 28;
const MENU_MAX_WIDTH: i32 = 456;
/// 한 줄 입력(유효 픽셀): 입력 상자의 폭과 플라이아웃 전체 크기 (상자, 그 둘레의 제목과 안내,
/// 플라이아웃의 여백과 테두리).
const TEXT_BOX_WIDTH: i32 = 280;
const TEXT_FLYOUT: (i32, i32) = (TEXT_BOX_WIDTH + 34, 118);
/// 플라이아웃을 잡을 수 있는, 창 위에서부터의 거리(유효 픽셀): 메뉴의 첫 줄과 그 아래 구분선
/// (터치처럼 높은 줄이면 그만큼 채움), 한 줄 입력의 제목.
const GRAB_MENU: i32 = MENU_PADDING + ROW + SEPARATOR;
const GRAB_TEXT: i32 = 42;
/// WinUI가 플라이아웃을 띄우는 창의 클래스.
const POPUP_CLASS: &str = "Microsoft.UI.Content.PopupWindowSiteBridge";
/// 메뉴의 글꼴과 크기(유효 픽셀).
const MENU_FONT: windows_sys::core::PCWSTR = w!("Segoe UI");
const MENU_FONT_SIZE: i32 = 14;
/// Segoe Fluent Icons의 CheckMark와 RadioBullet. 켜고 끄기와 라디오 선택에 씁니다.
const CHECK: char = '\u{E73E}';
const DOT: char = '\u{E915}';

// ---- WinUI 애플리케이션

fn run(on_click: OnClick, ready: mpsc::Sender<Result<Menu>>) -> Result<()> {
    bootstrap()?;
    // SAFETY: 이 스레드에 대한 평범한 호출.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hr = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        if hr < 0 {
            return Err(Error::from_hresult(HRESULT(hr)));
        }
    }
    let launch = RefCell::new(Some((on_click, ready)));
    // WinUI가 이 스레드의 메시지 반복문을 도는 동안 막힙니다.
    Application::Start(&ApplicationInitializationCallback::new(move |_| {
        let Some((on_click, ready)) = launch.borrow_mut().take() else { return };
        let failed = ready.clone();
        let app = App {
            provider: RefCell::new(None),
            launch: RefCell::new(Some((on_click, ready))),
        };
        match Application::compose(app) {
            Ok(app) => APP.with(|a| *a.borrow_mut() = Some(app)),
            Err(e) => {
                let _ = failed.send(Err(e));
            }
        }
    }))
}

thread_local! {
    static APP: RefCell<Option<Application>> = const { RefCell::new(None) };
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static GRIP: RefCell<Option<Grip>> = const { RefCell::new(None) };
}

/// WinUI가 필요로 하는 애플리케이션 개체: Fluent 컨트롤 스타일을 불러오고 그 형식을 찾습니다.
#[implement(IApplicationOverrides, IXamlMetadataProvider)]
struct App {
    provider: RefCell<Option<XamlControlsXamlMetaDataProvider>>,
    launch: RefCell<Option<(OnClick, mpsc::Sender<Result<Menu>>)>>,
}

impl App {
    fn provider(&self) -> Result<XamlControlsXamlMetaDataProvider> {
        if let Some(provider) = self.provider.borrow().as_ref() {
            return Ok(provider.clone());
        }
        let provider = XamlControlsXamlMetaDataProvider::new()?;
        *self.provider.borrow_mut() = Some(provider.clone());
        Ok(provider)
    }
}

impl IApplicationOverrides_Impl for App_Impl {
    fn OnLaunched(&self, _: Ref<LaunchActivatedEventArgs>) -> Result<()> {
        let Some((on_click, ready)) = self.launch.borrow_mut().take() else {
            return Ok(());
        };
        let started = (|| {
            let styles: ResourceDictionary = XamlControlsResources::new()?.cast()?;
            Application::Current()?.Resources()?.MergedDictionaries()?.Append(&styles)?;
            let host = Host::new(on_click)?;
            HOST.with(|h| *h.borrow_mut() = Some(host));
            Ok(Menu {
                queue: DispatcherQueue::GetForCurrentThread()?,
            })
        })();
        let _ = ready.send(started);
        Ok(())
    }
}

impl IXamlMetadataProvider_Impl for App_Impl {
    fn GetXamlType(&self, r#type: &TypeName) -> Result<IXamlType> {
        self.provider()?.GetXamlType(r#type)
    }

    fn GetXamlTypeByFullName(&self, full_name: &HSTRING) -> Result<IXamlType> {
        self.provider()?.GetXamlTypeByFullName(&full_name.to_string_lossy())
    }

    fn GetXmlnsDefinitions(&self) -> Result<windows_core::Array<XmlnsDefinition>> {
        self.provider()?.GetXmlnsDefinitions()
    }
}

/// 패키지되지 않은 이 프로세스에 Windows App Runtime 2 프레임워크 패키지를 더합니다.
fn bootstrap() -> Result<()> {
    #[cfg(target_arch = "x86_64")]
    const ARCH: PackageDependencyProcessorArchitectures = windows_sys::Win32::Storage::Packaging::Appx::PackageDependencyProcessorArchitectures_X64;
    #[cfg(target_arch = "aarch64")]
    const ARCH: PackageDependencyProcessorArchitectures = windows_sys::Win32::Storage::Packaging::Appx::PackageDependencyProcessorArchitectures_Arm64;
    #[cfg(target_arch = "x86")]
    const ARCH: PackageDependencyProcessorArchitectures = windows_sys::Win32::Storage::Packaging::Appx::PackageDependencyProcessorArchitectures_X86;
    let mut id = std::ptr::null_mut();
    let mut full_name = std::ptr::null_mut();
    let mut context = std::ptr::null_mut();
    // SAFETY: 지역 변수에 대한 출력 포인터. 문자열은 문서대로 해제합니다.
    unsafe {
        let hr = TryCreatePackageDependency(
            std::ptr::null_mut(),
            w!("Microsoft.WindowsAppRuntime.2_8wekyb3d8bbwe"),
            PACKAGE_VERSION {
                Anonymous: PACKAGE_VERSION_0 { Version: 2 << 48 },
            },
            ARCH | PackageDependencyProcessorArchitectures_Neutral,
            PackageDependencyLifetimeKind_Process,
            std::ptr::null(),
            CreatePackageDependencyOptions_None,
            &mut id,
        );
        if hr < 0 {
            return Err(Error::new(HRESULT(hr), "the Windows App Runtime 2 isn't installed"));
        }
        let hr = AddPackageDependency(id, 0, AddPackageDependencyOptions_None, &mut context, &mut full_name);
        HeapFree(GetProcessHeap(), 0, id.cast());
        if !full_name.is_null() {
            HeapFree(GetProcessHeap(), 0, full_name.cast());
        }
        if hr < 0 {
            return Err(Error::from_hresult(HRESULT(hr)));
        }
    }
    Ok(())
}

// ---- 메뉴 호스트

struct Host {
    hwnd: HWND,
    _source: DesktopWindowXamlSource,
    anchor: FrameworkElement,
    loaded: Rc<Cell<bool>>,
    /// 아일랜드가 준비되기 전에 요청한 메뉴.
    pending: Option<(At, Vec<Entry>, u64)>,
    open: Option<Open>,
    /// 열린 메뉴: 메뉴의 Closed는 자기 뒷정리만 합니다.
    generation: u64,
    on_click: Rc<OnClick>,
    _loaded: EventRevoker,
}

struct Open {
    flyout: IFlyoutBase,
    /// 메뉴일 때 그 메뉴 자체 (한 줄 입력은 아님).
    menu: Option<MenuFlyout>,
    tag: u64,
    _revokers: Vec<EventRevoker>,
    /// 항목들의 클릭 처리기. [Menu::update]가 다시 채우면 바뀝니다.
    entries: Vec<EventRevoker>,
}

impl Host {
    fn new(on_click: OnClick) -> Result<Host> {
        let hwnd = host_window()?;
        let root = Grid::new()?;
        let anchor = Grid::new()?;
        let anchor_element = anchor.cast::<IFrameworkElement>()?;
        anchor_element.SetWidth(1.0)?;
        anchor_element.SetHeight(1.0)?;
        anchor_element.SetHorizontalAlignment(HorizontalAlignment::Left)?;
        anchor_element.SetVerticalAlignment(VerticalAlignment::Top)?;
        root.cast::<IPanel>()?.Children()?.Append(&anchor.cast::<UIElement>()?)?;

        let source = DesktopWindowXamlSource::new()?;
        source.Initialize(WindowId { value: hwnd as u64 })?;
        // 트레이 메뉴는 작업 표시줄을 덮어도 됩니다.
        source.cast::<IDesktopWindowXamlSource2>()?.SetShouldConstrainPopupsToWorkArea(false)?;
        source.SetContent(&root.cast::<UIElement>()?)?;
        source.SiteBridge()?.SetResizePolicy(ContentSizePolicy::ResizeContentToParentWindow)?;

        let loaded = Rc::new(Cell::new(false));
        let flag = loaded.clone();
        let loaded_revoker = anchor_element.Loaded(move |_, _| {
            flag.set(true);
            let pending = HOST.with_borrow_mut(|h| h.as_mut().and_then(|h| h.pending.take()));
            if let Some((at, entries, tag)) = pending
                && let Err(e) = open(at, entries, tag)
            {
                eprintln!("menu: {e}");
            }
        })?;
        Ok(Host {
            hwnd,
            _source: source,
            anchor: anchor.cast()?,
            loaded,
            pending: None,
            open: None,
            generation: 0,
            on_click: Rc::new(on_click),
            _loaded: loaded_revoker,
        })
    }
}

/// 아무도 못 보는 2x2 팝업: 레이어드에 완전히 투명하고 작업 표시줄과 Alt+Tab에 나오지 않습니다.
fn host_window() -> Result<HWND> {
    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
        // SAFETY: 기본 처리.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
    // SAFETY: 우리 창 클래스와 창.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = w!("WirDropMenuHost");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            HOST_SIZE,
            HOST_SIZE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return Err(Error::from_thread());
        }
        SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

/// 플라이아웃이 열리는 곳: 그 점으로 옮긴 기준점과 여는 방향.
struct Spot {
    anchor: FrameworkElement,
    placement: FlyoutPlacementMode,
    on_click: Rc<OnClick>,
}

/// 메뉴가 열리는 곳.
#[derive(Clone, Copy, Debug)]
enum At {
    /// 한 점에 ([place] 참고).
    Point(i32, i32),
    /// 한 점을 가운데로 ([centre] 참고).
    Centre(i32, i32),
    /// 한 줄 입력을 한 점을 가운데로 ([open_text] 참고).
    Text(i32, i32),
}

/// at에 열 플라이아웃을 위해 호스트를 준비합니다: 열린 것을 숨기고, 숨은 창을 거기로 옮겨 앞으로
/// 가져옵니다. 아일랜드를 불러오기 전이면 None이고 메뉴는 그때를 위해 둡니다.
fn prepare(at: At, menu: Option<(Vec<Entry>, u64)>) -> Result<Option<Spot>> {
    let Some((hwnd, anchor, previous, on_click, loaded)) = HOST.with_borrow_mut(|h| {
        let h = h.as_mut()?;
        Some((h.hwnd, h.anchor.clone(), h.open.take(), h.on_click.clone(), h.loaded.get()))
    }) else {
        return Ok(None);
    };
    if let Some(previous) = previous {
        let _ = previous.flyout.Hide();
    }
    let (x, y, placement) = match at {
        At::Point(x, y) => place(x, y),
        At::Centre(x, y) => centre(x, y, menu.as_ref().map_or(&[][..], |(entries, _)| entries.as_slice())),
        At::Text(x, y) => {
            let (work, dpi) = screen_at(x, y);
            let scale = |px: i32| px * dpi as i32 / 96;
            centred(x, y, scale(TEXT_FLYOUT.0), scale(TEXT_FLYOUT.1), 0, work, dpi)
        }
    };
    // SAFETY: 우리 창.
    unsafe { SetWindowPos(hwnd, HWND_TOPMOST, x, y, HOST_SIZE, HOST_SIZE, SWP_NOACTIVATE) };
    if !loaded {
        if let Some((entries, tag)) = menu {
            HOST.with_borrow_mut(|h| h.as_mut().map(|h| h.pending = Some((at, entries, tag))));
        }
        return Ok(None);
    }
    // 메뉴는 앱과 다를 수 있는 작업 표시줄의 테마를 따릅니다.
    anchor.SetRequestedTheme(system_theme())?;
    // 가볍게 닫기와 입력에는 호스트가 앞에 있어야 합니다. 트레이 아이콘을 누르면 이 프로세스가
    // 전경을 가져갈 수 있고, Windows가 거절해도 플라이아웃은 열립니다.
    // SAFETY: 우리 창.
    unsafe { SetForegroundWindow(hwnd) };
    Ok(Some(Spot { anchor, placement, on_click }))
}

/// spot에서 플라이아웃을 띄우고, 닫히거나 다른 것이 열릴 때까지 이벤트 처리기와 함께 열린 것으로
/// 둡니다. menu와 entries는 메뉴의 것, tag는 [Menu::update]가 알아보는 이름. grab이면 위에서 그만큼
/// 아래까지 잡고 옮길 수 있습니다 ([grip_on] 참고).
fn show(
    spot: &Spot,
    base: IFlyoutBase,
    mut revokers: Vec<EventRevoker>,
    menu: Option<MenuFlyout>,
    entries: Vec<EventRevoker>,
    tag: u64,
    grab: Option<i32>,
) -> Result<()> {
    // 열려 있던 것은 사라졌고, 그 손잡이도 함께.
    grip_off();
    let generation = HOST.with_borrow_mut(|h| {
        h.as_mut().map(|h| {
            h.generation += 1;
            h.generation
        })
    });
    revokers.push(base.Closed(move |_, _| {
        // 나중에: Closed는 Hide나 ShowAt 안에서 올 수 있습니다.
        let tidy = DispatcherQueueHandler::new(move || {
            let current = HOST.with_borrow_mut(|h| match h.as_mut() {
                Some(h) if Some(h.generation) == generation => {
                    h.open = None;
                    true
                }
                _ => false,
            });
            if current {
                grip_off();
            }
        });
        if let Ok(queue) = DispatcherQueue::GetForCurrentThread() {
            let _ = queue.TryEnqueueWithPriority(DispatcherQueuePriority::Normal, &tidy);
        }
    })?);
    if let Some(reach) = grab {
        revokers.push(base.Opened(move |_, _| grip_on(reach))?);
    }
    let open = Open {
        flyout: base.clone(),
        menu,
        tag,
        _revokers: revokers,
        entries,
    };
    HOST.with_borrow_mut(|h| h.as_mut().map(|h| h.open = Some(open)));
    base.ShowAt(&spot.anchor)
}

/// 채우기 전의 플라이아웃 자리와 그 주인.
fn flyout_base(spot: &Spot, base: &IFlyoutBase) -> Result<()> {
    base.SetPlacement(spot.placement)?;
    base.SetShouldConstrainToRootBounds(false)?;
    base.SetXamlRoot(&spot.anchor.cast::<IUIElement>()?.XamlRoot()?)
}

/// 메뉴 스레드에서 메뉴를 엽니다. WinUI가 도는 동안 아무것도 빌려 두지 않습니다: 메뉴를 닫으면
/// 여기로 다시 들어옵니다.
fn open(at: At, entries: Vec<Entry>, tag: u64) -> Result<()> {
    let Some(spot) = prepare(at, Some((entries.clone(), tag)))? else {
        return Ok(());
    };
    let flyout = MenuFlyout::new()?;
    let base = flyout.cast::<IFlyoutBase>()?;
    flyout_base(&spot, &base)?;
    let mut clicks = Vec::new();
    build(&entries, &flyout.Items()?, &mut clicks, &spot.on_click)?;
    let grab = entries.first().is_some_and(|e| e.handle).then_some(GRAB_MENU);
    show(&spot, base, Vec::new(), Some(flyout), clicks, tag, grab)
}

/// 열린 메뉴(태그 tag)가 보여 주는 것을 entries로 바꿉니다 ([Menu::update] 참고), 메뉴 스레드에서.
/// 그 메뉴가 닫혔거나 다른 것이 열렸으면 아무것도 하지 않습니다.
fn refill(tag: u64, entries: Vec<Entry>) -> Result<()> {
    let Some((menu, on_click)) = HOST.with_borrow(|h| {
        let h = h.as_ref()?;
        let open = h.open.as_ref().filter(|o| o.tag == tag && tag != 0)?;
        Some((open.menu.clone()?, h.on_click.clone()))
    }) else {
        return Ok(());
    };
    let items = menu.Items()?;
    items.Clear()?;
    let mut clicks = Vec::new();
    build(&entries, &items, &mut clicks, &on_click)?;
    HOST.with_borrow_mut(|h| {
        if let Some(open) = h.as_mut().and_then(|h| h.open.as_mut()).filter(|o| o.tag == tag) {
            open.entries = clicks;
        }
    });
    Ok(())
}

/// 메뉴 스레드에서 한 줄 입력을 엽니다 ([Menu::ask_text] 참고).
fn open_text(x: i32, y: i32, title: &str, initial: &str, hint: &str, on_done: OnText) -> Result<()> {
    let Some(spot) = prepare(At::Text(x, y), None)? else { return Ok(()) };
    let label = TextBlock::new()?;
    let label_text = label.cast::<ITextBlock>()?;
    label_text.SetText(title)?;
    label_text.SetTextWrapping(TextWrapping::Wrap)?;
    label.cast::<IFrameworkElement>()?.SetMaxWidth(f64::from(TEXT_BOX_WIDTH))?;
    let input = TextBox::new()?;
    let input_box = input.cast::<ITextBox>()?;
    input_box.SetText(initial)?;
    input.cast::<IFrameworkElement>()?.SetMinWidth(f64::from(TEXT_BOX_WIDTH))?;
    let hint_line = TextBlock::new()?;
    let hint_text = hint_line.cast::<ITextBlock>()?;
    hint_text.SetText(hint)?;
    hint_text.SetFontSize(12.0)?;
    hint_line.cast::<IUIElement>()?.SetOpacity(0.7)?;
    let panel = StackPanel::new()?;
    panel.cast::<IStackPanel>()?.SetSpacing(8.0)?;
    let children = panel.cast::<IPanel>()?.Children()?;
    for element in [label.cast::<UIElement>()?, input.cast::<UIElement>()?, hint_line.cast::<UIElement>()?] {
        children.Append(&element)?;
    }

    let flyout = Flyout::new()?;
    flyout.cast::<IFlyout>()?.SetContent(&panel.cast::<UIElement>()?)?;
    let base = flyout.cast::<IFlyoutBase>()?;
    flyout_base(&spot, &base)?;
    let mut revokers = Vec::new();
    let field = input.cast::<IUIElement>()?;
    {
        let (input_box, field) = (input_box.clone(), field.clone());
        revokers.push(base.Opened(move |_, _| {
            let _ = field.Focus(FocusState::Programmatic);
            let _ = input_box.SelectAll();
        })?);
    }
    let on_done = RefCell::new(Some(on_done));
    let closer = base.clone();
    revokers.push(field.KeyDown(move |_, args| {
        let Some(args) = args.as_ref() else { return };
        match args.Key() {
            Ok(VirtualKey::Enter) => {
                let _ = args.SetHandled(true);
                if let (Some(done), Ok(text)) = (on_done.borrow_mut().take(), input_box.Text()) {
                    done(text);
                }
                let _ = closer.Hide();
            }
            Ok(VirtualKey::Escape) => {
                let _ = args.SetHandled(true);
                let _ = closer.Hide();
            }
            _ => {}
        }
    })?);
    show(&spot, base, revokers, None, Vec::new(), 0, Some(GRAB_TEXT))
}

fn build(
    entries: &[Entry],
    into: &windows_collections::IVector<MenuFlyoutItemBase>,
    revokers: &mut Vec<EventRevoker>,
    on_click: &Rc<OnClick>,
) -> Result<()> {
    for entry in entries {
        let text = entry.text.as_str();
        let item: MenuFlyoutItemBase = match &entry.kind {
            Kind::Separator => MenuFlyoutSeparator::new()?.cast()?,
            Kind::Sub(items) => {
                let sub = MenuFlyoutSubItem::new()?;
                sub.SetText(text)?;
                if let Some(glyph) = entry.glyph {
                    sub.SetIcon(&icon(glyph)?)?;
                }
                build(items, &sub.Items()?, revokers, on_click)?;
                sub.cast()?
            }
            Kind::Item(key) => {
                let item = MenuFlyoutItem::new()?;
                choice(&item.cast()?, text, entry.glyph, key, revokers, on_click)?;
                // 자기 아이콘이 없는 읽기 줄은 제목입니다: 아이콘이 다른 줄을 밀어내는 글 열이
                // 아니라 아이콘이 시작하는 곳에서 시작합니다.
                if key.is_empty()
                    && !entry.enabled
                    && entry.glyph.is_none()
                    && let Some(template) = heading_template()
                {
                    item.cast::<IControl>()?.SetTemplate(&template)?;
                }
                item.cast()?
            }
            Kind::Toggle(key, checked) => {
                // 체크를 아이콘으로 둔 평범한 항목: WinUI의 토글 항목은 아이콘 옆에 따로 체크 열을
                // 두어 글을 줄에서 밀어냅니다.
                let item = MenuFlyoutItem::new()?;
                choice(&item.cast()?, text, checked.then_some(CHECK), key, revokers, on_click)?;
                item.cast()?
            }
            Kind::Radio { key, checked } => {
                let item = MenuFlyoutItem::new()?;
                choice(&item.cast()?, text, checked.then_some(DOT), key, revokers, on_click)?;
                item.cast()?
            }
        };
        if !entry.enabled {
            item.cast::<IControl>()?.SetIsEnabled(false)?;
        }
        if !matches!(entry.kind, Kind::Separator) {
            item.cast::<IControl>()?.SetPadding(MOUSE_PADDING)?;
        }
        into.Append(&item)?;
    }
    Ok(())
}

/// 고를 수 있는 줄의 글, 아이콘, 클릭.
fn choice(
    item: &IMenuFlyoutItem,
    text: &str,
    glyph: Option<char>,
    key: &str,
    revokers: &mut Vec<EventRevoker>,
    on_click: &Rc<OnClick>,
) -> Result<()> {
    item.SetText(text)?;
    if let Some(glyph) = glyph {
        item.SetIcon(&icon(glyph)?)?;
    }
    if !key.is_empty() {
        let (key, on_click) = (key.to_string(), on_click.clone());
        revokers.push(item.Click(move |_, _| on_click(key.clone()))?);
    }
    Ok(())
}

fn icon(glyph: char) -> Result<IconElement> {
    let icon = FontIcon::new()?;
    icon.SetGlyph(&glyph.to_string())?;
    icon.cast()
}

/// 제목 줄의 템플릿: 메뉴 줄의 바깥 여백과 (마우스용) 안쪽 여백 안에 비활성 색의 글만 두어 아이콘과
/// 맞춥니다. XAML이 불러오지 못하면 None이고 줄은 평소 모습입니다.
fn heading_template() -> Option<ControlTemplate> {
    const XAML: &str = r#"<ControlTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" TargetType="MenuFlyoutItem">
  <Grid Margin="{StaticResource MenuFlyoutItemMargin}" Padding="{ThemeResource MenuFlyoutItemThemePaddingNarrow}" Background="Transparent">
    <TextBlock Text="{TemplateBinding Text}" Foreground="{ThemeResource MenuFlyoutItemForegroundDisabled}" TextTrimming="Clip" VerticalAlignment="Center" />
  </Grid>
</ControlTemplate>"#;
    thread_local! {
        static HEADING: RefCell<Option<Option<ControlTemplate>>> = const { RefCell::new(None) };
    }
    HEADING.with(|h| {
        h.borrow_mut()
            .get_or_insert_with(|| XamlReader::Load(XAML).and_then(|t| t.cast()).ok())
            .clone()
    })
}

/// 트레이 메뉴가 쓰는 Windows(작업 표시줄) 테마.
fn system_theme() -> ElementTheme {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: value와 size가 DWORD를 가리킵니다.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    match (status, value) {
        (0, 0) => ElementTheme::Dark,
        (0, _) => ElementTheme::Light,
        _ => ElementTheme::Default,
    }
}
