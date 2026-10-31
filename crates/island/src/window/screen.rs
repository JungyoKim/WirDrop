//! 화면: 섬 창을 주 화면 가장자리에 두기, 마우스를 받는 영역, 전체 화면 앱 알아보기.
use super::*;

/// 앞에 전체 화면이 있습니다 (게임, 발표, 영상): Windows가 배너를 미루듯 섬도 기다립니다.
/// Direct3D와 발표 모드는 Windows가 알려 주지만, 그 "바쁨"은 믿을 수 없어서 (바탕 화면이 앞에 있을
/// 때도, 제목 표시줄 없는 창이 작업 표시줄을 보이며 최대화되어 있을 때도 그렇다고 합니다) 앞의 창을
/// 직접 잽니다.
pub fn full_screen() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) if s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE => true,
        _ => covers_its_screen(unsafe { GetForegroundWindow() }),
    }
}

/// 창이 작업 표시줄까지 모니터 전체를 덮습니다. 바탕 화면이나 작업 표시줄 자체, 크기 조절 테두리가
/// 있는 최대화 창(작업 표시줄이 숨으면 모니터를 덮습니다)은 아닙니다.
pub(super) fn covers_its_screen(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetClassNameW, GetWindowLongW, GetWindowRect, IsZoomed, WS_THICKFRAME};
    if hwnd.is_invalid() {
        return false;
    }
    let mut name = [0u16; 32];
    let n = unsafe { GetClassNameW(hwnd, &mut name) };
    let class = String::from_utf16_lossy(&name[..n.max(0) as usize]);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return false;
    }
    let framed = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & WS_THICKFRAME.0 != 0;
    if framed && unsafe { IsZoomed(hwnd) }.as_bool() {
        return false;
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let mut r = RECT::default();
    if monitor.is_invalid() || !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() || unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return false;
    }
    let m = info.rcMonitor;
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

impl App {
    /// 창을 늘 주 화면에서 섬이 나오는 자리에 둡니다 (섬이 가장자리 뒤에서 미끄러져 나오도록 작업
    /// 영역 가장자리에 붙여), 크기는 섬이 가장 클 때에 맞춰 그 화면의 DPI로.
    pub(super) fn place(&mut self) {
        unsafe {
            // 주 화면은 원점에 있는 화면입니다.
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut ex = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: size_of::<MONITORINFOEXW>() as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut ex.monitorInfo);
            let info = ex.monitorInfo;
            // 화면 새로 고침마다 키 프레임 하나.
            let mut mode = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if EnumDisplaySettingsW(PCWSTR(ex.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() && mode.dmDisplayFrequency > 1 {
                self.scene.set_rate(mode.dmDisplayFrequency as f64);
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx as f32 / 96.0;
            let (w, h) = ((CANVAS_W * scale).ceil() as i32, (CANVAS_H * scale).ceil() as i32);
            let (work, screen) = (info.rcWork, info.rcMonitor);
            // 섬이 나올 수 있는 화면 자체의 가장자리. 작업 표시줄의 가장자리는 아닙니다.
            let bezels = [
                work.top == screen.top,
                work.bottom == screen.bottom,
                work.left == screen.left,
                work.right == screen.right,
            ];
            self.scene.set_position(self.position, bezels);
            let (fx, fy) = self.position.anchor();
            let x = work.left + ((work.right - work.left - w) as f32 * fx).round() as i32;
            let y = work.top + ((work.bottom - work.top - h) as f32 * fy).round() as i32;
            if (scale - self.scene.scale()).abs() > 1e-3 {
                let _ = self.scene.set_scale(scale);
                let _ = self.scene.redraw(&self.gfx);
            }
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            self.origin = (x, y);
        }
        self.visible = true;
    }

    pub(super) fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            SetWindowRgn(self.hwnd, None, false);
        }
        self.scene.clear();
        self.visible = false;
        self.hide_at = None;
        self.hover = false;
    }

    /// 창은 섬 둘레에서만 (지금 자리와 향하는 자리에서 margin pt 바깥까지) 마우스를 받고 그립니다:
    /// 움직이는 동안에는 스프링과 흘러나오는 방울이 둘 다를 넘어가므로 더 넓게.
    pub(super) fn set_region(&self, now: Instant, margin: f32) {
        let (a, b) = self.scene.bounds(now);
        let u = a.union(&b).inflate(margin);
        let (x0, y0) = (u.x.max(0.0), u.y.max(0.0));
        let (x1, y1) = (u.right().min(CANVAS_W), u.bottom().min(CANVAS_H));
        let s = self.scene.scale();
        unsafe {
            let rgn = CreateRectRgn((x0 * s).floor() as i32, (y0 * s) as i32, (x1 * s).ceil() as i32, (y1 * s).ceil() as i32);
            SetWindowRgn(self.hwnd, Some(rgn), true);
        }
    }

    pub(super) fn island_rect(&self, now: Instant) -> Rect {
        self.scene.bounds(now).0
    }
}
