//! 트레이 아이콘과 그 메뉴가 보여 주는 상태입니다. 허브가 채우고 메뉴가 읽습니다.
use std::path::PathBuf;

use crate::ipc::SendState;
use crate::settings::{Rule, Settings};

/// 메뉴가 그리는 데 필요한 모든 것.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// 보낼 곳으로 내보일 기기들: 지금 보이는 것 먼저, 그다음 최근 기기.
    pub devices: Vec<Device>,
    /// 보내기 작업들.
    pub jobs: Vec<JobLine>,
    pub settings: Settings,
    pub apple_id: AppleId,
    pub autostart: bool,
    /// 답을 기다리는 받기 요청들.
    pub asks: Vec<AskLine>,
    /// 지금 파일을 받고 있는 보내는 사람들.
    pub receiving: Vec<String>,
    /// 지금 도는 WirDrop의 버전.
    pub version: &'static str,
    /// 찾아 둔 새 릴리스의 버전.
    pub update: Option<String>,
    /// 그 릴리스를 지금 들이는 중입니다.
    pub updating: bool,
}

/// 보낼 곳으로 내보일 기기.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    /// 방금 찾았습니다. 아니면 깨우면 나타날 수도 있는 최근 기기입니다.
    pub visible: bool,
}

/// 보내기 작업 한 줄.
#[derive(Debug, Clone)]
pub struct JobLine {
    pub target: String,
    pub state: SendState,
}

/// 답을 기다리는 받기 요청 한 줄.
#[derive(Debug, Clone)]
pub struct AskLine {
    pub id: u64,
    pub sender: String,
    pub model: String,
    /// 무엇인지: "사진 2장".
    pub what: String,
}

#[derive(Debug, Clone, Default)]
pub struct AppleId {
    /// appleid.exe가 있어 로그인할 수 있습니다.
    pub can_sign_in: bool,
    /// 이 PC에 로그인한 Apple ID들. 첫 Apple ID가 먼저입니다.
    pub accounts: Vec<AppleAccount>,
}

impl AppleId {
    /// 하나라도 로그인했습니다.
    pub fn present(&self) -> bool {
        !self.accounts.is_empty()
    }
}

/// 이 PC에 로그인한 Apple ID 하나.
#[derive(Debug, Clone, Default)]
pub struct AppleAccount {
    /// 로그인 폴더. [Change::SignOut]과 다시 로그인할 때 이것으로 가리킵니다.
    pub root: PathBuf,
    /// 부를 이름 ([crate::paths::apple_id_label] 참고).
    pub label: String,
}

/// 메뉴에서 고른 설정 변경.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    DeviceName(String),
    Receive(bool),
    /// 받기 끄기, 또는 연락처만이나 모든 사람에게서 받기.
    Receiving {
        on: bool,
        contacts_only: bool,
    },
    SaveDir(PathBuf),
    /// 받은 것을 클립보드에 올릴지.
    Clipboard(bool),
    PersonRule {
        key: String,
        rule: Rule,
    },
    ForgetPerson(String),
    /// iPhone에 보일 모델 (settings::MODELS 중 하나), None이면 알아서 고릅니다.
    Model(Option<String>),
    /// 이 로그인 폴더의 Apple ID에서 로그아웃합니다.
    SignOut(PathBuf),
    /// 아일랜드가 나오는 자리 (wirdrop_island::Position의 키).
    IslandAt(String),
    Autostart(bool),
}
