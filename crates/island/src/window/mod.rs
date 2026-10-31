//! 섬의 스레드와 창: 합성기가 그려 넣는, 테두리 없고 맨 위에 있으며 (질문할 때 말고는) 포커스를
//! 받지 않는 창. 섬이 가진 카드들과, 그 가운데 무엇이 열려 있거나 작게 떠 있는지.
mod input;
mod keys;
mod pointer;
mod screen;

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use windows::UI::Composition::Compositor;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW,
    MonitorFromPoint, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::Ime::{HIMC, ImmAssociateContextEx, ImmDisableIME};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, MOD_ALT, MOD_NOREPEAT, MOD_WIN, RegisterHotKey, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, UnregisterHotKey, VIRTUAL_KEY, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::{QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, IDC_HAND, IDC_IBEAM, IsWindow, KillTimer, LoadCursorW, MA_ACTIVATE, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, WM_APP, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_GETOBJECT, WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Error, Interface, PCWSTR, Result, w};

use crate::access::{self, Access, Element, Role};
use crate::gfx::{Gfx, device_lost};
use crate::layout::{self, CANVAS_H, CANVAS_W, Face, Measure, Mode, Rect, Style, Typing};
use crate::scene::Scene;
use crate::{Answer, Card, Cmd, Kind, Position, thumbs};
use input::{Input, clipboard, wipe};
use keys::shift;
use pointer::point;
pub use screen::full_screen;

const WM_CMD: u32 = WM_APP + 1;
/// 미리보기를 만들었습니다.
const WM_THUMB: u32 = WM_APP + 2;
/// 창이 바쁜 동안 (자기 SetFocus, 다른 호출 안의 활성화) 포커스가 오갔습니다: 바쁘지 않을 때 다시
/// 봅니다.
const WM_FOCUS: u32 = WM_APP + 3;
/// (WinUser.h)
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_CAPTURECHANGED: u32 = 0x0215;
/// (WinUser.h; windows-rs에는 UI::Controls 아래에 있습니다.)
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER: usize = 1;
/// 새 전송은 이만큼 열려 있다가 작아집니다.
const OPEN_FOR: Duration = Duration::from_secs(4);
/// 결과나 알림이 떠 있는 시간 (버튼이 있으면 더 길게).
const RESULT_FOR: Duration = Duration::from_secs(6);
const BUTTONS_FOR: Duration = Duration::from_secs(9);
/// 마우스가 떠난 뒤 섬이 열려 있는 시간.
const GRACE: Duration = Duration::from_millis(900);
/// 쉬는 섬, 그리고 움직이는 섬 둘레로 창이 더 받는 여유(pt).
const REST_MARGIN: f32 = 4.0;
const MOVING_MARGIN: f32 = 24.0;
/// 전체 화면 앱이 있으면 섬은 기다렸다가 이만큼 뒤에 다시 봅니다.
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// 답한 질문이 다음 것(다음 질문, 진행 상황)에 자리를 내주려고 길어야 이만큼 떠 있습니다.
const ANSWERED_FOR: Duration = Duration::from_secs(10);
/// 입력칸이 받는 최대 글자 수.
const MAX_TYPED: usize = 256;
/// 떠 있는 카드로 키보드를 가져오는 전역 단축키 Win+Alt+A (카드가 떠 있는 동안만 등록).
const HOTKEY: i32 = 1;
const HOTKEY_KEY: u32 = b'A' as u32;
/// 작게 떠 있던 카드를 누르거나 단축키로 부르면 이만큼 펼쳐 둔다.
const PEEK_FOR: Duration = Duration::from_secs(6);

/// 명령이 왔다고 섬의 스레드를 깨웁니다 (PostMessage용 창 핸들).
#[derive(Clone)]
pub struct Waker(isize);

impl Waker {
    pub fn wake(&self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as _)), WM_CMD, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn spawn(rx: Receiver<Cmd>, on_action: Box<dyn Fn(String) + Send>) -> std::io::Result<Waker> {
    let (tx, ready) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("island".into())
        .spawn(move || match App::new(rx, on_action) {
            Ok(app) => {
                let _ = tx.send(Ok(app.hwnd.0 as isize));
                APP.with(|a| *a.borrow_mut() = Some(app));
                pump();
                APP.with(|a| a.borrow_mut().take());
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
            }
        })?;
    match ready.recv() {
        Ok(Ok(hwnd)) => Ok(Waker(hwnd)),
        Ok(Err(e)) => Err(std::io::Error::other(e)),
        Err(_) => Err(std::io::Error::other("the island's thread ended")),
    }
}

