//! WirDrop 없이 섬의 카드를 보여 줍니다: cargo run -p wirdrop-island --example demo
//! [story|ask|open|small|done|failed|info|spin] [picture]
//! ISLAND_AT=top-left|top|top-right|left|center|right|bottom-left|bottom|bottom-right로 다른 자리에
//! 띄웁니다.
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use wirdrop_island::{Answer, Button, Card, Field, Glyph, Island, Kind, Position, Progress, Tint};

fn ask(picture: Option<PathBuf>) -> Card {
    Card {
        tag: "recv-1".into(),
        kind: Kind::Ask,
        glyph: Some(Glyph::Phone),
        picture,
        title: "홍길동의 iPhone".into(),
        detail: "사진 3장을 보내려고 합니다.".into(),
        status: "IMG_4021.HEIC 외 2개 · 12.4 MB".into(),
        brief: "홍길동의 iPhone".into(),
        buttons: vec![
            Button::new("거절", "decline:1", false),
            Button::new("항상 수락", "always:1", false),
            Button::new("수락", "accept:1", true),
        ],
        ..Card::default()
    }
}

fn receiving(picture: Option<PathBuf>, done: f32) -> Card {
    Card {
        tag: "recv-1".into(),
        kind: Kind::Transfer,
        glyph: Some(Glyph::Receive),
        picture,
        title: "홍길동의 iPhone에서 받는 중".into(),
        detail: "사진 3장 · IMG_4021.HEIC 외 2개".into(),
        brief: "받는 중".into(),
        progress: Some(progress(done)),
        launch: Some("recvdir:1".into()),
        ..Card::default()
    }
}

fn progress(done: f32) -> Progress {
    let total = 12.4;
    Progress {
        fraction: Some(done),
        amount: format!("{:.1} MB / {total} MB", total * done),
        status: if done < 1.0 {
            format!("21.3 MB/s · {}초 남음", ((1.0 - done) * 6.0).ceil().max(1.0))
        } else {
            "21.3 MB/s".into()
        },
    }
}

fn done(picture: Option<PathBuf>) -> Card {
    Card {
        tag: "recv-1".into(),
        kind: Kind::Result,
        glyph: Some(Glyph::Done),
        tint: Tint::Green,
        picture,
        title: "홍길동의 iPhone에서 받았습니다".into(),
        detail: "IMG_4021.HEIC 외 2개".into(),
        status: "12.4 MB · 0.6초 · 평균 20.7 MB/s".into(),
        brief: "받았습니다".into(),
        buttons: vec![Button::new("열기", "open:C:\\", true), Button::new("폴더 열기", "reveal:C:\\", false)],
        launch: Some("reveal:C:\\".into()),
        ..Card::default()
    }
}

fn failed() -> Card {
    Card {
        tag: "send-2".into(),
        kind: Kind::Result,
        glyph: Some(Glyph::Failed),
        tint: Tint::Red,
        title: "iPad에 보내지 못했습니다".into(),
        detail: "보고서.pdf".into(),
        status: "연결이 끊겼습니다.".into(),
        brief: "보내지 못함".into(),
        ..Card::default()
    }
}

fn info() -> Card {
    Card {
        tag: "apple-id".into(),
        kind: Kind::Info,
        glyph: Some(Glyph::Lock),
        title: "Apple ID에 다시 로그인해 주세요".into(),
        detail: "kim@example.com의 인증을 갱신하지 못했습니다. 다시 로그인하면 연락처에 있는 사람에게도 이 PC가 보입니다.".into(),
        buttons: vec![Button::new("로그인", "signin:", true)],
        ..Card::default()
    }
}

fn sending(state: &str) -> Card {
    Card {
        tag: "send-2".into(),
        kind: Kind::Transfer,
        glyph: Some(Glyph::Send),
        title: "iPad에 보내는 중".into(),
        detail: "보고서.pdf".into(),
        brief: state.into(),
        progress: Some(Progress {
            fraction: None,
            amount: String::new(),
            status: format!("{state}…"),
        }),
        ..Card::default()
    }
}

