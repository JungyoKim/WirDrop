//! 트레이 아이콘의 메뉴: WirDrop의 설정과 동작을 모두 담은 Windows 11 메뉴 하나. 아이콘에서 열리고
//! (wirdrop-menu 참고) 자체 창은 없습니다.
use std::path::PathBuf;

use wirdrop_menu::Entry;

use super::TrayView;
use super::hub::MenuCmd;
use super::view::{Change, Device, Snapshot};
use crate::settings::{MODELS, Rule};
use crate::win::Pick;

// Segoe Fluent Icons.
const READY: char = '\u{E701}';
const WARNING: char = '\u{E7BA}';
const SEND: char = '\u{E724}';
const PHONE: char = '\u{E8EA}';
const TABLET: char = '\u{E70A}';
const LAPTOP: char = '\u{E7F8}';
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
const CHECK: char = '\u{E73E}';
const ALWAYS: char = '\u{E8FB}';
const CANCEL: char = '\u{E711}';

/// 메뉴에 뜨는 허브 알림([notice_entries])의 앞 아이콘.
pub const WARNING_GLYPH: char = WARNING;
pub const RECEIVE_GLYPH: char = RECEIVE;
pub const ACCOUNT_GLYPH: char = ACCOUNT;
pub const INFO_GLYPH: char = '\u{E946}';

/// view로 만드는 메뉴. TranslucentTB처럼 켜고 끄는 항목은 아이콘 자리에 체크를 둡니다. 모두 한
/// 단계에 무리 지어 둡니다: 받기(누가 보낼 수 있는지, 받은 파일이 가는 폴더와 그 위치), 이 PC가
/// iPhone에 보이는 방식(이름, Mac 모델, Apple ID), 그리고 Windows와 함께 시작. 기기나 사람을
/// 가리키는 키는 view의 스냅숏 번호이므로 [pick]에도 같은 view가 필요합니다.
pub fn entries(view: &TrayView) -> Vec<Entry> {
    // 메뉴를 끌어 옮기는 손잡이인 상태 줄. 시작에 사용자가 필요하면 (관리자 권한을 거절했을 때)
    // 상태 줄이 그 방법이 됩니다.
    let status = if view.startable {
        Entry::item("restart", view.status.clone())
    } else {
        Entry::label(view.status.clone()).handle()
    };
    let status = status.glyph(if view.ready { READY } else { WARNING });
    let Some(s) = &view.snapshot else {
        return vec![status, Entry::separator(), Entry::item("exit", "종료").glyph(POWER)];
    };
    let mut items = vec![status];
    items.extend(transfer_items(s));
    items.extend([
        Entry::separator(),
        Entry::sub("받기", receive_items(s)).glyph(RECEIVE),
        Entry::item("open-dir", "폴더").glyph(FOLDER),
        Entry::item("save-dir", s.settings.save_dir.display().to_string()).glyph(SAVE),
        Entry::separator(),
    ]);
    items.extend(pc_items(s));
    items.extend([
        Entry::sub("Apple ID", apple_items(s)).glyph(ACCOUNT),
        Entry::separator(),
        Entry::sub("알림 위치", island_items(s)).glyph(BELL),
        Entry::toggle("autostart", "부팅 시 켜기", s.autostart),
        Entry::separator(),
        Entry::item("exit", "종료").glyph(POWER),
    ]);
    items
}

/// iPhone처럼 이 PC에 누가 보낼 수 있는지: 아무도, 연락처만(누구인지 알려면 Apple ID가 필요), 모든
/// 사람. 그다음 기억한 보내는 사람들과 각자의 규칙.
fn receive_items(s: &Snapshot) -> Vec<Entry> {
    let on = s.settings.receive;
    let can_contacts = s.apple_id.present();
    let contacts = on && s.settings.contacts_only && can_contacts;
    vec![
        Entry::radio("receive:off", "끔", !on),
        Entry::radio("receive:contacts", "연락처만", contacts).enabled(can_contacts),
        Entry::radio("receive:everyone", "모든 사람", on && !contacts),
        Entry::separator(),
        Entry::sub("기억한 기기", people_items(s)).glyph(PEOPLE),
    ]
}

