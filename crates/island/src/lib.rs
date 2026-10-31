//! WirDrop의 아일랜드: iPhone의 Dynamic Island를 따라 만든, 화면 가장자리의 검은 섬. 전송 하나가
//! 질문부터 끝까지 이 안에서 지냅니다: 질문에 열리고, 파일이 들어오는 동안 진행 링이 있는 작은
//! 알약으로 줄었다가, 결과와 함께 다시 열립니다. 화면에 자라 나올 섬이 없으니 화면 가장자리에서
//! 액체 방울처럼 흘러나오고, 쉬던 자리의 한 점으로 줄어들며 사라집니다. 한 줄 입력(로그인의 Apple
//! ID, 암호, 코드)도 이 안에서 받습니다.
//!
//! DWM이 돌리는 합성기 Windows.UI.Composition으로 그립니다: 섬의 윤곽(Apple의 연속 곡률 모서리)은
//! 여기서 스프링으로 뽑은 키 프레임을 따라 합성기가 바꾸는 경로이고, 내용은 바뀔 때만 Direct2D로
//! 합성 표면에 그립니다. 움직이는 동안 이 프로세스는 아무것도 하지 않고, 쉴 때는 창을 숨깁니다.
//!
//! 카드가 떠 있는 동안 Win+Alt+A를 누르면 키보드가 섬으로 옵니다: Tab과 ←→로 버튼을 고르고 Enter나
//! Space로 누르며, Esc로 원래 창에 돌려줍니다. 화면 읽기 프로그램은 UI Automation으로 읽고 누릅니다
//! (access.rs).
//!
//! 글꼴은 Pretendard, 아이콘은 Framework7 Icons이고 둘 다 내장합니다 (assets/ 참고).
mod access;
mod gfx;
mod layout;
mod scene;
mod spring;
mod squircle;
mod thumbs;
mod window;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

pub use spring::{Motion, Spring};
pub use window::full_screen;

/// 화면 문구를 영어로 쓸지. WIRDROP_LANG(ko 또는 en)이 있으면 그것을, 없으면 Windows 표시 언어를
/// 따릅니다: 한국어면 한국어, 그 밖에는 영어. 한 번 정하면 프로세스가 끝날 때까지 같습니다.
pub fn english() -> bool {
    static ENGLISH: OnceLock<bool> = OnceLock::new();
    *ENGLISH.get_or_init(|| match std::env::var("WIRDROP_LANG") {
        Ok(lang) => !lang.trim().to_ascii_lowercase().starts_with("ko"),
        Err(_) => {
            // SAFETY: 인자 없는 조회.
            let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
            // 아래 10비트가 주 언어이고 0x12가 한국어입니다.
            lang & 0x3ff != 0x12
        }
    })
}

/// 지금 언어의 문구 (한국어, 영어 순).
fn t(ko: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { ko }
}

/// Framework7 Icons의 아이콘 (tools/make-icons.py가 각각을 U+E000 + 여기의 번호에 둡니다).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Glyph {
    Done,
    Failed,
    Warning,
    Receive,
    Send,
    Phone,
    Tablet,
    Laptop,
    Desktop,
    Person,
    Folder,
    Doc,
    Photo,
    Film,
    Link,
    Check,
    Cross,
    AlwaysAccept,
    Open,
    NoWifi,
    AirDrop,
    Wait,
    Info,
    Lock,
    Quiet,
    Music,
    Text,
    Archive,
    Retry,
    Inbox,
    Share,
    PersonFill,
}

impl Glyph {
    pub fn code(self) -> char {
        char::from_u32(0xE000 + self as u32).unwrap_or('?')
    }

    /// 이미 채운 원이라 그대로 크게 그립니다. 나머지는 원판 위에 그립니다.
    pub fn round(self) -> bool {
        matches!(
            self,
            Glyph::Done | Glyph::Failed | Glyph::Receive | Glyph::Send | Glyph::Person | Glyph::Info
        )
    }

    /// 모델 이름("iPhone15,2", "iPad", "MacBookPro18,1")에 맞는 기기 아이콘.
    pub fn device(model: &str) -> Glyph {
        let m = model.to_ascii_lowercase();
        if m.contains("ipad") {
            Glyph::Tablet
        } else if m.contains("imac") || m.contains("macmini") || m.contains("macstudio") || m.contains("macpro") {
            Glyph::Desktop
        } else if m.contains("mac") {
            Glyph::Laptop
        } else {
            Glyph::Phone
        }
    }
}

/// Apple의 시스템 색 (어두운 모드).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tint {
    #[default]
    Blue,
    Green,
    Red,
    Orange,
    Gray,
}

/// 카드의 용도. 머무는 시간과 모습이 이것으로 정해집니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    /// 질문: 답하거나 내릴 때까지 열려 있습니다.
    Ask,
    /// 진행 중인 일: 처음엔 열려 있다가 작아집니다.
    Transfer,
    /// 끝난 결과: 몇 초 동안 열려 있습니다.
    Result,
    /// 알림: 몇 초 동안 열려 있습니다.
    #[default]
    Info,
}

