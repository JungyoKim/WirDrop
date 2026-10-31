//! 열린 플라이아웃을 위쪽을 잡고 창처럼 옮기기: 열려 있는 동안만 거는 저수준 마우스 후크.
use super::*;

/// 열린 플라이아웃의 손잡이: 옮길 수 있는 플라이아웃이 열려 있는 동안만 있는 저수준 마우스 후크.
/// 창의 위쪽 범위를 누르면 WinUI에 넘기지 않고 (클릭으로 보고 메뉴를 닫을 것이므로), 버튼을 뗄
/// 때까지 메뉴 스레드의 모든 창(플라이아웃, 하위 메뉴, 플라이아웃이 열린 호스트)을 포인터가 끌고
/// 다닙니다. 그래서 그 뒤에 연 하위 메뉴도 자기 줄 옆에 열립니다.
pub(super) struct Grip {
    pub(super) hook: HHOOK,
    /// 잡을 수 있는 아래쪽 거리(유효 픽셀).
    pub(super) reach: i32,
    /// 플라이아웃의 창.
    pub(super) window: HWND,
    pub(super) held: Option<Held>,
}

/// 옮기는 중인 플라이아웃: 누른 곳, 그리고 움직이는 창마다 원래 있던 곳.
#[derive(Clone)]
pub(super) struct Held {
    pub(super) from: POINT,
    pub(super) windows: Vec<(HWND, i32, i32)>,
}

/// 방금 연 플라이아웃을 위에서 reach만큼 아래까지 잡을 수 있게 합니다, 메뉴 스레드에서.
pub(super) fn grip_on(reach: i32) {
    let Some(host) = HOST.with_borrow(|h| h.as_ref().map(|h| h.hwnd)) else {
        return;
    };
    let at = window_rect(host);
    // 그 창은 호스트 창에 붙어 열리므로 가장 가까운 팝업 창입니다.
    let window = thread_windows(host)
        .into_iter()
        .filter(|w| class_of(*w) == POPUP_CLASS)
        .min_by_key(|w| distance(window_rect(*w), at.left, at.top));
    let Some(window) = window else { return };
    let hook = match GRIP.with_borrow(|g| g.as_ref().map(|g| g.hook)) {
        Some(hook) => hook,
        // SAFETY: 모든 스레드의 마우스 입력에 대한 우리 후크 프로시저.
        None => unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(grip_hook), GetModuleHandleW(std::ptr::null()), 0) },
    };
    if hook.is_null() {
        return;
    }
    GRIP.with_borrow_mut(|g| {
        *g = Some(Grip {
            hook,
            reach,
            window,
            held: None,
        })
    });
}

/// 손잡이가 있으면 끝냅니다.
pub(super) fn grip_off() {
    if let Some(grip) = GRIP.with_borrow_mut(Option::take) {
        // SAFETY: grip_on이 건 후크.
        unsafe { UnhookWindowsHookEx(grip.hook) };
    }
}

unsafe extern "system" fn grip_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: 저수준 마우스 후크의 lparam은 그것을 가리킵니다.
        let pt = unsafe { (*(lparam as *const MSLLHOOKSTRUCT)).pt };
        if grip_mouse(wparam as u32, pt) {
            return 1;
        }
    }
    // SAFETY: 이벤트를 넘깁니다.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// 손잡이에 알리는 마우스 동작: 다른 모든 곳에서 막으려면 true. 창을 보고 옮길 때 아무것도 빌려
/// 두지 않습니다: WinUI로 들어갈 수 있습니다.
pub(super) fn grip_mouse(msg: u32, pt: POINT) -> bool {
    let Some((window, reach, held)) = GRIP.with_borrow(|g| g.as_ref().map(|g| (g.window, g.reach, g.held.clone()))) else {
        return false;
    };
    match (msg, held) {
        (WM_LBUTTONDOWN, None) => {
            let top = window_rect(window).top;
            // SAFETY: 평범한 조회.
            let dpi = unsafe { GetDpiForWindow(window) }.max(96) as i32;
            // SAFETY: 평범한 조회들.
            let grabbed = unsafe { GetAncestor(WindowFromPoint(pt), GA_ROOT) == window } && pt.y < top + reach * dpi / 96;
            if !grabbed {
                return false;
            }
            let windows = thread_windows(window)
                .into_iter()
                .map(|w| {
                    let r = window_rect(w);
                    (w, r.left, r.top)
                })
                .collect();
            let held = Held { from: pt, windows };
            GRIP.with_borrow_mut(|g| g.as_mut().map(|g| g.held = Some(held)));
            true
        }
        (WM_MOUSEMOVE, Some(held)) => {
            let (dx, dy) = (pt.x - held.from.x, pt.y - held.from.y);
            for (w, x, y) in held.windows {
                // SAFETY: 메뉴 스레드의 창들이고, 이곳이 그 스레드입니다.
                unsafe { SetWindowPos(w, std::ptr::null_mut(), x + dx, y + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
            }
            false
        }
        (WM_LBUTTONUP, Some(_)) => {
            GRIP.with_borrow_mut(|g| g.as_mut().map(|g| g.held = None));
            true
        }
        _ => false,
    }
}

/// 보이는 창이 있는 스레드의 보이는 창들.
pub(super) fn thread_windows(any: HWND) -> Vec<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        // SAFETY: found는 아래의 Vec이고, hwnd는 그 스레드의 창입니다.
        unsafe {
            if IsWindowVisible(hwnd) != 0 {
                (*(found as *mut Vec<HWND>)).push(hwnd);
            }
        }
        1
    }
    let mut found: Vec<HWND> = Vec::new();
    // SAFETY: 각 호출은 호출보다 오래 사는 found에 넣기만 합니다.
    unsafe {
        let thread = GetWindowThreadProcessId(any, std::ptr::null_mut());
        EnumThreadWindows(thread, Some(each), &mut found as *mut Vec<HWND> as LPARAM);
    }
    found
}

pub(super) fn window_rect(hwnd: HWND) -> RECT {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: r은 지역 변수.
    unsafe { GetWindowRect(hwnd, &mut r) };
    r
}

pub(super) fn class_of(hwnd: HWND) -> String {
    let mut name = [0u16; 128];
    // SAFETY: name은 그 길이의 버퍼.
    let n = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..n.max(0) as usize])
}

/// x, y가 r에서 축마다 몇 칸 떨어졌는지. 안이면 0.
pub(super) fn distance(r: RECT, x: i32, y: i32) -> i32 {
    let dx = (r.left - x).max(x - r.right).max(0);
    let dy = (r.top - y).max(y - r.bottom).max(0);
    dx + dy
}