fn pump() {
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// 화면 읽기 프로그램에 보이는 창 (access.rs): APP이 바쁠 때도 UI Automation에 답합니다.
    static ACCESS: RefCell<Option<Access>> = const { RefCell::new(None) };
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_GETOBJECT
        && let Some(result) = ACCESS.with(|a| a.borrow().as_ref().and_then(|a| a.get_object(hwnd, wparam, lparam)))
    {
        return result;
    }
    // 메시지를 보내는 Win32 호출(SetWindowPos)은 앱을 빌려 쓰는 동안 여기로 돌아옵니다: 그것들은
    // DefWindowProc로 보냅니다.
    let handled = APP.with(|a| match a.try_borrow_mut() {
        Ok(mut a) => a.as_mut().and_then(|app| app.message(msg, wparam, lparam)),
        Err(_) => {
            if msg == WM_SETFOCUS || msg == WM_KILLFOCUS {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_FOCUS, WPARAM(0), LPARAM(0));
                }
            }
            None
        }
    });
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

/// 카드와 섬이 그 카드에 대해 아는 것.
struct Entry {
    card: Card,
    version: u64,
    /// 이때까지 열려 있음 (시작하는 전송, 또는 눌러서 연 것).
    open_until: Option<Instant>,
    /// 이때 사라짐 (결과, 알림).
    ends: Option<Instant>,
    /// 버튼을 눌렀습니다: 그 결과를 기다리는 중.
    acted: bool,
    /// 입력 중인 질문의 입력칸.
    input: Option<Input>,
    /// 질문이 떠 있는 동안 같은 태그로 온 카드: 질문에 답하면 그 자리를 차지합니다.
    after: Option<Card>,
    /// 진행 막대가 떴습니다 (그리고 소리를 냈습니다).
    rang: bool,
    /// 마지막으로 온 때 (명령의 버전): 가장 새 이벤트가 섬을 차지합니다.
    seq: u64,
}

impl Entry {
    /// 답을 기다리는 질문.
    fn asking(&self) -> bool {
        self.input.as_ref().is_some_and(|i| i.reply.is_some())
    }

    /// 진행 막대가 처음 보입니다: 소리를 내야 하면 true.
    fn ring(&mut self) -> bool {
        let bar = self.card.kind == Kind::Transfer && self.card.progress.as_ref().is_some_and(|p| p.fraction.is_some());
        if self.rang || !bar {
            return false;
        }
        self.rang = true;
        self.card.chime
    }
}

/// 지금 보여 준 카드가 작아질 때(전송)와 사라질 때(결과, 알림).
fn timing(card: &Card, now: Instant) -> (Option<Instant>, Option<Instant>) {
    let ends = match card.kind {
        Kind::Result | Kind::Info => Some(now + if card.buttons.is_empty() { RESULT_FOR } else { BUTTONS_FOR }),
        _ => None,
    };
    ((card.kind == Kind::Transfer).then_some(now + OPEN_FOR), ends)
}

/// 떠 있는 것.
struct Up {
    tag: String,
    mode: Mode,
    version: u64,
    face: Face,
}