/// 전송의 진행. fraction이 None이면 끝을 아직 모르는 진행입니다.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub fraction: Option<f32>,
    /// "12.3 MB / 30 MB"
    pub amount: String,
    /// "8.2 MB/s · 3초 남음"
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub label: String,
    /// 누르면 동작 콜백에 넘깁니다.
    pub args: String,
    /// 강조색으로 그립니다: 대부분이 원하는 버튼.
    pub primary: bool,
}

impl Button {
    pub fn new(label: &str, args: &str, primary: bool) -> Self {
        Button {
            label: label.into(),
            args: args.into(),
            primary,
        }
    }
}

/// 섬이 보여 주는 것. 카드는 태그로 관리합니다: 떠 있는 태그로 다시 보여 주면 그 자리에서 바뀝니다.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub tag: String,
    pub kind: Kind,
    pub glyph: Option<Glyph>,
    pub tint: Tint,
    /// 아이콘 대신 미리보기를 보일 파일 (미리보기나 받은 것).
    pub picture: Option<PathBuf>,
    pub title: String,
    pub detail: String,
    /// 세 번째 줄: 결과, 크기.
    pub status: String,
    /// 작은 섬에 보이는 짧은 말.
    pub brief: String,
    pub progress: Option<Progress>,
    pub buttons: Vec<Button>,
    /// 카드 자체를 누르면 동작 콜백에 넘깁니다.
    pub launch: Option<String>,
    /// 진행 막대가 처음 보일 때 (전송이 시작될 때) Windows 알림음을 냅니다. 다른 것은 소리를 내지
    /// 않습니다.
    pub chime: bool,
    /// 입력받을 한 줄 ([Handle::ask]).
    pub field: Option<Field>,
    /// 열렸을 때 오른쪽 위의 X. 누르면 이것을 동작 콜백에 넘깁니다 (전송 멈춤).
    pub cancel: Option<String>,
}

/// 카드가 묻는 한 줄 입력. 섬에서 입력하고, 떠 있는 동안 섬이 키보드를 가져갔다가 돌려줍니다.
#[derive(Clone, Default, PartialEq)]
pub struct Field {
    /// 빈 칸에 보이는 안내.
    pub placeholder: String,
    /// 처음에 들어 있는 것.
    pub value: String,
    /// 점으로 보입니다 (암호).
    pub secret: bool,
    /// The button that sends it ("다음", "로그인"); Enter does too.
    pub action: String,
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = if self.secret { "…" } else { self.value.as_str() };
        f.debug_struct("Field")
            .field("placeholder", &self.placeholder)
            .field("value", &value)
            .field("action", &self.action)
            .finish()
    }
}

/// 카드 입력칸의 결과.
#[derive(PartialEq, Eq)]
pub enum Answer {
    /// 입력해서 보냈습니다 (Enter나 카드의 버튼).
    Typed(String),
    /// 입력 없이 닫았거나 (Esc, 닫기 버튼) 내렸습니다.
    Closed,
}

impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Answer::Typed(_) => "Typed(…)",
            Answer::Closed => "Closed",
        })
    }
}

/// 섬이 나오는 화면 위치: 가운데, 또는 화면(작업 영역)의 가장자리나 모서리에 붙어 거기서 미끄러져
/// 나옵니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Position {
    TopLeft,
    #[default]
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Position {
    /// 화면에서처럼 줄마다.
    pub const ALL: [Position; 9] = [
        Position::TopLeft,
        Position::Top,
        Position::TopRight,
        Position::Left,
        Position::Center,
        Position::Right,
        Position::BottomLeft,
        Position::Bottom,
        Position::BottomRight,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Position::TopLeft => "top-left",
            Position::Top => "top",
            Position::TopRight => "top-right",
            Position::Left => "left",
            Position::Center => "center",
            Position::Right => "right",
            Position::BottomLeft => "bottom-left",
            Position::Bottom => "bottom",
            Position::BottomRight => "bottom-right",
        }
    }

    pub fn from_key(key: &str) -> Option<Position> {
        Position::ALL.into_iter().find(|p| p.key() == key)
    }

    /// 섬이 붙는 곳을 화면 가로·세로 비율로 (0, 0.5, 1).
    pub(crate) fn anchor(self) -> (f32, f32) {
        let i = Position::ALL.iter().position(|p| *p == self).unwrap_or(1);
        ((i % 3) as f32 * 0.5, (i / 3) as f32 * 0.5)
    }

    /// 섬이 나오는 가장자리를 화면 밖으로 나가는 방향으로: 위, 아래, 왼쪽, 오른쪽. 나올 가장자리가
    /// 없는 곳(가운데나 작업 표시줄 쪽)은 None이고 그 자리에서 자랍니다. bezels: 위, 아래, 왼쪽,
    /// 오른쪽 순으로 화면 가장자리(작업 표시줄이 아닌 곳)면 true. 위쪽 모서리는 iPhone의 섬처럼 위
    /// 가장자리를, 아래쪽 모서리는 아래가 보통 작업 표시줄이므로 옆 가장자리를 씁니다.
    pub(crate) fn edge(self, bezels: [bool; 4]) -> Option<(f32, f32)> {
        let [top, bottom, left, right] = bezels;
        let (fx, fy) = self.anchor();
        let up_down = if fy == 0.0 && top {
            Some((0.0, -1.0))
        } else if fy == 1.0 && bottom {
            Some((0.0, 1.0))
        } else {
            None
        };
        let side = if fx == 0.0 && left {
            Some((-1.0, 0.0))
        } else if fx == 1.0 && right {
            Some((1.0, 0.0))
        } else {
            None
        };
        match (fx == 0.5, fy) {
            (true, _) => up_down,
            (false, 0.5) => side,
            (false, 0.0) => up_down.or(side),
            _ => side.or(up_down),
        }
    }
}