/// ISLAND_BACKDROP=x,y,w,h (픽셀): 섬 아래 그 자리에 포커스를 받지 않는 밝은 창을 깔아 섬의 윤곽을
/// 봅니다.
fn backdrop() {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;
    let Some(rect) = std::env::var("ISLAND_BACKDROP").ok() else { return };
    let v: Vec<i32> = rect.split(',').filter_map(|n| n.trim().parse().ok()).collect();
    if v.len() != 4 {
        return;
    }
    extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }
    std::thread::spawn(move || unsafe {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            lpszClassName: w!("IslandBackdrop"),
            hbrBackground: HBRUSH(CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00F2F2F2)).0),
            ..Default::default()
        };
        let atom = RegisterClassW(&class);
        let ex = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let made = CreateWindowExW(
            ex,
            w!("IslandBackdrop"),
            w!(""),
            WS_POPUP | WS_VISIBLE,
            v[0],
            v[1],
            v[2],
            v[3],
            None,
            None,
            None,
            None,
        );
        tracing::info!("backdrop: class {atom}, window {made:?}");
        let Ok(hwnd) = made else { return };
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), v[0], v[1], v[2], v[3], SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    });
    std::thread::sleep(Duration::from_millis(300));
}

fn main() {
    tracing_subscriber::fmt().with_env_filter("debug").init();
    tracing::info!("backdrop: {:?}", std::env::var("ISLAND_BACKDROP"));
    backdrop();
    let mut args = std::env::args().skip(1);
    let what = args.next().unwrap_or_else(|| "story".into());
    let picture = args.next().map(PathBuf::from);
    let (tx, rx) = channel();
    let island = Island::start(move |a| {
        println!("action: {a}");
        let _ = tx.send(a);
    })
    .expect("island");
    if let Some(at) = std::env::var("ISLAND_AT").ok().and_then(|k| Position::from_key(&k)) {
        island.place(at);
    }
    let pause = |s: f32| std::thread::sleep(Duration::from_secs_f32(s));
    match what.as_str() {
        "ask" => island.show(ask(picture)),
        "open" => island.show(receiving(picture, 0.42)),
        "small" => {
            island.show(receiving(picture, 0.42));
            pause(4.5);
            island.progress("recv-1", progress(0.63));
        }
        "done" => island.show(done(picture)),
        "failed" => island.show(failed()),
        "info" => island.show(info()),
        "spin" => island.show(sending("기기를 찾는 중")),
        "login" => {
            login(&island);
            pause(8.0);
            return;
        }
        "gallery" => {
            gallery(&island, picture);
            return;
        }
        // 결과가 떠 있고 1.5초 뒤 새 전송: 곧바로 섬을 차지합니다.
        "chain" => {
            island.show(done(picture.clone()));
            pause(1.5);
            island.show(Card {
                tag: "recv-2".into(),
                ..receiving(picture, 0.1)
            });
            for i in 2..=12 {
                pause(0.5);
                island.progress("recv-2", progress(i as f32 / 12.0));
            }
            pause(3.0);
            return;
        }
        _ => {
            island.show(ask(picture.clone()));
            let answer = rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| "accept:1".into());
            if answer.starts_with("decline") {
                island.remove("recv-1");
            } else {
                island.show(receiving(picture.clone(), 0.0));
                for i in 1..=24 {
                    pause(0.5);
                    island.progress("recv-1", progress(i as f32 / 24.0));
                }
                island.show(done(picture));
            }
            pause(7.0);
            island.show(sending("기기를 찾는 중"));
            pause(2.5);
            island.show(sending("수락을 기다리는 중"));
            pause(2.5);
            island.show(failed());
        }
    }
    pause(if what == "story" { 8.0 } else { 600.0 });
}

