//! WirDrop 없이 섬의 카드를 보여 줍니다: cargo run -p wirdrop-island --example demo
//! [story|ask|open|small|done|failed|info|spin|login|gallery|chain] [picture]
//!
//! ISLAND_AT=top-left|top|top-right|left|center|right|bottom-left|bottom|bottom-right로 다른 자리에
//! 띄우고, WIRDROP_LANG=ko|en으로 문구의 언어를 정합니다. 문구는 WirDrop의 것과 같고, 사람과
//! 파일은 모두 견본입니다.
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use wirdrop_island::{Answer, Button, Card, Field, Glyph, Island, Kind, Position, Progress, Tint};

/// 지금 언어의 문구 (한국어, 영어 순).
fn t(ko: &'static str, en: &'static str) -> &'static str {
    if wirdrop_island::english() { en } else { ko }
}

/// 지금 언어의 format! (한국어, 영어 순).
macro_rules! tf {
    ($ko:literal, $en:literal) => {
        if wirdrop_island::english() { format!($en) } else { format!($ko) }
    };
}

const ME: &str = "user@example.com";

fn phone() -> &'static str {
    t("홍길동의 iPhone", "John's iPhone")
}

fn tablet() -> &'static str {
    t("홍길동의 iPad", "John's iPad")
}

fn photos() -> String {
    let names = t("IMG_4021.HEIC 외 2개", "IMG_4021.HEIC and 2 more");
    format!("{} · {names}", t("사진 3장", "3 photos"))
}

fn report() -> &'static str {
    t("보고서.pdf", "Report.pdf")
}

fn buttons(labels: &[&str]) -> Vec<Button> {
    labels.iter().enumerate().map(|(i, l)| Button::new(l, &format!("b{i}"), i == 0)).collect()
}

fn ask(picture: Option<PathBuf>) -> Card {
    Card {
        tag: "recv-1".into(),
        kind: Kind::Ask,
        glyph: Some(Glyph::Phone),
        picture,
        title: phone().into(),
        detail: t("사진 3장을 보내려고 합니다.", "Wants to share 3 photos.").into(),
        status: t("IMG_4021.HEIC 외 2개 · 12.4 MB", "IMG_4021.HEIC and 2 more · 12.4 MB").into(),
        brief: phone().into(),
        buttons: vec![
            Button::new(t("거절", "Decline"), "decline:1", false),
            Button::new(t("항상 수락", "Always Accept"), "always:1", false),
            Button::new(t("수락", "Accept"), "accept:1", true),
        ],
        ..Card::default()
    }
}

/// 받기 진행 (done: 0~1).
fn progress(done: f32, rate: &str) -> Progress {
    let total = 12.4;
    let left = ((1.0 - done) * 6.0).ceil().max(1.0);
    Progress {
        fraction: Some(done),
        amount: format!("{:.1} MB / {total} MB", total * done),
        status: if done < 1.0 {
            tf!("{rate}/s · {left}초 남음", "{rate}/s · {left} s left")
        } else {
            format!("{rate}/s")
        },
    }
}

fn receiving(picture: Option<PathBuf>, done: f32) -> Card {
    let from = phone();
    Card {
        tag: "recv-1".into(),
        kind: Kind::Transfer,
        glyph: Some(Glyph::Receive),
        picture,
        title: tf!("{from}에서 받는 중", "Receiving from {from}"),
        detail: photos(),
        brief: t("받는 중", "Receiving").into(),
        progress: Some(progress(done, "21.3 MB")),
        launch: Some("recvdir:1".into()),
        cancel: Some("stop:1".into()),
        ..Card::default()
    }
}

fn received(picture: Option<PathBuf>) -> Card {
    let from = phone();
    Card {
        tag: "recv-1".into(),
        kind: Kind::Result,
        glyph: Some(Glyph::Done),
        tint: Tint::Green,
        picture,
        title: tf!("{from}에서 받았습니다", "Received from {from}"),
        detail: t("IMG_4021.HEIC 외 2개", "IMG_4021.HEIC and 2 more").into(),
        status: t("12.4 MB · 0.6초 · 평균 20.7 MB/s", "12.4 MB · 0.6 s · avg 20.7 MB/s").into(),
        brief: t("받았습니다", "Received").into(),
        buttons: vec![
            Button::new(t("열기", "Open"), "open:C:\\", true),
            Button::new(t("폴더 열기", "Show in Folder"), "reveal:C:\\", false),
        ],
        launch: Some("reveal:C:\\".into()),
        ..Card::default()
    }
}

