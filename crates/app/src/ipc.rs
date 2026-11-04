//! WirDrop 프로세스들을 잇는 파이프입니다. 트레이 앱이 열고, 관리자 권한의 엔진과 탐색기 메뉴
//! (wirdrop_shell.dll), 다른 WirDrop.exe 실행이 접속해 먼저 인사(hello)합니다. 메시지 하나가 JSON 한 줄입니다.
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

/// \\.\pipe\WirDrop.<세션>: 로그인한 사용자 세션마다 하나. 엔진과 탐색기의 COM 대리 프로세스도 같은
/// 세션에서 접속합니다.
pub fn pipe_name() -> String {
    format!(r"\\.\pipe\WirDrop.{}", SESSION.get().copied().unwrap_or_else(current_session))
}

static SESSION: OnceLock<u32> = OnceLock::new();

/// 서비스로 도는 엔진은 세션 0에 있으므로, 맡은 사용자 세션의 트레이를 찾아가도록 정해 둡니다.
pub fn set_session(session: u32) {
    let _ = SESSION.set(session);
}

pub fn current_session() -> u32 {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0;
    // SAFETY: 출력 인자 하나를 채우는 호출.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}

/// 접속한 쪽.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Engine,
    /// 탐색기 메뉴.
    Menu,
    /// 다른 WirDrop.exe 실행 (--pick, --exit, 두 번째 실행).
    Cli,
}

/// 엔진이 어디까지 왔는지.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Stopped,
    /// 지원하는 USB 어댑터가 꽂혀 있지 않습니다.
    NoAdapter,
    /// 엔진을 띄우려면 관리자 권한이 한 번 필요합니다.
    NeedsSetup,
    /// 띄웠고, 접속을 기다리는 중.
    Launching,
    /// 어댑터와 awdl0을 올리는 중.
    Starting,
    Ready,
    Failed,
}

/// 엔진이 찾은 받는 기기.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TargetInfo {
    /// mDNS 인스턴스 이름.
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub verified: bool,
}

/// 받을 기기: 보이는 동안은 id로, 아니면 이 이름의 첫 기기.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TargetRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileInfo {
    pub name: String,
    pub size: Option<u64>,
    /// public.jpeg 같은 UTI.
    pub kind: String,
}

/// 답을 기다리는 받기 요청 (/Ask).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IncomingInfo {
    pub id: u64,
    pub sender: String,
    pub model: String,
    pub files: Vec<FileInfo>,
    /// 이 PC와 같은 Apple ID의 기기.
    pub own: bool,
    /// Apple ID 기록을 보냈습니다.
    pub apple_id: bool,
    /// [crate::record::person_key] 참고.
    pub person: String,
    /// 보내는 쪽이 붙인 미리보기를 저장한 파일.
    pub icon: Option<PathBuf>,
}

/// 받을 기기가 나타나지 않아 보내기가 멈춘 까닭 ([SendState::Failed]). 트레이가 지금 언어로
/// 바꿔 보여 줍니다.
pub const NOT_FOUND: &str = "device not found";

/// 보내기 한 건의 상태.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "s", rename_all = "snake_case")]
pub enum SendState {
    /// 엔진을 기다리는 중.
    Queued,
    /// 받는 기기를 찾는 중.
    Waiting,
    /// 보낼 파일을 묶는 중 (큰 폴더는 오래 걸립니다). 묻기 전입니다.
    Packing,
    /// 물었고, 상대의 수락을 기다리는 중.
    Asking,
    Accepted,
    Progress {
        sent: u64,
        total: u64,
    },
    Delivered,
    Declined,
    Failed {
        reason: String,
    },
    Cancelled,
}

impl SendState {
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            SendState::Delivered | SendState::Declined | SendState::Failed { .. } | SendState::Cancelled
        )
    }
}

/// 트레이가 엔진에 주는 설정.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EngineConfig {
    pub name: String,
    pub save_dir: PathBuf,
    pub receive: bool,
    /// 연락처도 내 기기도 아닌 사람에게도 보입니다 ("연락처만"이 아닌 "모든 사람").
    #[serde(default = "yes")]
    pub everyone: bool,
    /// iPhone에 알릴 Mac 모델 (settings::MODELS). ""이면 win::auto_model이 고릅니다.
    #[serde(default)]
    pub model: String,
    /// Apple ID 신원들. 하나마다 받는 쪽이 따로 돌고, 첫 Apple ID가 먼저입니다. 자체 서명 신원만
    /// 있으면 비어 있습니다.
    #[serde(default)]
    pub apple_ids: Vec<AccountDir>,
    /// 이 PC가 HEVC 동영상과 HEIF 사진을 열 수 있습니다 (wirdrop::airdrop::Media 참고).
    #[serde(default)]
    pub hevc: bool,
    #[serde(default)]
    pub heif: bool,
    /// Windows 설정의 국가 또는 지역 (ISO 3166 두 글자): 어댑터의 송신 출력 규제.
    #[serde(default)]
    pub country: Option<String>,
}

fn yes() -> bool {
    true
}