struct App {
    hwnd: HWND,
    _queue: windows::System::DispatcherQueueController,
    _target: DesktopWindowTarget,
    gfx: Gfx,
    scene: Scene,
    rx: Receiver<Cmd>,
    /// 일꾼에게 보낼 미리보기 요청과 만든 것.
    asks: Sender<thumbs::Ask>,
    made: Receiver<thumbs::Made>,
    on_action: Box<dyn Fn(String) + Send>,
    /// 섬이 나오는 자리 (다음에 나올 때 반영).
    position: Position,
    cards: Vec<Entry>,
    up: Option<Up>,
    visible: bool,
    /// 화면 위 창의 왼쪽 위(픽셀).
    origin: (i32, i32),
    hover: bool,
    hot: Option<usize>,
    pressed: Option<usize>,
    hide_at: Option<Instant>,
    region_at: Option<Instant>,
    retry_at: Option<Instant>,
    version: u64,
    /// 질문 때문에 가져온 키보드: 돌려줄, 그 전에 키보드를 가졌던 창.
    keys: Option<isize>,
    /// 창이 키보드를 가지고 있습니다.
    focused: bool,
    /// 마우스 위치(pt) (입력칸 위의 커서용).
    mouse: (f32, f32),
    /// 섬이 뜨면 낼 Windows 알림음.
    chime_due: bool,
    /// 입력칸에서 마우스로 선택하는 중 (버튼을 누르고 있음).
    selecting: bool,
    /// 화면 읽기 프로그램에 마지막으로 알린 카드 (태그와 제목).
    spoken: String,
    /// 키보드로 고른 버튼 (None이면 입력칸이거나 고른 것 없음).
    nav: Option<usize>,
    /// 단축키로 키보드를 가져왔다: 입력칸 없는 카드에서도 키를 받는다.
    navigating: bool,
    /// 방금 처리한 키(Tab, Enter, Space, Esc)가 뒤이어 만들 WM_CHAR를 버린다.
    skip_char: bool,
    /// 단축키가 등록되어 있다.
    hotkey: bool,
}