/// WirDrop이 섬에 띄우는 모든 카드를 그 문구 그대로 하나씩: 올라와 자리를 잡으면 스크린숏용으로
/// "READY n ask|card"를 찍습니다. 작은 것은 작아진 뒤에.
fn gallery(island: &Island, picture: Option<PathBuf>) {
    let handle = island.handle();
    let photo = picture;
    let card = |kind: Kind, glyph: Glyph, tint: Tint, title: &str, detail: &str, status: &str, brief: &str| Card {
        kind,
        glyph: Some(glyph),
        tint,
        title: title.into(),
        detail: detail.into(),
        status: status.into(),
        brief: brief.into(),
        ..Card::default()
    };
    let buttons = |labels: &[&str]| {
        labels
            .iter()
            .enumerate()
            .map(|(i, l)| Button::new(l, &format!("b{i}"), i == 0))
            .collect::<Vec<_>>()
    };
    let going = |f: Option<f32>, amount: &str, status: &str| {
        Some(Progress {
            fraction: f,
            amount: amount.into(),
            status: status.into(),
        })
    };
    let field = |placeholder: &str, value: &str, secret: bool, action: &str| {
        Some(Field {
            placeholder: placeholder.into(),
            value: value.into(),
            secret,
            action: action.into(),
        })
    };
    let ask_buttons = vec![
        Button::new("거절", "decline", false),
        Button::new("항상 수락", "always", false),
        Button::new("수락", "accept", true),
    ];
    let (to_ipad, me) = ("홍길동의 iPad", "user@example.com");
    let sending = format!("{to_ipad}에 보내는 중");
    // (이름, 카드, small: 작아진 뒤, ask: 입력칸)
    let shots: Vec<(&str, Card, bool, bool)> = vec![
        (
            "받기 요청",
            Card {
                picture: photo.clone(),
                buttons: ask_buttons.clone(),
                ..card(
                    Kind::Ask,
                    Glyph::Phone,
                    Tint::Blue,
                    "홍길동의 iPhone",
                    "사진 3장을 보내려고 합니다.",
                    "IMG_4021.HEIC 외 2개 · 12.4 MB",
                    "홍길동의 iPhone",
                )
            },
            false,
            false,
        ),
        (
            "받기 요청 (미리보기 없음)",
            Card {
                buttons: ask_buttons,
                ..card(
                    Kind::Ask,
                    Glyph::Phone,
                    Tint::Blue,
                    "홍길동의 iPhone",
                    "파일 1개를 보내려고 합니다.",
                    "보고서.pdf · 2.1 MB",
                    "홍길동의 iPhone",
                )
            },
            false,
            false,
        ),
        (
            "받는 중",
            Card {
                picture: photo.clone(),
                progress: going(Some(0.42), "5.2 MB / 12.4 MB", "21.3 MB/s · 1초 남음"),
                cancel: Some("stop".into()),
                ..card(
                    Kind::Transfer,
                    Glyph::Receive,
                    Tint::Blue,
                    "홍길동의 iPhone에서 받는 중",
                    "사진 3장 · IMG_4021.HEIC 외 2개",
                    "",
                    "받는 중",
                )
            },
            false,
            false,
        ),
        (
            "받는 중 (작은 섬)",
            Card {
                picture: photo.clone(),
                progress: going(Some(0.76), "9.4 MB / 12.4 MB", "21.3 MB/s · 1초 남음"),
                cancel: Some("stop".into()),
                ..card(
                    Kind::Transfer,
                    Glyph::Receive,
                    Tint::Blue,
                    "홍길동의 iPhone에서 받는 중",
                    "사진 3장 · IMG_4021.HEIC 외 2개",
                    "",
                    "받는 중",
                )
            },
            true,
            false,
        ),
        (
            "받았습니다",
            Card {
                picture: photo.clone(),
                buttons: buttons(&["열기", "폴더 열기"]),
                ..card(
                    Kind::Result,
                    Glyph::Done,
                    Tint::Green,
                    "홍길동의 iPhone에서 받았습니다",
                    "IMG_4021.HEIC 외 2개",
                    "12.4 MB · 0.6초 · 평균 20.7 MB/s",
                    "받았습니다",
                )
            },
            false,
            false,
        ),
        (
            "받지 못했습니다",
            card(
                Kind::Result,
                Glyph::Failed,
                Tint::Red,
                "홍길동의 iPhone에서 받지 못했습니다",
                "사진 3장",
                "연결이 끊겼습니다.",
                "받지 못했습니다",
            ),
            false,
            false,
        ),
        (
            "링크를 받았습니다",
            Card {
                buttons: buttons(&["열기"]),
                ..card(
                    Kind::Result,
                    Glyph::Link,
                    Tint::Blue,
                    "홍길동의 iPhone에서 링크를 받았습니다",
                    "https://www.apple.com/kr/",
                    "",
                    "링크",
                )
            },
            false,
            false,
        ),
        (
            "보내기: 준비 중",
            Card {
                progress: going(None, "", "USB 어댑터를 준비하는 중…"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "보고서.pdf", "", "준비 중")
            },
            false,
            false,
        ),
        (
            "보내기: 기기를 찾는 중",
            Card {
                progress: going(None, "", "기기를 찾는 중…"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "보고서.pdf", "", "기기를 찾는 중")
            },
            false,
            false,
        ),
        (
            "보내기: 기기를 찾는 중 (작은 섬)",
            Card {
                progress: going(None, "", "기기를 찾는 중…"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "보고서.pdf", "", "기기를 찾는 중")
            },
            true,
            false,
        ),
        (
            "보내기: 수락 대기 중",
            Card {
                progress: going(None, "", "수락을 기다리는 중…"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "보고서.pdf", "", "수락 대기 중")
            },
            false,
            false,
        ),
        (
            "보내는 중",
            Card {
                picture: photo.clone(),
                progress: going(Some(0.63), "7.8 MB / 12.4 MB", "18.2 MB/s · 1초 남음"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "IMG_4021.HEIC 외 2개", "", "보내는 중")
            },
            false,
            false,
        ),
        (
            "보내는 중 (작은 섬)",
            Card {
                picture: photo.clone(),
                progress: going(Some(0.81), "10 MB / 12.4 MB", "18.2 MB/s · 1초 남음"),
                cancel: Some("stop".into()),
                ..card(Kind::Transfer, Glyph::Send, Tint::Blue, &sending, "IMG_4021.HEIC 외 2개", "", "보내는 중")
            },
            true,
            false,
        ),
        (
            "보냈습니다",
            Card {
                picture: photo.clone(),
                ..card(
                    Kind::Result,
                    Glyph::Done,
                    Tint::Green,
                    &format!("{to_ipad}에 보냈습니다"),
                    "IMG_4021.HEIC 외 2개",
                    "12.4 MB · 0.8초 · 평균 15.5 MB/s",
                    "보냈습니다",
                )
            },
            false,
            false,
        ),
        (
            "거절했습니다",
            card(
                Kind::Result,
                Glyph::Failed,
                Tint::Orange,
                &format!("{to_ipad}에서 거절했습니다"),
                "보고서.pdf",
                "",
                "거절됨",
            ),
            false,
            false,
        ),
        (
            "보내지 못했습니다",
            card(
                Kind::Result,
                Glyph::Failed,
                Tint::Red,
                &format!("{to_ipad}에 보내지 못했습니다"),
                "보고서.pdf",
                "상대가 응답하지 않았습니다.",
                "보내지 못했습니다",
            ),
            false,
            false,
        ),
        (
            "로그인: Apple ID",
            Card {
                field: field("Apple ID (이메일 또는 전화번호)", "", false, "다음"),
                ..card(
                    Kind::Ask,
                    Glyph::Lock,
                    Tint::Blue,
                    "Apple ID 로그인",
                    "암호는 이 PC에 저장되지 않습니다",
                    "",
                    "",
                )
            },
            false,
            true,
        ),
        (
            "로그인: 암호",
            Card {
                field: field("암호", "hunter22", true, "로그인"),
                ..card(Kind::Ask, Glyph::Lock, Tint::Blue, "Apple ID 암호", me, "", "")
            },
            false,
            true,
        ),
        (
            "로그인: 확인 코드",
            Card {
                field: field("6자리 코드", "1234", false, "확인"),
                ..card(Kind::Ask, Glyph::Lock, Tint::Blue, "확인 코드", "Apple 기기에 표시된 6자리 코드", "", "")
            },
            false,
            true,
        ),
        (
            "로그인 중",
            Card {
                progress: going(None, "", "Apple이 인증서를 발급하는 중… (1~2분)"),
                ..card(Kind::Transfer, Glyph::Lock, Tint::Blue, "Apple ID 로그인 중", "", "", "로그인 중")
            },
            false,
            false,
        ),
        (
            "로그인 중 (작은 섬)",
            Card {
                progress: going(None, "", "Apple이 인증서를 발급하는 중… (1~2분)"),
                ..card(Kind::Transfer, Glyph::Lock, Tint::Blue, "Apple ID 로그인 중", "", "", "로그인 중")
            },
            true,
            false,
        ),
        (
            "로그인 성공",
            card(
                Kind::Result,
                Glyph::Done,
                Tint::Green,
                "Apple ID에 로그인했습니다",
                me,
                "",
                "Apple ID에 로그인했습니다",
            ),
            false,
            false,
        ),
        (
            "로그인 실패",
            Card {
                buttons: buttons(&["다시 로그인"]),
                ..card(
                    Kind::Result,
                    Glyph::Failed,
                    Tint::Red,
                    "Apple ID에 로그인하지 못했습니다",
                    "Apple 서버에 연결하지 못했습니다. 인터넷 연결을 확인하고 잠시 뒤 다시 시도해 주세요.",
                    "",
                    "Apple ID에 로그인하지 못했습니다",
                )
            },
            false,
            false,
        ),
        (
            "인증서 받음",
            card(
                Kind::Result,
                Glyph::Done,
                Tint::Green,
                "AirDrop 인증서를 받았습니다",
                me,
                "",
                "AirDrop 인증서를 받았습니다",
            ),
            false,
            false,
        ),
        (
            "USB 어댑터 없음",
            card(
                Kind::Info,
                Glyph::NoWifi,
                Tint::Blue,
                "USB 어댑터를 꽂아 주세요",
                "TP-Link Archer T3U Nano를 꽂으면 바로 시작합니다.",
                "",
                "USB 어댑터를 꽂아 주세요",
            ),
            false,
            false,
        ),
        (
            "관리자 권한 필요",
            Card {
                buttons: buttons(&["다시 시도"]),
                ..card(
                    Kind::Info,
                    Glyph::Info,
                    Tint::Blue,
                    "관리자 권한이 필요합니다",
                    "AirDrop 엔진을 설치하거나 업데이트하려면 허용해 주세요.",
                    "",
                    "관리자 권한이 필요합니다",
                )
            },
            false,
            false,
        ),
        (
            "로그아웃 확인",
            Card {
                buttons: buttons(&["로그아웃", "취소"]),
                ..card(
                    Kind::Info,
                    Glyph::Lock,
                    Tint::Blue,
                    &format!("{me}에서 로그아웃할까요?"),
                    "이 PC의 AirDrop 인증서와 로그인 정보를 지웁니다.",
                    "",
                    "로그아웃할까요?",
                )
            },
            false,
            false,
        ),
        (
            "다시 로그인 필요",
            Card {
                buttons: buttons(&["로그인"]),
                ..card(
                    Kind::Info,
                    Glyph::Lock,
                    Tint::Blue,
                    "Apple ID에 다시 로그인해 주세요",
                    &format!("{me}의 인증을 갱신하지 못했습니다."),
                    "",
                    "다시 로그인해 주세요",
                )
            },
            false,
            false,
        ),
        (
            "이미 로그인하는 중",
            card(
                Kind::Info,
                Glyph::Lock,
                Tint::Blue,
                "이미 로그인하는 중입니다",
                "지금 하는 로그인이 끝나면 다시 해 주세요.",
                "",
                "이미 로그인하는 중입니다",
            ),
            false,
            false,
        ),
        (
            "보낼 파일 없음",
            card(
                Kind::Info,
                Glyph::Doc,
                Tint::Blue,
                "보낼 파일이 없습니다",
                "보낼 수 있는 파일이나 폴더가 없습니다.",
                "",
                "보낼 파일이 없습니다",
            ),
            false,
            false,
        ),
        (
            "알림 위치 미리보기",
            card(
                Kind::Info,
                Glyph::Info,
                Tint::Blue,
                "알림 위치",
                "알림이 여기에 나타납니다.",
                "",
                "알림 위치",
            ),
            false,
            false,
        ),
    ];
    for (i, (name, card, small, ask)) in shots.into_iter().enumerate() {
        let tag = format!("g{i}");
        let card = Card { tag: tag.clone(), ..card };
        let _answer = if ask {
            Some(handle.ask(card))
        } else {
            island.show(card);
            None
        };
        std::thread::sleep(Duration::from_secs_f32(if small { 5.0 } else { 1.4 }));
        println!("READY {i} {} {name}", if ask { "ask" } else { "card" });
        std::thread::sleep(Duration::from_millis(700));
        island.remove(&tag);
    }
    std::thread::sleep(Duration::from_secs(1));
}