/// 상태 줄 아래 지금 일어나는 일: 답을 기다리는 받기 요청(하나마다 선택지), 받고 있는 파일, 보내는
/// 중인 작업.
fn transfer_items(s: &Snapshot) -> Vec<Entry> {
    let mut items = Vec::new();
    for ask in &s.asks {
        items.push(Entry::sub(format!("{} · {}", ask.sender, ask.what), answers(ask.id)).glyph(device_glyph(Some(&ask.model))));
    }
    for sender in &s.receiving {
        items.push(Entry::label(format!("{sender}에서 받는 중…")).glyph(RECEIVE));
    }
    for job in s.jobs.iter().filter(|j| !j.state.is_final()) {
        let done = match job.state {
            crate::ipc::SendState::Progress { sent, total } if total > 0 => format!(" · {}%", sent * 100 / total),
            _ => String::new(),
        };
        items.push(Entry::label(format!("{}에 보내는 중{done}", job.target)).glyph(SEND));
    }
    if !items.is_empty() {
        items.insert(0, Entry::separator());
    }
    items
}

/// 받기 요청에 대한 답들 (카드의 버튼과 같습니다).
fn answers(id: u64) -> Vec<Entry> {
    vec![
        Entry::item(format!("press:accept:{id}"), "수락").glyph(CHECK),
        Entry::item(format!("press:always:{id}"), "항상 수락").glyph(ALWAYS),
        Entry::item(format!("press:decline:{id}"), "거절").glyph(CANCEL),
    ]
}

/// 아일랜드가 없을 때 아이콘 메뉴에 띄우는 받기 요청 질문: 누가, 무엇을, 그리고 답들.
pub fn ask_entries(req: &crate::ipc::IncomingInfo) -> Vec<Entry> {
    let what = super::text::describe(&req.files);
    let mut items = vec![
        Entry::label(req.sender.clone()).glyph(device_glyph(Some(&req.model))).handle(),
        Entry::label(format!("{what}{} 보내려고 합니다", super::text::eul(&what))),
        Entry::separator(),
    ];
    items.extend(answers(req.id));
    items
}

/// 카드의 제목, 줄, 버튼((글자, 인자))을 메뉴로. 아일랜드가 없을 때 씁니다.
pub fn notice_entries(glyph: char, title: &str, detail: &str, buttons: &[(&str, &str)]) -> Vec<Entry> {
    let mut items = vec![Entry::label(title).glyph(glyph).handle()];
    if !detail.is_empty() {
        items.push(Entry::label(detail));
    }
    if !buttons.is_empty() {
        items.push(Entry::separator());
        items.extend(buttons.iter().map(|(label, args)| Entry::item(format!("press:{args}"), *label)));
    }
    items
}

/// 파일을 보낼 기기 메뉴: 지금 주변에 있는 기기만 담고, 트레이가 오가는 대로 바꿉니다 (키
/// "share:<기기 번호>"). 바꿀 때를 알도록 기기와 그 밖에 보여 주는 말도 돌려줍니다.
pub fn share_entries(view: &TrayView, files: &[PathBuf]) -> (Vec<Entry>, Vec<Device>, String) {
    let what = match files {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many if many.iter().any(|f| f.is_dir()) => format!("항목 {}개", many.len()),
        many => format!("파일 {}개", many.len()),
    };
    let devices: Vec<Device> = view
        .snapshot
        .as_ref()
        .map(|s| s.devices.iter().filter(|d| d.visible).cloned().collect())
        .unwrap_or_default();
    let note = if !view.ready {
        view.status.clone()
    } else if devices.is_empty() {
        "주변 기기를 찾는 중…".to_string()
    } else {
        String::new()
    };
    let mut items = vec![Entry::label(what).glyph(SEND).handle(), Entry::separator()];
    items.extend(
        devices
            .iter()
            .enumerate()
            .map(|(i, d)| Entry::item(format!("share:{i}"), d.name.clone()).glyph(device_glyph(d.model.as_deref()))),
    );
    if !note.is_empty() {
        items.push(Entry::label(note.clone()).glyph(if view.ready { SEARCH } else { WARNING }));
    }
    (items, devices, note)
}

