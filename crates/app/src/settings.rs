//! settings.json: 사용자가 고른 것과 우리에게 파일을 보낸 사람들. 파일은 트레이 앱이 맡고, 메뉴가
//! 바꿉니다.
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// iPhone에 이 PC를 보일 수 있는 Mac들 (iPhone은 모델 이름으로 기기 그림을 그립니다): 모델 이름과
/// 부를 이름. 종류마다 나온 지 1년 지난 가장 새 모델입니다 (Apple의 "Mac 모델 식별하기" 페이지,
/// 2026년 9월): iPhone은 자기 iOS가 아는 Mac의 그림만 있고, 이것들은 이미 최신 모습입니다.
pub const MODELS: &[(&str, &str)] = &[
    // iMac (24형, 2024, 포트 4개): M4.
    ("Mac16,3", "iMac"),
    // MacBook Pro (14형, M5, 2025).
    ("Mac17,2", "MacBook Pro"),
    // MacBook Air (13형, M4, 2025).
    ("Mac16,12", "MacBook Air"),
    // Mac mini (2024): M4.
    ("Mac16,10", "Mac mini"),
    // Mac Studio (2025): M4 Max.
    ("Mac16,9", "Mac Studio"),
    // Mac Pro (2023): M2 Ultra.
    ("Mac14,8", "Mac Pro"),
];

/// 보내는 사람 한 명에 대한 규칙.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// 매번 묻기.
    #[default]
    Default,
    Accept,
    Block,
}

/// 우리에게 파일을 보낸 사람: 기기가 Apple ID 기록을 보냈으면 사람, 아니면 기기.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Person {
    pub key: String,
    /// 보낸 기기가 마지막으로 알려 준 이름.
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub rule: Rule,
    #[serde(default)]
    pub apple_id: bool,
    #[serde(default)]
    pub last_seen: u64,
    /// 우리에게 무언가 보낸 그 사람의 모든 기기 이름, 먼저 보낸 것이 먼저: 한 사람의 iPhone과
    /// iPad는 키(Apple ID)를 함께 쓰고, name은 마지막 것뿐입니다.
    #[serde(default)]
    pub devices: Vec<String>,
}

impl Person {
    /// 메뉴에 쓸 기기 이름들: 다른 기기가 보내기 전까지는 마지막으로 보낸 기기 하나.
    pub fn device_names(&self) -> String {
        if self.devices.len() > 1 {
            self.devices.join(", ")
        } else {
            self.name.clone()
        }
    }
}

/// 우리가 보낸 적 있는 기기. 탐색기 메뉴에 다시 내보입니다.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Recent {
    pub name: String,
    pub model: Option<String>,
    pub last_used: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// iPhone에 보이는 이 PC의 이름.
    pub device_name: String,
    pub receive: bool,
    /// 연락처와 이 PC Apple ID의 기기에만 보이고 모든 사람에게는 보이지 않습니다. 누구인지 알려면
    /// Apple ID가 있어야 하므로, 없으면 모든 사람에게 보입니다.
    pub contacts_only: bool,
    /// 받은 것을 클립보드에도 올립니다 (clipboard.rs): 바로 붙여 넣을 수 있게.
    pub clipboard: bool,
    pub save_dir: PathBuf,
    pub people: Vec<Person>,
    pub recent: Vec<Recent>,
    /// iPhone에 알릴 모델, [MODELS] 중 하나. None이면 알아서 고릅니다 (win::auto_model 참고).
    pub model: Option<String>,
    /// 아일랜드가 나오는 자리: wirdrop_island::Position 키 ("top", "bottom-right", ...).
    pub island_at: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into()),
            receive: true,
            contacts_only: false,
            clipboard: true,
            save_dir: crate::paths::downloads(),
            people: Vec::new(),
            recent: Vec::new(),
            model: None,
            island_at: "top".into(),
        }
    }
}

const MAX_RECENT: usize = 6;

impl Settings {
    pub fn load() -> Settings {
        let mut settings: Settings = std::fs::read_to_string(crate::paths::settings_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // 예전 목록의 모델: 다시 고릅니다.
        settings.model = settings.model.filter(|m| MODELS.iter().any(|(id, _)| id == m));
        settings
    }

    pub fn save(&self) -> io::Result<()> {
        let path = crate::paths::settings_file();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn person(&self, key: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.key == key)
    }

    /// 보낸 사람을 기록합니다. 처음 보는 사람이면 true.
    pub fn note_person(&mut self, key: &str, name: &str, model: &str, apple_id: bool) -> bool {
        let now = now();
        if let Some(p) = self.people.iter_mut().find(|p| p.key == key) {
            // 목록이 생기기 전부터 있던 것: 알려져 있던 기기.
            if p.devices.is_empty() && !p.name.is_empty() {
                p.devices.push(p.name.clone());
            }
            if !p.devices.iter().any(|d| d == name) {
                p.devices.push(name.to_string());
            }
            p.name = name.to_string();
            p.model = model.to_string();
            p.apple_id = apple_id;
            p.last_seen = now;
            return false;
        }
        self.people.push(Person {
            key: key.to_string(),
            name: name.to_string(),
            model: model.to_string(),
            rule: Rule::Default,
            apple_id,
            last_seen: now,
            devices: vec![name.to_string()],
        });
        true
    }

    pub fn set_rule(&mut self, key: &str, rule: Rule) {
        if let Some(p) = self.people.iter_mut().find(|p| p.key == key) {
            p.rule = rule;
        }
    }

    pub fn forget(&mut self, key: &str) {
        self.people.retain(|p| p.key != key);
    }

    pub fn note_recent(&mut self, name: &str, model: Option<String>) {
        self.recent.retain(|r| !r.name.eq_ignore_ascii_case(name));
        self.recent.insert(
            0,
            Recent {
                name: name.to_string(),
                model,
                last_used: now(),
            },
        );
        self.recent.truncate(MAX_RECENT);
    }
}

/// 1970년부터의 초.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