/// 보내기 카드: picture와 진행이 있으면 보내는 중, 없으면 brief와 status의 도는 링.
fn sending(picture: Option<PathBuf>, brief: &str, status: Progress) -> Card {
    let to = tablet();
    let detail = if picture.is_some() {
        t("IMG_4021.HEIC 외 2개", "IMG_4021.HEIC and 2 more")
    } else {
        report()
    };
    Card {
        tag: "send-2".into(),
        kind: Kind::Transfer,
        glyph: Some(Glyph::Send),
        picture,
        title: tf!("{to}에 보내는 중", "Sending to {to}"),
        detail: detail.into(),
        brief: brief.into(),
        progress: Some(status),
        cancel: Some("cancel:2".into()),
        ..Card::default()
    }
}

fn spinning(status: &str) -> Progress {
    Progress {
        fraction: None,
        amount: String::new(),
        status: status.into(),
    }
}

fn result(glyph: Glyph, tint: Tint, title: String, detail: &str, status: &str, brief: &str) -> Card {
    Card {
        kind: Kind::Result,
        glyph: Some(glyph),
        tint,
        title,
        detail: detail.into(),
        status: status.into(),
        brief: brief.into(),
        ..Card::default()
    }
}

fn send_failed() -> Card {
    let to = tablet();
    Card {
        tag: "send-2".into(),
        ..result(
            Glyph::Failed,
            Tint::Red,
            tf!("{to}에 보내지 못했습니다", "Couldn't send to {to}"),
            report(),
            t("상대가 응답하지 않았습니다.", "The other device didn't answer."),
            t("보내지 못했습니다", "Not sent"),
        )
    }
}

fn info(glyph: Glyph, title: String, detail: String, labels: &[&str]) -> Card {
    Card {
        kind: Kind::Info,
        glyph: Some(glyph),
        brief: title.clone(),
        title,
        detail,
        buttons: buttons(labels),
        ..Card::default()
    }
}

fn sign_in_again() -> Card {
    Card {
        tag: "apple-id".into(),
        ..info(
            Glyph::Lock,
            t("Apple ID에 다시 로그인해 주세요", "Sign in to Apple ID again").into(),
            tf!("{ME}의 인증을 갱신하지 못했습니다.", "Couldn't renew the sign-in for {ME}."),
            &[t("로그인", "Sign In")],
        )
    }
}

/// 로그인 카드의 입력칸.
fn field(title: &str, detail: &str, placeholder: &str, value: &str, secret: bool, action: &str) -> Card {
    Card {
        kind: Kind::Ask,
        glyph: Some(Glyph::Lock),
        title: title.into(),
        detail: detail.into(),
        field: Some(Field {
            placeholder: placeholder.into(),
            value: value.into(),
            secret,
            action: action.into(),
        }),
        ..Card::default()
    }
}

fn signing_in(status: &str) -> Card {
    Card {
        kind: Kind::Transfer,
        glyph: Some(Glyph::Lock),
        title: t("Apple ID 로그인 중", "Signing in to Apple ID").into(),
        brief: t("로그인 중", "Signing in").into(),
        progress: Some(spinning(status)),
        ..Card::default()
    }
}