fn people_items(s: &Snapshot) -> Vec<Entry> {
    let mut items = Vec::new();
    if s.settings.people.is_empty() {
        items.push(Entry::label("아직 받은 적이 없습니다"));
    }
    for (i, p) in s.settings.people.iter().enumerate() {
        let rule = |key: &str, text: &str, rule: Rule| Entry::radio(format!("rule:{i}:{key}"), text, p.rule == rule);
        // 한 사람의 기기를 함께 (그 사람의 iPhone과 iPad), 아니면 기기와 그 모델.
        let label = if p.devices.len() > 1 || p.model.is_empty() {
            p.device_names()
        } else {
            format!("{} · {}", p.name, p.model)
        };
        let glyph = if p.apple_id { PERSON } else { device_glyph(Some(&p.model)) };
        items.push(
            Entry::sub(
                label,
                vec![
                    rule("default", "매번 묻기", Rule::Default),
                    rule("accept", "항상 수락", Rule::Accept),
                    rule("block", "차단", Rule::Block),
                    Entry::separator(),
                    Entry::item(format!("forget:{i}"), "목록에서 지우기").glyph(DELETE),
                ],
            )
            .glyph(glyph),
        );
    }
    items
}

/// 이 PC가 iPhone에 보이는 방식을 지금 상태대로: 고르면 바뀝니다 (이름은 입력칸, Mac은 목록).
fn pc_items(s: &Snapshot) -> Vec<Entry> {
    let model = MODELS.iter().find(|(id, _)| *id == chosen_model(s)).map_or("Mac", |(_, label)| label);
    vec![
        Entry::item("rename", s.settings.device_name.clone()).glyph(RENAME),
        Entry::sub(model, model_items(s)).glyph(PC),
    ]
}

/// iPhone에 알리는 이 PC의 Mac: 고른 것, 고르기 전에는 배터리 유무로 고른 것.
fn chosen_model(s: &Snapshot) -> &str {
    match s.settings.model.as_deref() {
        Some(model) => model,
        None => crate::win::auto_model(),
    }
}

/// iPhone이 이 PC를 보여 줄 수 있는 Mac들 (모델로 그림을 그립니다). 지금 것에 체크.
fn model_items(s: &Snapshot) -> Vec<Entry> {
    let chosen = chosen_model(s);
    MODELS
        .iter()
        .map(|(id, label)| Entry::radio(format!("model:{id}"), *label, chosen == *id))
        .collect()
}

/// 아일랜드가 나오는 자리를 화면처럼: 3×3.
fn island_items(s: &Snapshot) -> Vec<Entry> {
    use wirdrop_island::Position;
    let label = |p: Position| match p {
        Position::TopLeft => "왼쪽 위",
        Position::Top => "위",
        Position::TopRight => "오른쪽 위",
        Position::Left => "왼쪽",
        Position::Center => "가운데",
        Position::Right => "오른쪽",
        Position::BottomLeft => "왼쪽 아래",
        Position::Bottom => "아래",
        Position::BottomRight => "오른쪽 아래",
    };
    let mut items = Vec::new();
    for (i, p) in Position::ALL.into_iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            items.push(Entry::separator());
        }
        items.push(Entry::radio(format!("island-at:{}", p.key()), label(p), s.settings.island_at == p.key()));
    }
    items
}