/// WirDrop의 로그인 순서대로: Apple ID, 암호, 진행, 코드, 끝. 입력한 것은 길이만 찍습니다.
fn login(island: &Island) {
    let handle = island.handle();
    let ask = |title: &str, detail: &str, placeholder: &str, secret: bool, action: &str| {
        let answer = handle.ask(Card {
            tag: "signin".into(),
            kind: Kind::Ask,
            glyph: Some(Glyph::Lock),
            title: title.into(),
            detail: detail.into(),
            field: Some(Field {
                placeholder: placeholder.into(),
                secret,
                action: action.into(),
                ..Field::default()
            }),
            ..Card::default()
        });
        let got = answer.recv().unwrap_or(Answer::Closed);
        println!(
            "{title}: {}",
            match &got {
                Answer::Typed(t) => format!("typed {} characters", t.chars().count()),
                Answer::Closed => "closed".into(),
            }
        );
        got
    };
    let progress = |status: &str| Card {
        tag: "signin".into(),
        kind: Kind::Transfer,
        glyph: Some(Glyph::Lock),
        title: "Apple ID 로그인 중".into(),
        brief: "로그인 중".into(),
        progress: Some(Progress {
            fraction: None,
            amount: String::new(),
            status: status.into(),
        }),
        ..Card::default()
    };
    if ask("Apple ID 로그인", "암호는 이 PC에 저장되지 않습니다", "Apple ID", false, "다음") == Answer::Closed {
        return island.remove("signin");
    }
    if ask("Apple ID 로그인", "Apple ID 암호를 입력하세요", "암호", true, "로그인") == Answer::Closed {
        return island.remove("signin");
    }
    island.show(progress("로그인하는 중…"));
    std::thread::sleep(Duration::from_secs(2));
    if ask("확인 코드", "Apple 기기에 표시된 6자리 코드", "000000", false, "확인") == Answer::Closed {
        return island.remove("signin");
    }
    island.show(progress("Apple이 인증서를 발급하는 중…"));
    std::thread::sleep(Duration::from_secs(3));
    island.show(Card {
        tag: "signin".into(),
        kind: Kind::Result,
        glyph: Some(Glyph::Done),
        tint: Tint::Green,
        title: "Apple ID에 로그인했습니다".into(),
        detail: "user@example.com".into(),
        brief: "로그인했습니다".into(),
        ..Card::default()
    });
}