/// ISLAND_BACKDROP=x,y,w,h[,rrggbb] (픽셀): 섬 아래 그 자리에 포커스를 받지 않는 창을 깔아 섬의
/// 윤곽을 봅니다. 색을 주지 않으면 밝은 회색입니다.
fn backdrop() {
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;
    let Some(rect) = std::env::var("ISLAND_BACKDROP").ok() else { return };
    let parts: Vec<&str> = rect.split(',').map(str::trim).collect();
    let v: Vec<i32> = parts.iter().take(4).filter_map(|n| n.parse().ok()).collect();
    if v.len() != 4 {
        return;
    }
    // GDI의 색은 0x00BBGGRR입니다.
    let rgb = parts.get(4).and_then(|c| u32::from_str_radix(c, 16).ok()).unwrap_or(0xF2F2F2);
    let color = COLORREF(((rgb & 0xFF) << 16) | (rgb & 0xFF00) | (rgb >> 16 & 0xFF));
    extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }
    std::thread::spawn(move || unsafe {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            lpszClassName: w!("IslandBackdrop"),
            hbrBackground: HBRUSH(CreateSolidBrush(color).0),
            ..Default::default()
        };
        RegisterClassW(&class);
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
    tracing_subscriber::fmt().with_env_filter("info").init();
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
    let looking = || {
        sending(
            None,
            t("기기를 찾는 중", "Looking"),
            spinning(t("기기를 찾는 중…", "Looking for the device…")),
        )
    };
    match what.as_str() {
        "ask" => island.show(ask(picture)),
        "open" => island.show(receiving(picture, 0.42)),
        "small" => {
            island.show(receiving(picture, 0.42));
            pause(4.5);
            island.progress("recv-1", progress(0.63, "21.3 MB"));
        }
        "done" => island.show(received(picture)),
        "failed" => island.show(send_failed()),
        "info" => island.show(sign_in_again()),
        "spin" => island.show(looking()),
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
            island.show(received(picture.clone()));
            pause(1.5);
            island.show(Card {
                tag: "recv-2".into(),
                ..receiving(picture, 0.1)
            });
            for i in 2..=12 {
                pause(0.5);
                island.progress("recv-2", progress(i as f32 / 12.0, "21.3 MB"));
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
                    island.progress("recv-1", progress(i as f32 / 24.0, "21.3 MB"));
                }
                island.show(received(picture));
            }
            pause(7.0);
            island.show(looking());
            pause(2.5);
            island.show(sending(
                None,
                t("수락 대기 중", "Waiting"),
                spinning(t("수락을 기다리는 중…", "Waiting for acceptance…")),
            ));
            pause(2.5);
            island.show(send_failed());
        }
    }
    pause(if what == "story" { 8.0 } else { 600.0 });
}