impl App {
    fn new(rx: Receiver<Cmd>, on_action: Box<dyn Fn(String) + Send>) -> Result<App> {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            // 섬에 입력하는 것은 Apple ID, 암호, 코드입니다: Windows 암호 칸처럼 IME를 쓰지
            // 않습니다 (한글 입력은 글자를 조합합니다).
            let _ = ImmDisableIME(GetCurrentThreadId());
            let queue = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?;
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                // 두 번 누르면 입력칸의 글을 모두 선택합니다.
                style: windows::Win32::UI::WindowsAndMessaging::CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: w!("WirDrop.Island"),
                ..Default::default()
            };
            RegisterClassExW(&class);
            let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(
                ex,
                w!("WirDrop.Island"),
                w!("WirDrop"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let _ = ImmAssociateContextEx(hwnd, HIMC::default(), 0);
            ACCESS.with(|a| *a.borrow_mut() = Some(Access::new(hwnd)));
            let comp = Compositor::new()?;
            let interop: ICompositorDesktopInterop = comp.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = comp.CreateContainerVisual()?;
            target.SetRoot(&root)?;
            let gfx = Gfx::new(&comp)?;
            let scene = Scene::new(&comp, &root)?;
            let raw = hwnd.0 as isize;
            let (asks, made) = thumbs::spawn(move || {
                let _ = PostMessageW(Some(HWND(raw as _)), WM_THUMB, WPARAM(0), LPARAM(0));
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
            Ok(App {
                hwnd,
                _queue: queue,
                _target: target,
                gfx,
                scene,
                rx,
                asks,
                made,
                on_action,
                position: Position::Top,
                cards: Vec::new(),
                up: None,
                visible: false,
                origin: (0, 0),
                hover: false,
                hot: None,
                pressed: None,
                hide_at: None,
                region_at: None,
                retry_at: None,
                version: 0,
                keys: None,
                focused: false,
                mouse: (0.0, 0.0),
                chime_due: false,
                selecting: false,
                spoken: String::new(),
                nav: None,
                navigating: false,
                skip_char: false,
                hotkey: false,
            })
        }
    }

    fn message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let now = Instant::now();
        match msg {
            WM_CMD => {
                while let Ok(cmd) = self.rx.try_recv() {
                    if !self.command(cmd, now) {
                        unsafe {
                            let _ = DestroyWindow(self.hwnd);
                        }
                        return Some(LRESULT(0));
                    }
                }
                self.reconcile(now);
            }
            WM_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER);
                }
                self.reconcile(now);
            }
            WM_THUMB => {
                let mut any = false;
                while let Ok(m) = self.made.try_recv() {
                    self.gfx.made(m.path, m.side, m.bitmap);
                    any = true;
                }
                if any && let Err(e) = self.scene.repaint(&self.gfx) {
                    tracing::warn!("island: {e}");
                }
                self.ask_thumbnails();
            }
            // 무언가 묻는 동안에만 포커스를 받습니다.
            WM_MOUSEACTIVATE => return Some(LRESULT((if self.keys.is_some() { MA_ACTIVATE } else { MA_NOACTIVATE }) as isize)),
            WM_SETFOCUS | WM_KILLFOCUS | WM_FOCUS => self.refocus(msg != WM_KILLFOCUS, now),
            access::WM_PRESS => self.press(wparam.0, now),
            WM_HOTKEY if wparam.0 == HOTKEY as usize => self.summon(now),
            WM_CHAR => self.typed(wparam.0 as u16, now),
            WM_KEYDOWN => {
                let key = VIRTUAL_KEY(wparam.0 as u16);
                if !self.navigate(key, now) && !self.key(key, now) {
                    return None;
                }
            }
            // 키보드를 가진 상태의 Alt+F4: 창이 아니라 질문을 닫습니다.
            WM_CLOSE => self.answer(false, now),
            WM_NCHITTEST => return Some(LRESULT(HTCLIENT as isize)),
            WM_SETCURSOR => {
                let clickable = self.hot.is_some() || self.up.as_ref().is_some_and(|u| u.mode == Mode::Small);
                let ibeam = self.keys.is_some() && self.field_at(self.mouse.0, self.mouse.1);
                unsafe {
                    SetCursor(
                        LoadCursorW(
                            None,
                            if ibeam {
                                IDC_IBEAM
                            } else if clickable {
                                IDC_HAND
                            } else {
                                IDC_ARROW
                            },
                        )
                        .ok(),
                    );
                }
                return Some(LRESULT(1));
            }
            WM_MOUSEMOVE => {
                self.mouse = point(lparam, self.scene.scale());
                if self.selecting {
                    self.place_caret(self.mouse.0, true, now);
                } else {
                    self.mouse_move(self.mouse, now);
                }
            }
            WM_MOUSELEAVE => self.mouse_leave(now),
            WM_LBUTTONDOWN => {
                let (x, y) = point(lparam, self.scene.scale());
                if self.keys.is_some() {
                    unsafe {
                        let _ = SetFocus(Some(self.hwnd));
                    }
                    self.refocus(true, now);
                }
                // 입력칸에서는 여느 입력 상자처럼: 누른 곳에 캐럿이 가고 (Shift면 선택이 거기까지
                // 늘어남), 끌면 선택합니다.
                if self.keys.is_some() && self.field_at(x, y) && self.place_caret(x, shift(), now) {
                    self.set_nav(None, now);
                    self.selecting = true;
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                } else {
                    self.pressed = self.hot;
                    let _ = self.scene.hover(self.hot, true);
                }
            }
            WM_LBUTTONDBLCLK => {
                let (x, y) = point(lparam, self.scene.scale());
                if self.keys.is_some() && self.field_at(x, y) {
                    self.select_all(now);
                } else {
                    self.pressed = self.hot;
                    let _ = self.scene.hover(self.hot, true);
                }
            }
            WM_LBUTTONUP => {
                if std::mem::take(&mut self.selecting) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                } else {
                    self.click(point(lparam, self.scene.scale()), now);
                }
            }
            WM_CAPTURECHANGED => self.selecting = false,
            WM_RBUTTONUP => self.dismiss(now),
            WM_DISPLAYCHANGE => {
                if self.visible {
                    self.place();
                    self.ask_thumbnails();
                }
            }
            WM_DESTROY => unsafe { PostQuitMessage(0) },
            _ => return None,
        }
        let _ = wparam;
        Some(LRESULT(0))
    }

    /// 끝내려면 false.
    fn command(&mut self, cmd: Cmd, now: Instant) -> bool {
        self.version += 1;
        let version = self.version;
        match cmd {
            Cmd::Show(card) => {
                let card = *card;
                // 그 태그의 질문이 떠 있으면 답할 때까지 뒤에서 기다립니다.
                if let Some(e) = self.cards.iter_mut().find(|e| e.card.tag == card.tag && e.asking()) {
                    e.after = Some(card);
                    return true;
                }
                let (open_until, ends) = timing(&card, now);
                match self.cards.iter_mut().find(|e| e.card.tag == card.tag) {
                    Some(e) => {
                        if e.card.kind != card.kind {
                            e.open_until = open_until;
                            e.ends = ends;
                            e.acted = false;
                        }
                        e.card = card;
                        e.version = version;
                        e.input = None;
                        e.seq = version;
                        self.chime_due |= e.ring();
                    }
                    None => {
                        let mut e = Entry {
                            card,
                            version,
                            open_until,
                            ends,
                            acted: false,
                            input: None,
                            after: None,
                            rang: false,
                            seq: version,
                        };
                        self.chime_due |= e.ring();
                        self.cards.push(e);
                    }
                }
            }
            Cmd::Ask(card, reply) => {
                let card = *card;
                let input = Some(Input::new(card.field.as_ref().map_or("", |f| f.value.as_str()), reply));
                match self.cards.iter_mut().find(|e| e.card.tag == card.tag) {
                    Some(e) => {
                        if let Some(earlier) = e.input.as_mut().and_then(|i| i.reply.take()) {
                            let _ = earlier.send(Answer::Closed);
                        }
                        e.card = card;
                        e.version = version;
                        e.seq = version;
                        (e.open_until, e.ends, e.acted, e.input, e.after) = (None, None, false, input, None);
                    }
                    None => self.cards.push(Entry {
                        card,
                        version,
                        open_until: None,
                        ends: None,
                        acted: false,
                        input,
                        after: None,
                        rang: false,
                        seq: version,
                    }),
                }
            }
            Cmd::Progress(tag, progress) => {
                if let Some(e) = self.cards.iter_mut().find(|e| e.card.tag == tag) {
                    if e.asking() {
                        if let Some(after) = &mut e.after {
                            after.progress = Some(progress);
                        }
                    } else {
                        e.card.progress = Some(progress);
                        e.version = version;
                        self.chime_due |= e.ring();
                    }
                }
            }
            Cmd::Remove(tag) => {
                for e in self.cards.iter_mut().filter(|e| e.card.tag == tag) {
                    if let Some(reply) = e.input.as_mut().and_then(|i| i.reply.take()) {
                        let _ = reply.send(Answer::Closed);
                    }
                }
                self.cards.retain(|e| e.card.tag != tag);
            }
            // 다른 자리로: 떠 있는 것은 바로 사라지고 새 자리에서 나옵니다.
            Cmd::Place(position) => {
                if position != self.position {
                    if self.visible {
                        self.up = None;
                        self.hide();
                    }
                    self.position = position;
                }
            }
            Cmd::Quit => return false,
        }
        true
    }

    /// 보여 줄 카드와 방식: 질문 먼저, 그다음 가장 새 이벤트 (진행 중인 전송, 아직 떠 있는 결과나
    /// 알림). 마우스가 올라와 있으면 떠 있는 것이 열린 채 머뭅니다.
    fn choose(&self, now: Instant) -> Option<(usize, Mode)> {
        if self.held()
            && let Some(up) = &self.up
            && let Some(i) = self.cards.iter().position(|e| e.card.tag == up.tag)
        {
            return Some((i, Mode::Open));
        }
        if let Some(i) = self.cards.iter().position(|e| e.card.kind == Kind::Ask) {
            return Some((i, Mode::Open));
        }
        // 진행 중인 것과 방금 일어난 것 가운데 가장 새 것: 새 이벤트는 곧바로 섬을 차지합니다 (떠
        // 있던 결과는 기다려 주지 않습니다).
        let i = (0..self.cards.len())
            .filter(|&i| self.cards[i].card.kind == Kind::Transfer || self.cards[i].ends.is_some_and(|t| t > now))
            .max_by_key(|&i| self.cards[i].seq)?;
        let e = &self.cards[i];
        let open = e.ends.is_some() || e.open_until.is_some_and(|t| t > now);
        Some((i, if open { Mode::Open } else { Mode::Small }))
    }

    fn reconcile(&mut self, now: Instant) {
        if let Err(e) = self.try_reconcile(now) {
            if device_lost(&e) {
                tracing::info!("island: GPU device lost; drawing again");
                if self.gfx.renew().is_ok() {
                    let _ = self.scene.redraw(&self.gfx);
                }
            } else {
                tracing::warn!("island: {e}");
            }
        }
        self.schedule(now);
        self.ask_thumbnails();
        // 섬이 뜰 때 한 번 (전체 화면 앱 뒤에서는 배너처럼 조용히).
        if std::mem::take(&mut self.chime_due) && self.visible {
            chime();
        }
        self.keyboard(now);
        self.sync_hotkey();
        self.publish();
    }

    /// 그리기가 요청한 미리보기를 일꾼에게 보냅니다.
    fn ask_thumbnails(&self) {
        for ask in self.gfx.asks() {
            let _ = self.asks.send(ask);
        }
    }

    fn try_reconcile(&mut self, now: Instant) -> Result<()> {
        // 시간이 다 된 결과와 알림은 사라집니다. 마우스가 올라와 있으면 남습니다.
        let hovered = if self.held() { self.up.as_ref().map(|u| u.tag.clone()) } else { None };
        self.cards
            .retain(|e| e.ends.is_none_or(|t| t > now) || Some(&e.card.tag) == hovered.as_ref());
        // 더 새 이벤트에 자리를 내준 결과나 알림은 그 뒤에 다시 나오지 않습니다.
        if hovered.is_none()
            && !self.cards.iter().any(|e| e.card.kind == Kind::Ask)
            && let Some(newest) = self
                .cards
                .iter()
                .filter(|e| e.card.kind == Kind::Transfer || e.ends.is_some())
                .map(|e| e.seq)
                .max()
        {
            self.cards.retain(|e| e.ends.is_none() || e.seq >= newest);
        }
        if self.hide_at.is_some_and(|t| t <= now) && self.up.is_none() {
            self.hide();
        }
        if self.region_at.is_some_and(|t| t <= now) {
            self.region_at = None;
            self.set_region(now, REST_MARGIN);
        }
        self.scene.prune(now);
        let Some((i, mode)) = self.choose(now) else {
            if self.up.take().is_some() {
                let secs = self.scene.vanish(&self.gfx, now)?;
                self.set_region(now, MOVING_MARGIN);
                // 지금부터: 이것이 끝나면 합성기가 시작합니다.
                self.hide_at = Some(Instant::now() + Duration::from_secs_f64(secs));
                self.hot = None;
                self.pressed = None;
            }
            return Ok(());
        };
        let fresh = !self.visible;
        if !self.visible {
            if full_screen() {
                tracing::debug!("island: something is full screen; waiting");
                self.retry_at = Some(now + BUSY_RETRY);
                return Ok(());
            }
            self.place();
        }
        self.retry_at = None;
        let entry = &self.cards[i];
        let (tag, version) = (entry.card.tag.clone(), entry.version);
        if let Some(up) = &self.up
            && up.tag == tag
            && up.mode == mode
            && up.version == version
        {
            return Ok(());
        }
        let secret = entry.card.field.as_ref().is_some_and(|f| f.secret);
        let typing = entry.input.as_ref().map(|i| i.typing(secret, self.focused && self.nav.is_none()));
        let face = layout::face(&entry.card, mode, &self.gfx, typing.as_ref());
        // 레이아웃이 캐럿을 보이게 맞춘 입력칸의 스크롤. 다음 편집에 씁니다.
        if let Some(input) = self.cards[i].input.as_mut() {
            input.scroll = face.field_scroll;
        }
        let same = self
            .up
            .as_ref()
            .is_some_and(|u| u.tag == tag && u.mode == mode && u.face.same_shape(&face));
        if same {
            self.scene.update(&self.gfx, face.clone(), now)?;
        } else {
            self.hide_at = None;
            self.scene.appear(&self.gfx, fresh, now)?;
            let secs = self.scene.morph(&self.gfx, face.w, face.h, face.radius, now)?;
            self.scene.enter(&self.gfx, face.clone(), now)?;
            self.hot = None;
            self.pressed = None;
            // 새 모양의 카드: 키보드로 고르던 버튼은 없던 일이 된다.
            self.nav = None;
            self.navigating = false;
            self.set_region(now, MOVING_MARGIN);
            self.region_at = Some(Instant::now() + Duration::from_secs_f64(secs + 0.05));
        }
        tracing::debug!("island: {tag} {mode:?} ({same})");
        self.up = Some(Up { tag, mode, version, face });
        Ok(())
    }

    /// 스스로 일어날 다음 일에 타이머를 맞춥니다.
    fn schedule(&mut self, now: Instant) {
        let mut next: Vec<Instant> = [self.hide_at, self.region_at, self.retry_at, self.scene.prune(now)]
            .into_iter()
            .flatten()
            .collect();
        if let Some(up) = &self.up
            && let Some(e) = self.cards.iter().find(|e| e.card.tag == up.tag)
            && !self.held()
        {
            if up.mode == Mode::Open
                && let Some(t) = e.open_until
            {
                next.push(t);
            }
            next.extend(e.ends);
        }
        // 다른 카드(질문) 아래에 깔린 결과도 시간이 지나면 사라집니다.
        next.extend(self.cards.iter().filter_map(|e| e.ends).filter(|t| *t > now));
        if let Some(t) = next.into_iter().min() {
            let ms = t.saturating_duration_since(now).as_millis().clamp(10, 60_000) as u32;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER, ms, None);
            }
        }
    }

    /// 화면 읽기 프로그램에 보일 떠 있는 것 (access.rs): 읽을 수 있게 두고, 카드가 오거나 새 말을
    /// 할 때 (물음, 진행, 끝) 알립니다.
    fn publish(&mut self) {
        let up = self.up.as_ref().filter(|_| self.visible);
        let card = up.and_then(|u| self.cards.iter().find(|e| e.card.tag == u.tag)).map(|e| &e.card);
        let (Some(up), Some(card)) = (up, card) else {
            ACCESS.with(|a| a.borrow().as_ref().map(|a| a.set(String::new(), Vec::new())));
            self.spoken.clear();
            return;
        };
        let name = [card.title.as_str(), card.detail.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(". ");
        let scale = self.scene.scale();
        let origin = self.scene.top().map(|t| (t.origin.X, t.origin.Y)).unwrap_or_default();
        let mut window = windows::Win32::Foundation::RECT::default();
        // SAFETY: 우리 창의 위치를 지역 변수로.
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut window) };
        let on_screen = |r: &Rect| windows::Win32::Foundation::RECT {
            left: window.left + ((origin.0 + r.x) * scale).round() as i32,
            top: window.top + ((origin.1 + r.y) * scale).round() as i32,
            right: window.left + ((origin.0 + r.right()) * scale).round() as i32,
            bottom: window.top + ((origin.1 + r.bottom()) * scale).round() as i32,
        };
        let mut elements = Vec::new();
        if let (Some(rect), Some(field)) = (up.face.field, &card.field) {
            let name = if field.placeholder.is_empty() {
                card.title.clone()
            } else {
                field.placeholder.clone()
            };
            let focused = self.focused && self.nav.is_none();
            elements.push(Element {
                role: Role::Field {
                    secret: field.secret,
                    focused,
                },
                name,
                rect: on_screen(&rect),
            });
        }
        for (b, (rect, args)) in up.face.buttons.iter().enumerate() {
            elements.push(Element {
                role: Role::Button(b),
                name: button_name(&up.face, card, rect, args),
                rect: on_screen(rect),
            });
        }
        let said = format!("{}\n{}", up.tag, card.title);
        let tell = said != self.spoken;
        self.spoken = said;
        ACCESS.with(|a| {
            if let Some(a) = a.borrow().as_ref() {
                a.set(name.clone(), elements);
                if tell {
                    a.announce(&name);
                }
            }
        });
    }

    /// 떠 있는 카드가 답을 기다리는 질문이면 그 번호.
    fn asking_index(&self) -> Option<usize> {
        let up = self.up.as_ref()?;
        self.cards.iter().position(|e| e.card.tag == up.tag && e.asking())
    }

    /// 카드의 입력칸이 바뀌었습니다 (또는 키보드가 오거나 갔습니다): 다시 그립니다.
    fn edited(&mut self, i: usize, now: Instant) {
        self.version += 1;
        if let Some(e) = self.cards.get_mut(i) {
            e.version = self.version;
        }
        self.reconcile(now);
    }
}

/// Windows 알림음 (Notification.Default), 알림이 낼 때처럼.
fn chime() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
    unsafe {
        let _ = PlaySoundW(w!("Notification.Default"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
    }
}

/// 화면 읽기 프로그램이 부를 버튼 이름: 글자, 둥근 버튼은 하는 일.
fn button_name(face: &Face, card: &Card, rect: &Rect, args: &str) -> String {
    let label = face.items.iter().find_map(|i| match i {
        layout::Item::Capsule { rect: r, label, .. } if r == rect => Some(label.clone()),
        _ => None,
    });
    match label {
        Some(label) => label,
        None if args == layout::CLOSE => crate::t("닫기", "Close").into(),
        None if args == layout::SUBMIT => card
            .field
            .as_ref()
            .map(|f| f.action.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| crate::t("확인", "OK").into()),
        None if card.cancel.as_deref() == Some(args) => crate::t("중지", "Stop").into(),
        None => card.title.clone(),
    }
}
