//! 트레이 앱의 메뉴를 견본 상태로 열고 고른 것을 찍습니다: cargo run -p wirdrop-menu --example demo [x y]
//!
//! MENU_SHARE=1이면 탐색기에서 보낼 때 뜨는 기기 메뉴를, MENU_RENAME=1이면 이름 입력칸을 엽니다.
//! WIRDROP_LANG=ko|en으로 문구의 언어를 정하고, MENU_IGPU=1이면 트레이 앱처럼 절전 GPU를 씁니다.
//! 문구와 아이콘은 WirDrop(app/src/tray/menu.rs)의 것과 같고, 이름과 경로는 견본입니다.
use std::time::Duration;

use wirdrop_menu::{Entry, Menu};

// Segoe Fluent Icons.
const READY: char = '\u{E701}';
const SEND: char = '\u{E724}';
const PHONE: char = '\u{E8EA}';
const TABLET: char = '\u{E70A}';
const SEARCH: char = '\u{E721}';
const FOLDER: char = '\u{E838}';
const PEOPLE: char = '\u{E716}';
const PERSON: char = '\u{E77B}';
const PC: char = '\u{E977}';
const RENAME: char = '\u{E8AC}';
const DELETE: char = '\u{E74D}';
const ACCOUNT: char = '\u{E910}';
const ADD: char = '\u{E710}';
const SIGN_OUT: char = '\u{F3B1}';
const BELL: char = '\u{EA8F}';
const POWER: char = '\u{E7E8}';
const RECEIVE: char = '\u{E896}';
const SAVE: char = '\u{E74E}';

/// 지금 언어의 문구 (한국어, 영어 순).
fn t(ko: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { ko }
}

/// WIRDROP_LANG이 ko로 시작하지 않으면 영어 (견본이라 Windows 언어는 보지 않습니다).
fn english() -> bool {
    !std::env::var("WIRDROP_LANG").unwrap_or_default().to_ascii_lowercase().starts_with("ko")
}

/// 트레이 아이콘의 메뉴: 받기, 폴더와 저장 위치, 이 PC가 보이는 방식, Apple ID, 알림 위치, 부팅 시 켜기.
fn tray() -> Vec<Entry> {
    let rule = |key: &str, text: &str, on: bool| Entry::radio(format!("rule:0:{key}"), text, on);
    let person = vec![
        rule("default", t("매번 묻기", "Always ask"), false),
        rule("accept", t("항상 수락", "Always accept"), true),
        rule("block", t("차단", "Block"), false),
        Entry::separator(),
        Entry::item("forget:0", t("목록에서 지우기", "Remove from list")).glyph(DELETE),
    ];
    let receive = vec![
        Entry::radio("receive:off", t("끔", "Off"), false),
        Entry::radio("receive:contacts", t("연락처만", "Contacts only"), false),
        Entry::radio("receive:everyone", t("모든 사람", "Everyone"), true),
        Entry::separator(),
        Entry::toggle("clipboard", t("클립보드에 복사", "Copy to clipboard"), true),
        Entry::sub(
            t("기억한 기기", "Known devices"),
            vec![Entry::sub(t("홍길동의 iPhone · iPhone 16 Pro", "John's iPhone · iPhone 16 Pro"), person).glyph(PERSON)],
        )
        .glyph(PEOPLE),
    ];
    let models = ["iMac", "MacBook Pro", "MacBook Air", "Mac mini", "Mac Studio", "Mac Pro"]
        .into_iter()
        .map(|m| Entry::radio(format!("model:{m}"), m, m == "MacBook Pro"))
        .collect();
    let apple_id = vec![
        Entry::sub(
            "user@example.com",
            vec![
                Entry::item("signin:0", t("다시 로그인…", "Sign in again…")).glyph(ACCOUNT),
                Entry::item("signout:0", t("로그아웃…", "Sign out…")).glyph(SIGN_OUT),
            ],
        )
        .glyph(PERSON),
        Entry::separator(),
        Entry::item("signin:new", t("다른 Apple ID 추가…", "Add another Apple ID…")).glyph(ADD),
    ];
    let spots = [
        t("왼쪽 위", "Top left"),
        t("위", "Top"),
        t("오른쪽 위", "Top right"),
        t("왼쪽", "Left"),
        t("가운데", "Center"),
        t("오른쪽", "Right"),
        t("왼쪽 아래", "Bottom left"),
        t("아래", "Bottom"),
        t("오른쪽 아래", "Bottom right"),
    ];
    let mut island = Vec::new();
    for (i, spot) in spots.into_iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            island.push(Entry::separator());
        }
        island.push(Entry::radio(format!("island-at:{i}"), spot, i == 1));
    }
    // 한국어 Windows의 WinUI는 역슬래시를 "₩"로 그립니다. 영어 견본은 영어 Windows에서처럼 보이도록 비슷한
    // 글자(⧵)로 씁니다.
    let downloads = t(r"C:\Users\me\Downloads", "C:⧵Users⧵me⧵Downloads");
    vec![
        Entry::label(t("준비됨 · MY-PC", "Ready · MY-PC")).glyph(READY).handle(),
        Entry::separator(),
        Entry::sub(t("받기", "Receive"), receive).glyph(RECEIVE),
        Entry::item("open-dir", t("폴더", "Folder")).glyph(FOLDER),
        Entry::item("save-dir", downloads).glyph(SAVE),
        Entry::separator(),
        Entry::item("rename", "MY-PC").glyph(RENAME),
        Entry::sub("MacBook Pro", models).glyph(PC),
        Entry::sub("Apple ID", apple_id).glyph(ACCOUNT),
        Entry::separator(),
        Entry::sub(t("알림 위치", "Notification position"), island).glyph(BELL),
        Entry::toggle("autostart", t("부팅 시 켜기", "Open at boot"), true),
        Entry::separator(),
        Entry::label("WirDrop 0.2.7"),
        Entry::item("exit", t("종료", "Quit")).glyph(POWER),
    ]
}

