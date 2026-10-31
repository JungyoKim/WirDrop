//! 트레이 아이콘처럼 오른쪽 아래 근처에 견본 메뉴를 열고 고른 것을 찍습니다.
use std::time::Duration;

use wirdrop_menu::{Entry, Menu};

fn main() {
    // MENU_IGPU=1: 트레이 앱처럼 시작할 때 이 프로그램에 절전 GPU를 정합니다.
    if std::env::var("MENU_IGPU").is_ok() {
        let exe = std::env::current_exe().unwrap().display().to_string();
        let key = windows_registry::CURRENT_USER
            .create(r"Software\Microsoft\DirectX\UserGpuPreferences")
            .unwrap();
        key.set_string(&exe, "GpuPreference=1;").unwrap();
    }
    // SAFETY: 창이 생기기 전.
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let menu = Menu::start(move |key| {
        let _ = tx.send(key);
    })
    .expect("WinUI");
    // SAFETY: 평범한 치수 조회.
    let (w, h) = unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    };
    let people = vec![
        Entry::radio("rule:0:default", "묻기", true),
        Entry::radio("rule:0:accept", "항상 수락", false),
        Entry::radio("rule:0:block", "차단", false),
        Entry::separator(),
        Entry::item("forget:0", "목록에서 지우기").glyph('\u{E74D}'),
    ];
    let entries = vec![
        Entry::label("준비됨 · MY-PC").glyph('\u{E701}').handle(),
        Entry::separator(),
        // 제목 (아이콘 없는 레이블)은 아이콘이 시작하는 곳에서 시작합니다.
        Entry::label("로그인되어 있지 않습니다"),
        Entry::item("signin", "Apple ID로 로그인…").glyph('\u{E710}'),
        Entry::separator(),
        Entry::sub(
            "파일 보내기",
            vec![
                Entry::item("send:0", "iPhone").glyph('\u{E8EA}'),
                Entry::item("send:1", "iPad").glyph('\u{E70A}'),
            ],
        )
        .glyph('\u{E724}'),
        Entry::item("open-dir", "받은 파일 폴더 열기").glyph('\u{E838}'),
        Entry::separator(),
        Entry::toggle("receive", "AirDrop 받기", true).glyph('\u{E896}'),
        Entry::sub(
            "받기 허용",
            vec![
                Entry::radio("unknown:ask", "처음 보는 기기는 묻기", true),
                Entry::radio("unknown:decline", "처음 보는 기기는 거절", false),
                Entry::separator(),
                Entry::sub("iPhone (iPhone 16 Pro)", people).glyph('\u{E77B}'),
            ],
        )
        .glyph('\u{E8D7}'),
        Entry::separator(),
        Entry::toggle("autostart", "Windows 시작 시 실행", true).glyph('\u{E7E8}'),
        Entry::item("exit", "끝내기").glyph('\u{E711}'),
    ];
    // demo [x y]: 열 자리(물리 픽셀).
    let at: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (x, y) = match at[..] {
        [x, y] => (x, y),
        _ => (w - 160, h - 24),
    };
    // 트레이 앱에서처럼: 첫 메뉴가 옮길 때 아일랜드는 이미 오래전에 떠 있습니다.
    std::thread::sleep(Duration::from_millis(1500));
    // MENU_SHARE=1: 점을 가운데로 열고 기기가 오가는 탐색기의 기기 메뉴. (알려진 문제: 다른 GPU가
    // 맡은 화면에서는 바뀐 줄이 뒤섞여 나옵니다.)
    if std::env::var("MENU_SHARE").is_ok() {
        let searching = vec![
            Entry::label("KakaoTalk_20260718_231908671.png").glyph('\u{E724}').handle(),
            Entry::separator(),
            Entry::label("주변 기기를 찾는 중…").glyph('\u{E721}'),
        ];
        let found = vec![
            Entry::label("KakaoTalk_20260718_231908671.png").glyph('\u{E724}').handle(),
            Entry::separator(),
            Entry::item("share:0", "홍길동의 iPhone").glyph('\u{E8EA}'),
            Entry::item("share:1", "iPad").glyph('\u{E70A}'),
        ];
        menu.show_centred(x, y, searching.clone(), 7);
        for entries in [found, searching] {
            std::thread::sleep(Duration::from_millis(2500));
            menu.update(7, entries);
        }
        std::thread::sleep(Duration::from_millis(3000));
        return;
    }
    menu.show(x, y, entries);
    match rx.recv_timeout(Duration::from_secs(25)) {
        Ok(key) => println!("picked {key}"),
        Err(_) => println!("nothing picked"),
    }
}
