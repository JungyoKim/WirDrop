//! 메뉴를 열 자리: 점에 붙이거나 점을 가운데로, 작업 영역 안으로 들이고, 항목으로 크기를 셈합니다.
use super::*;

/// x, y에 요청한 메뉴의 기준점과 여는 방향. 작업 영역 밖의 점은 작업 표시줄 위이므로 가장자리에서
/// GAP만큼 안으로 옮깁니다. 트레이 메뉴처럼 화면 아래쪽의 점이면 위로, 위쪽이면 아래로, 오른쪽
/// 절반이면 왼쪽으로, 아니면 오른쪽으로 엽니다.
pub(super) fn place(x: i32, y: i32) -> (i32, i32, FlyoutPlacementMode) {
    let (work, dpi) = screen_at(x, y);
    let gap = GAP * dpi as i32 / 96;
    let x = if x >= work.right {
        work.right - gap
    } else if x < work.left {
        work.left + gap
    } else {
        x
    };
    let y = if y >= work.bottom {
        work.bottom - gap
    } else if y < work.top {
        work.top + gap
    } else {
        y
    };
    let low = y > (work.top + work.bottom) / 2;
    let right = x > (work.left + work.right) / 2;
    let placement = match (low, right) {
        (true, true) => FlyoutPlacementMode::TopEdgeAlignedRight,
        (true, false) => FlyoutPlacementMode::TopEdgeAlignedLeft,
        (false, true) => FlyoutPlacementMode::BottomEdgeAlignedRight,
        (false, false) => FlyoutPlacementMode::BottomEdgeAlignedLeft,
    };
    (x, y, placement)
}

/// entries를 담은 메뉴가 x, y를 가운데로 열리게 할 기준점: 크기는 항목으로 셈하고, 안 맞으면 작업
/// 영역 가장자리 안으로 들이며 아래에 항목 두 개만큼 여유를 둡니다 (보는 동안 늘어나는 목록은
/// 아래로 늘어납니다).
pub(super) fn centre(x: i32, y: i32, entries: &[Entry]) -> (i32, i32, FlyoutPlacementMode) {
    let (work, dpi) = screen_at(x, y);
    let scale = |px: i32| px * dpi as i32 / 96;
    let rows: i32 = entries
        .iter()
        .map(|e| if matches!(e.kind, Kind::Separator) { SEPARATOR } else { ROW })
        .sum();
    let height = scale(rows + 2 * MENU_PADDING);
    let marks = [
        entries.iter().any(|e| matches!(e.kind, Kind::Sub(_))),
        entries.iter().any(|e| matches!(e.kind, Kind::Toggle(..) | Kind::Radio { .. })),
    ];
    let room = TEXT_START + TEXT_END + MARK * marks.iter().filter(|m| **m).count() as i32;
    let width = (widest(entries, dpi) + scale(room)).min(scale(MENU_MAX_WIDTH));
    centred(x, y, width, height, scale(2 * ROW), work, dpi)
}

/// width × height(물리 픽셀) 플라이아웃이 x, y를 가운데로 열리게 할 기준점: 안 맞으면 work의
/// 가장자리 안으로 들이고, 아래에 spare만큼 비워 둡니다.
pub(super) fn centred(x: i32, y: i32, width: i32, height: i32, spare: i32, work: RECT, dpi: u32) -> (i32, i32, FlyoutPlacementMode) {
    let left = (x - width / 2).clamp(work.left, (work.right - width).max(work.left));
    let top = (y - height / 2).clamp(work.top, (work.bottom - height - spare).max(work.top));
    // 플라이아웃은 호스트 창의 2x2 사각형 아래, 그 왼쪽에 맞춰 열립니다.
    (
        left,
        top - FLYOUT_OFFSET * dpi as i32 / 96 - HOST_SIZE,
        FlyoutPlacementMode::BottomEdgeAlignedLeft,
    )
}

/// x, y가 있는 (가장 가까운) 화면의 작업 영역과 그 화면의 DPI.
pub(super) fn screen_at(x: i32, y: i32) -> (RECT, u32) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let (mut dpi, mut unused) = (96u32, 0u32);
    // SAFETY: info는 호출에 맞는 크기이고 DPI 출력 포인터는 지역 변수입니다.
    let work = unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused);
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcWork
        } else {
            RECT {
                left: x,
                top: y,
                right: x,
                bottom: y,
            }
        }
    };
    (work, dpi)
}

/// 메뉴 글꼴로 잰 entries 글 가운데 가장 넓은 폭(dpi에서의 물리 픽셀).
pub(super) fn widest(entries: &[Entry], dpi: u32) -> i32 {
    use windows_sys::Win32::Foundation::SIZE;
    use windows_sys::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, GetDC, GetTextExtentPoint32W,
        OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject,
    };
    let mut widest = 0;
    // SAFETY: 화면 DC와 글꼴, 둘 다 돌려줍니다. 글은 각각 그 길이의 버퍼입니다.
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let font = CreateFontW(
            -(MENU_FONT_SIZE * dpi as i32 / 96),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            OUT_DEFAULT_PRECIS.into(),
            CLIP_DEFAULT_PRECIS.into(),
            CLEARTYPE_QUALITY.into(),
            0,
            MENU_FONT,
        );
        let old = SelectObject(dc, font);
        for e in entries.iter().filter(|e| !matches!(e.kind, Kind::Separator)) {
            let text: Vec<u16> = e.text.encode_utf16().collect();
            let mut size = SIZE { cx: 0, cy: 0 };
            if GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32, &mut size) != 0 {
                widest = widest.max(size.cx);
            }
        }
        SelectObject(dc, old);
        DeleteObject(font);
        ReleaseDC(std::ptr::null_mut(), dc);
    }
    widest
}