/// 엔진에 알리는, 이 PC의 Apple ID 하나.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccountDir {
    /// 신원 폴더.
    pub identity: PathBuf,
    /// 받는 쪽의 서비스 ID를 저장하는 이름. 첫 Apple ID는 PC 자신의 것을 쓰므로 "".
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    /// 모든 접속의 첫 메시지.
    Hello {
        role: Role,
        pid: u32,
    },

    // 엔진 → 트레이
    Status {
        phase: Phase,
        detail: String,
    },
    Peers {
        count: usize,
    },
    Found {
        target: TargetInfo,
    },
    Lost {
        id: String,
    },
    /// 찾기가 멈췄습니다: 찾은 기기가 모두 사라집니다.
    LostAll,
    Incoming {
        req: IncomingInfo,
    },
    Withdrawn {
        id: u64,
    },
    /// 수락한 요청에서 지금까지 쓴 파일 데이터.
    ReceiveProgress {
        id: u64,
        done: u64,
    },
    Received {
        id: u64,
        paths: Vec<PathBuf>,
    },
    ReceiveFailed {
        id: u64,
        reason: String,
    },
    Urls {
        id: u64,
        urls: Vec<String>,
    },
    SendUpdate {
        job: u64,
        state: SendState,
    },

    // 트레이 → 엔진
    Config {
        cfg: EngineConfig,
    },
    Answer {
        id: u64,
        accept: bool,
    },
    /// 적어도 secs초 더, BLE로 깨우며 찾습니다.
    Discover {
        secs: u64,
    },
    Send {
        job: u64,
        target: TargetRef,
        files: Vec<PathBuf>,
    },
    Cancel {
        job: u64,
    },
    /// 받고 있는 요청 id를 멈춥니다.
    CancelReceive {
        id: u64,
    },
    Reconnect,
    Shutdown,

    // 탐색기 메뉴와 다른 WirDrop.exe → 트레이
    /// 잠시 받는 기기를 찾습니다 (탐색기 메뉴가 열릴 때).
    Wake,
    /// 마우스 자리에 기기 메뉴를 열어 files를 보낼 곳을 고르게 합니다. 탐색기 메뉴의 AirDrop이나
    /// WirDrop --pick. 마우스 위치는 트레이가 직접 구합니다: 모니터마다의 배율을 아는 건 트레이 쪽입니다.
    Pick {
        files: Vec<PathBuf>,
    },
    /// 트레이 메뉴를 엽니다 (WirDrop.exe를 한 번 더 실행했을 때).
    OpenSettings,
    /// 트레이 앱을 끝냅니다.
    Exit,

    // 트레이 → 탐색기 메뉴와 다른 WirDrop.exe
    Ok,
}

/// 파이프의 한쪽 끝: JSON 줄들.
pub struct Conn<S> {
    lines: Lines<BufReader<ReadHalf<S>>>,
    writer: WriteHalf<S>,
}

impl Conn<NamedPipeClient> {
    /// 트레이 앱에 접속해 인사합니다. 트레이가 시작 중이거나 바쁘면 wait 동안 다시 해 봅니다.
    pub async fn connect(role: Role, wait: Duration) -> io::Result<Self> {
        const ERROR_PIPE_BUSY: i32 = 231;
        let name = pipe_name();
        let deadline = tokio::time::Instant::now() + wait;
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(c) => break c,
                Err(e)
                    if tokio::time::Instant::now() < deadline
                        && (e.raw_os_error() == Some(ERROR_PIPE_BUSY) || e.kind() == io::ErrorKind::NotFound) =>
                {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(e) => return Err(e),
            }
        };
        let mut conn = Conn::new(client);
        conn.send(&Msg::Hello {
            role,
            pid: std::process::id(),
        })
        .await?;
        Ok(conn)
    }
}

impl<S: AsyncRead + AsyncWrite> Conn<S> {
    pub fn new(stream: S) -> Self {
        let (r, w) = tokio::io::split(stream);
        Conn {
            lines: BufReader::new(r).lines(),
            writer: w,
        }
    }

    pub async fn send(&mut self, msg: &Msg) -> io::Result<()> {
        let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line).await?;
        self.writer.flush().await
    }

    /// 다음 메시지. 상대가 닫았으면 None. 취소해도 안전합니다.
    pub async fn recv(&mut self) -> io::Result<Option<Msg>> {
        loop {
            match self.lines.next_line().await? {
                None => return Ok(None),
                Some(line) if line.trim().is_empty() => {}
                Some(line) => match serde_json::from_str(&line) {
                    Ok(msg) => return Ok(Some(msg)),
                    Err(e) => tracing::warn!("ignoring a message I can't read ({e}): {line}"),
                },
            }
        }
    }

    pub fn split(self) -> (Lines<BufReader<ReadHalf<S>>>, WriteHalf<S>) {
        (self.lines, self.writer)
    }
}

/// [Conn::split]의 읽는 쪽에서 온 한 줄을 읽습니다.
pub fn parse(line: &str) -> Option<Msg> {
    serde_json::from_str(line)
        .map_err(|e| tracing::warn!("ignoring a message I can't read ({e}): {line}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let m = Msg::Pick {
            files: vec!["C:\\a b.jpg".into()],
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.starts_with("{\"t\":\"pick\""), "{s}");
        assert!(matches!(serde_json::from_str::<Msg>(&s).unwrap(), Msg::Pick { .. }));
        let u = serde_json::to_string(&Msg::SendUpdate {
            job: 1,
            state: SendState::Progress { sent: 1, total: 2 },
        })
        .unwrap();
        assert!(matches!(
            serde_json::from_str::<Msg>(&u).unwrap(),
            Msg::SendUpdate {
                state: SendState::Progress { .. },
                ..
            }
        ));
    }

    /// 탐색기 메뉴(shell 크레이트)는 serde 없이 이 꼴로 보냅니다.
    #[test]
    fn the_explorer_menus_lines_parse() {
        assert!(matches!(
            parse(r#"{"t":"hello","role":"menu","pid":7}"#),
            Some(Msg::Hello { role: Role::Menu, pid: 7 })
        ));
        assert!(matches!(parse(r#"{"t":"wake"}"#), Some(Msg::Wake)));
        assert!(matches!(parse(r#"{"t":"pick","files":["C:\\x.jpg"]}"#), Some(Msg::Pick { .. })));
    }
}