/// WirDrop이 섬에 띄우는 모든 카드를 그 문구 그대로 하나씩: 올라와 자리를 잡으면 스크린숏용으로
/// "READY n ask|card 이름"을 찍습니다. 작은 것은 작아진 뒤에.
fn gallery(island: &Island, picture: Option<PathBuf>) {
    let handle = island.handle();
    let photo = picture;
    let (from, to) = (phone(), tablet());
    let ask_file = Card {
        picture: None,
        detail: t("파일 1개를 보내려고 합니다.", "Wants to share 1 file.").into(),
        status: t("보고서.pdf · 2.1 MB", "Report.pdf · 2.1 MB").into(),
        ..ask(None)
    };
    let send_spin = |brief: &str, status: &str| sending(None, brief, spinning(status));
    let send_bar = |done: f32| sending(photo.clone(), t("보내는 중", "Sending"), progress(done, "18.2 MB"));
    let issuing = t("Apple이 인증서를 발급하는 중… (1~2분)", "Apple is issuing the certificate… (1–2 min)");
    // (이름, 카드, small: 작아진 뒤, ask: 입력칸)
    let shots: Vec<(&str, Card, bool, bool)> = vec![
        ("receive-ask", ask(photo.clone()), false, false),
        ("receive-ask-file", ask_file, false, false),
        ("receiving", receiving(photo.clone(), 0.42), false, false),
        ("receiving-small", receiving(photo.clone(), 0.76), true, false),
        ("received", received(photo.clone()), false, false),
        (
            "receive-failed",
            result(
                Glyph::Failed,
                Tint::Red,
                tf!("{from}에서 받지 못했습니다", "Couldn't receive from {from}"),
                t("사진 3장", "3 photos"),
                t("연결이 끊겼습니다.", "The connection was lost."),
                t("받지 못했습니다", "Not received"),
            ),
            false,
            false,
        ),
        (
            "link",
            Card {
                buttons: buttons(&[t("열기", "Open")]),
                ..result(
                    Glyph::Link,
                    Tint::Blue,
                    tf!("{from}에서 링크를 받았습니다", "Link from {from}"),
                    "https://www.apple.com/",
                    "",
                    t("링크", "Link"),
                )
            },
            false,
            false,
        ),
        (
            "send-preparing",
            send_spin(t("준비 중", "Preparing"), t("USB 어댑터를 준비하는 중…", "Preparing the USB adapter…")),
            false,
            false,
        ),
        (
            "send-looking",
            send_spin(t("기기를 찾는 중", "Looking"), t("기기를 찾는 중…", "Looking for the device…")),
            false,
            false,
        ),
        (
            "send-looking-small",
            send_spin(t("기기를 찾는 중", "Looking"), t("기기를 찾는 중…", "Looking for the device…")),
            true,
            false,
        ),
        (
            "send-waiting",
            send_spin(t("수락 대기 중", "Waiting"), t("수락을 기다리는 중…", "Waiting for acceptance…")),
            false,
            false,
        ),
        ("sending", send_bar(0.63), false, false),
        ("sending-small", send_bar(0.81), true, false),
        (
            "sent",
            Card {
                picture: photo.clone(),
                ..result(
                    Glyph::Done,
                    Tint::Green,
                    tf!("{to}에 보냈습니다", "Sent to {to}"),
                    t("IMG_4021.HEIC 외 2개", "IMG_4021.HEIC and 2 more"),
                    t("12.4 MB · 0.8초 · 평균 15.5 MB/s", "12.4 MB · 0.8 s · avg 15.5 MB/s"),
                    t("보냈습니다", "Sent"),
                )
            },
            false,
            false,
        ),
        (
            "declined",
            result(
                Glyph::Failed,
                Tint::Orange,
                tf!("{to}에서 거절했습니다", "{to} declined"),
                report(),
                "",
                t("거절됨", "Declined"),
            ),
            false,
            false,
        ),
        ("send-failed", send_failed(), false, false),
        (
            "signin-id",
            field(
                t("Apple ID 로그인", "Sign in to Apple ID"),
                t("암호는 이 PC에 저장되지 않습니다", "Your password isn't stored on this PC"),
                t("Apple ID (이메일 또는 전화번호)", "Apple ID (email or phone number)"),
                "",
                false,
                t("다음", "Next"),
            ),
            false,
            true,
        ),
        (
            "signin-password",
            field(
                t("Apple ID 암호", "Apple ID password"),
                ME,
                t("암호", "Password"),
                "hunter22",
                true,
                t("로그인", "Sign In"),
            ),
            false,
            true,
        ),
        (
            "signin-code",
            field(
                t("확인 코드", "Verification code"),
                t("Apple 기기에 표시된 6자리 코드", "The 6-digit code on your Apple device"),
                t("6자리 코드", "6-digit code"),
                "1234",
                false,
                t("확인", "Verify"),
            ),
            false,
            true,
        ),
        ("signing-in", signing_in(issuing), false, false),
        ("signing-in-small", signing_in(issuing), true, false),
        (
            "signed-in",
            result(
                Glyph::Done,
                Tint::Green,
                t("Apple ID에 로그인했습니다", "Signed in to Apple ID").into(),
                ME,
                "",
                t("Apple ID에 로그인했습니다", "Signed in to Apple ID"),
            ),
            false,
            false,
        ),
        (
            "signin-failed",
            Card {
                buttons: buttons(&[t("다시 로그인", "Sign In Again")]),
                ..result(
                    Glyph::Failed,
                    Tint::Red,
                    t("Apple ID에 로그인하지 못했습니다", "Couldn't sign in to Apple ID").into(),
                    t("Apple ID나 암호가 맞지 않습니다.", "The Apple ID or password is wrong."),
                    "",
                    t("Apple ID에 로그인하지 못했습니다", "Couldn't sign in to Apple ID"),
                )
            },
            false,
            false,
        ),
        (
            "certificate",
            result(
                Glyph::Done,
                Tint::Green,
                t("AirDrop 인증서를 받았습니다", "Got the AirDrop certificate").into(),
                ME,
                "",
                t("AirDrop 인증서를 받았습니다", "Got the AirDrop certificate"),
            ),
            false,
            false,
        ),
        (
            "no-adapter",
            info(
                Glyph::NoWifi,
                t("USB 어댑터를 꽂아 주세요", "Plug in the USB adapter").into(),
                t(
                    "T3U Nano나 BE6500UA를 꽂으면 바로 시작합니다.",
                    "WirDrop starts as soon as a T3U Nano or BE6500UA is in.",
                )
                .into(),
                &[],
            ),
            false,
            false,
        ),
        (
            "admin",
            info(
                Glyph::Info,
                t("관리자 권한이 필요합니다", "Administrator permission needed").into(),
                t(
                    "AirDrop 엔진을 설치하거나 업데이트하려면 허용해 주세요.",
                    "Allow it to install or update the AirDrop engine.",
                )
                .into(),
                &[t("다시 시도", "Try Again")],
            ),
            false,
            false,
        ),
        (
            "sign-out",
            info(
                Glyph::Lock,
                tf!("{ME}에서 로그아웃할까요?", "Sign out of {ME}?"),
                t(
                    "이 PC의 AirDrop 인증서와 로그인 정보를 지웁니다.",
                    "This removes the AirDrop certificate and sign-in from this PC.",
                )
                .into(),
                &[t("로그아웃", "Sign Out"), t("취소", "Cancel")],
            ),
            false,
            false,
        ),
        ("sign-in-again", sign_in_again(), false, false),
        (
            "signin-busy",
            info(
                Glyph::Lock,
                t("이미 로그인하는 중입니다", "Already signing in").into(),
                t("지금 하는 로그인이 끝나면 다시 해 주세요.", "Try again when this sign-in is done.").into(),
                &[],
            ),
            false,
            false,
        ),
        (
            "nothing-to-send",
            info(
                Glyph::Doc,
                t("보낼 파일이 없습니다", "Nothing to send").into(),
                t("보낼 수 있는 파일이나 폴더가 없습니다.", "There are no files or folders to send.").into(),
                &[],
            ),
            false,
            false,
        ),
        (
            "position",
            info(
                Glyph::Info,
                t("알림 위치", "Notification position").into(),
                t("알림이 여기에 나타납니다.", "Notifications appear here.").into(),
                &[],
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
    let ask = |card: Card| {
        let title = card.title.clone();
        let got = handle
            .ask(Card {
                tag: "signin".into(),
                ..card
            })
            .recv()
            .unwrap_or(Answer::Closed);
        println!(
            "{title}: {}",
            match &got {
                Answer::Typed(t) => format!("typed {} characters", t.chars().count()),
                Answer::Closed => "closed".into(),
            }
        );
        got
    };
    let id = field(
        t("Apple ID 로그인", "Sign in to Apple ID"),
        t("암호는 이 PC에 저장되지 않습니다", "Your password isn't stored on this PC"),
        t("Apple ID (이메일 또는 전화번호)", "Apple ID (email or phone number)"),
        "",
        false,
        t("다음", "Next"),
    );
    if ask(id) == Answer::Closed {
        return island.remove("signin");
    }
    let password = field(
        t("Apple ID 암호", "Apple ID password"),
        ME,
        t("암호", "Password"),
        "",
        true,
        t("로그인", "Sign In"),
    );
    if ask(password) == Answer::Closed {
        return island.remove("signin");
    }
    island.show(Card {
        tag: "signin".into(),
        ..signing_in(t("로그인하는 중…", "Signing in…"))
    });
    std::thread::sleep(Duration::from_secs(2));
    let code = field(
        t("확인 코드", "Verification code"),
        t("Apple 기기에 표시된 6자리 코드", "The 6-digit code on your Apple device"),
        t("6자리 코드", "6-digit code"),
        "",
        false,
        t("확인", "Verify"),
    );
    if ask(code) == Answer::Closed {
        return island.remove("signin");
    }
    island.show(Card {
        tag: "signin".into(),
        ..signing_in(t("Apple이 인증서를 발급하는 중… (1~2분)", "Apple is issuing the certificate… (1–2 min)"))
    });
    std::thread::sleep(Duration::from_secs(3));
    island.show(Card {
        tag: "signin".into(),
        ..result(
            Glyph::Done,
            Tint::Green,
            t("Apple ID에 로그인했습니다", "Signed in to Apple ID").into(),
            ME,
            "",
            t("Apple ID에 로그인했습니다", "Signed in to Apple ID"),
        )
    });
}