/// 탐색기에서 보낼 때의 기기 메뉴: 찾는 중, 그리고 찾은 기기들.
fn share(found: bool) -> Vec<Entry> {
    let mut items = vec![Entry::label("IMG_4021.HEIC").glyph(SEND).handle(), Entry::separator()];
    if found {
        items.push(Entry::item("share:0", t("홍길동의 iPhone", "John's iPhone")).glyph(PHONE));
        items.push(Entry::item("share:1", t("홍길동의 iPad", "John's iPad")).glyph(TABLET));
    } else {
        items.push(Entry::label(t("주변 기기를 찾는 중…", "Looking for nearby devices…")).glyph(SEARCH));
    }
    items
}

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
    // demo [x y]: 열 자리(물리 픽셀).
    let at: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (x, y) = match at[..] {
        [x, y] => (x, y),
        _ => (w - 160, h - 24),
    };
    // 트레이 앱에서처럼: 첫 메뉴가 열릴 때 아일랜드는 이미 오래전에 떠 있습니다.
    std::thread::sleep(Duration::from_millis(1500));
    if std::env::var("MENU_SHARE").is_ok() {
        // 점을 가운데로 열고, 기기가 오가는 대로 바뀝니다.
        menu.show_centred(x, y, share(false), 7);
        for found in [true, false] {
            std::thread::sleep(Duration::from_millis(2500));
            menu.update(7, share(found));
        }
        std::thread::sleep(Duration::from_millis(3000));
        return;
    }
    if std::env::var("MENU_RENAME").is_ok() {
        let hint = t("Enter: 저장 · Esc: 취소", "Enter: Save · Esc: Cancel");
        let (done, typed) = std::sync::mpsc::channel();
        menu.ask_text(x, y, t("AirDrop 이름 설정", "AirDrop name"), "MY-PC", hint, move |name| {
            let _ = done.send(name);
        });
        match typed.recv_timeout(Duration::from_secs(25)) {
            Ok(name) => println!("typed {name}"),
            Err(_) => println!("nothing typed"),
        }
        return;
    }
    menu.show(x, y, tray());
    match rx.recv_timeout(Duration::from_secs(25)) {
        Ok(key) => println!("picked {key}"),
        Err(_) => println!("nothing picked"),
    }
}