fn apple_items(s: &Snapshot) -> Vec<Entry> {
    let a = &s.apple_id;
    let mut items: Vec<Entry> = a
        .accounts
        .iter()
        .enumerate()
        .map(|(i, account)| {
            Entry::sub(
                account.label.clone(),
                vec![
                    Entry::item(format!("signin:{i}"), "다시 로그인…").glyph(ACCOUNT).enabled(a.can_sign_in),
                    Entry::item(format!("signout:{i}"), "로그아웃…").glyph(SIGN_OUT),
                ],
            )
            .glyph(PERSON)
        })
        .collect();
    if items.is_empty() {
        items.push(Entry::label("로그인되어 있지 않습니다"));
    }
    items.push(Entry::separator());
    let sign_in = if a.present() {
        "다른 Apple ID 추가…"
    } else {
        "Apple ID로 로그인…"
    };
    items.push(Entry::item("signin:new", sign_in).glyph(ADD).enabled(a.can_sign_in));
    items
}

fn device_glyph(model: Option<&str>) -> char {
    let model = model.unwrap_or_default().to_ascii_lowercase();
    if model.contains("ipad") {
        TABLET
    } else if model.contains("mac") {
        LAPTOP
    } else {
        PHONE
    }
}

/// 메뉴 선택이 요청하는 것: 바로 허브에 넘길 일, 또는 먼저 띄울 파일·폴더 대화상자.
pub enum Picked {
    Now(MenuCmd),
    /// 대화상자를 (막히며) 띄우고 허브에 무엇을 요청할지 말합니다. 트레이 창 스레드가 아닌 따로
    /// 스레드에서 돌립니다 (tray::open_dialog 참고).
    Dialog(Box<dyn FnOnce() -> Option<MenuCmd> + Send>),
}

/// key를 고르면 요청하는 것.
pub fn pick(key: &str, view: &TrayView) -> Option<Picked> {
    let s = view.snapshot.as_ref();
    let (verb, rest) = key.split_once(':').unwrap_or((key, ""));
    let index = || rest.split(':').next()?.parse::<usize>().ok();
    let person = || s?.settings.people.get(index()?);
    let dialog = |run: Box<dyn FnOnce() -> Option<MenuCmd> + Send>| Some(Picked::Dialog(run));
    let cmd = match verb {
        "open-dir" => MenuCmd::OpenSaveDir,
        "receive" => MenuCmd::Change(match rest {
            "off" => Change::Receiving {
                on: false,
                contacts_only: s?.settings.contacts_only,
            },
            "contacts" => Change::Receiving {
                on: true,
                contacts_only: true,
            },
            _ => Change::Receiving {
                on: true,
                contacts_only: false,
            },
        }),
        "rule" => {
            let rule = match rest.rsplit(':').next()? {
                "accept" => Rule::Accept,
                "block" => Rule::Block,
                _ => Rule::Default,
            };
            MenuCmd::Change(Change::PersonRule {
                key: person()?.key.clone(),
                rule,
            })
        }
        "forget" => MenuCmd::Change(Change::ForgetPerson(person()?.key.clone())),
        "model" => MenuCmd::Change(Change::Model(Some(rest.to_string()))),
        "island-at" => MenuCmd::Change(Change::IslandAt(rest.to_string())),
        "signin" if rest == "new" => MenuCmd::SignIn(None),
        "signin" => MenuCmd::SignIn(Some(s?.apple_id.accounts.get(index()?)?.root.clone())),
        "signout" => {
            let account = s?.apple_id.accounts.get(index()?)?;
            MenuCmd::AskSignOut(account.root.clone(), account.label.clone())
        }
        "save-dir" => {
            return dialog(Box::new(|| {
                let dir = crate::win::pick(0, "받은 파일을 저장할 폴더", Pick::Folder).into_iter().next()?;
                Some(MenuCmd::Change(Change::SaveDir(dir)))
            }));
        }
        "autostart" => MenuCmd::ToggleAutostart,
        "restart" => MenuCmd::Reconnect,
        "exit" => MenuCmd::Exit,
        "press" => MenuCmd::Press(rest.to_string()),
        _ => return None,
    };
    Some(Picked::Now(cmd))
}