enum Cmd {
    Show(Box<Card>),
    /// 입력칸이 있는 카드와 그 답이 갈 곳.
    Ask(Box<Card>, Sender<Answer>),
    Progress(String, Progress),
    Remove(String),
    Place(Position),
    Quit,
}

/// 섬의 스레드. 버리면 섬을 내립니다.
pub struct Island {
    handle: Handle,
}

/// 다른 스레드가 섬에 보여 주고 묻는 손잡이 ([Island]가 있는 동안 유효).
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Cmd>,
    wake: window::Waker,
}

impl Island {
    /// 섬의 스레드와 창을 시작합니다. on_action은 그 스레드에서 누른 버튼의 args(또는 카드의
    /// launch)를 받습니다.
    pub fn start(on_action: impl Fn(String) + Send + 'static) -> std::io::Result<Island> {
        let (tx, rx) = channel();
        let wake = window::spawn(rx, Box::new(on_action))?;
        Ok(Island { handle: Handle { tx, wake } })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn show(&self, card: Card) {
        self.handle.show(card);
    }

    /// 전송 카드의 진행을 바꿉니다. 떠 있지 않으면 아무것도 하지 않습니다.
    pub fn progress(&self, tag: &str, progress: Progress) {
        self.handle.send(Cmd::Progress(tag.into(), progress));
    }

    pub fn remove(&self, tag: &str) {
        self.handle.remove(tag);
    }

    /// 다음부터 섬이 나올 자리.
    pub fn place(&self, position: Position) {
        self.handle.send(Cmd::Place(position));
    }
}

impl Handle {
    pub fn show(&self, card: Card) {
        self.send(Cmd::Show(Box::new(card)));
    }

    pub fn remove(&self, tag: &str) {
        self.send(Cmd::Remove(tag.into()));
    }

    /// card.field를 묻습니다: 섬이 그 카드로 열려 키보드를 가져가고, 답은 받는 쪽으로 옵니다.
    /// 그사이 같은 태그로 보여 준 카드는 질문 뒤에서 기다렸다가 답하면 올라오고, 태그를 내리면
    /// 질문이 닫힙니다.
    pub fn ask(&self, card: Card) -> Receiver<Answer> {
        let (reply, answer) = channel();
        self.send(Cmd::Ask(Box::new(card), reply));
        answer
    }

    fn send(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            self.wake.wake();
        }
    }
}

impl Drop for Island {
    fn drop(&mut self) {
        self.handle.send(Cmd::Quit);
    }
}

#[cfg(test)]
mod tests {
    use super::Position;

    /// Windows 기본처럼 아래쪽에 있는 작업 표시줄.
    const TASKBAR_BOTTOM: [bool; 4] = [true, false, true, true];

    #[test]
    fn it_comes_out_of_the_screens_edges_not_the_taskbar() {
        assert_eq!(Position::Top.edge(TASKBAR_BOTTOM), Some((0.0, -1.0)));
        assert_eq!(Position::TopLeft.edge(TASKBAR_BOTTOM), Some((0.0, -1.0)));
        // 아래쪽 모서리는 옆에서 나오고, 아래 가운데는 나올 가장자리가 없습니다.
        assert_eq!(Position::BottomRight.edge(TASKBAR_BOTTOM), Some((1.0, 0.0)));
        assert_eq!(Position::BottomLeft.edge(TASKBAR_BOTTOM), Some((-1.0, 0.0)));
        assert_eq!(Position::Bottom.edge(TASKBAR_BOTTOM), None);
        assert_eq!(Position::Center.edge([true; 4]), None);
        // 왼쪽 작업 표시줄: 왼쪽 가운데는 그 자리에서 자라고, 왼쪽 위는 위에서 나옵니다.
        let left_bar = [true, true, false, true];
        assert_eq!(Position::Left.edge(left_bar), None);
        assert_eq!(Position::TopLeft.edge(left_bar), Some((0.0, -1.0)));
        assert_eq!(Position::BottomLeft.edge(left_bar), Some((0.0, 1.0)));
    }
}
