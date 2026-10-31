//! 아일랜드에 띄우는 카드들입니다. 한 전송은 받기 요청부터 끝까지 태그 하나(recv-{id}, send-{job})의
//! 카드로 보이고, 같은 태그의 새 카드가 앞의 카드를 바꿉니다. 카드의 버튼을 누르면 그 버튼의 인자
//! ("accept:3" 같은)가 허브로 갑니다 ([HubEvent::Pressed]).
use std::path::{Path, PathBuf};

use tokio::sync::mpsc::UnboundedSender;
use wirdrop_island::{Button, Card, Glyph, Handle, Island, Kind, Position, Progress, Tint};

use super::hub::HubEvent;
use super::rate;
use super::text::{describe, eul, file_line, names, reason_text};
use crate::ipc::{IncomingInfo, SendState};

pub struct Cards {
    island: Island,
}

impl Cards {
    /// 아일랜드를 at 자리에 띄울 준비를 합니다. 띄울 수 없으면 None이고, 그때는 트레이 아이콘의
    /// 메뉴가 대신 묻고 알립니다.
    pub fn start(events: UnboundedSender<HubEvent>, at: &str) -> Option<Cards> {
        let island = Island::start(move |args| {
            let _ = events.send(HubEvent::Pressed(args));
        })
        .map_err(|e| tracing::error!("no island: {e}"))
        .ok()?;
        island.place(Position::from_key(at).unwrap_or_default());
        Some(Cards { island })
    }

    pub fn remove(&self, tag: &str) {
        self.island.remove(tag);
    }

    /// 다른 스레드가 아일랜드로 물을 때 쓰는 손잡이 (로그인의 Apple ID, 암호, 확인 코드).
    pub fn handle(&self) -> Handle {
        self.island.handle()
    }

    /// 아일랜드를 key 자리로 옮기고, 그 자리에 한 번 띄워 보여 줍니다.
    pub fn place(&self, key: &str) {
        self.island.place(Position::from_key(key).unwrap_or_default());
        self.island.show(Card {
            tag: "island-at".into(),
            kind: Kind::Info,
            glyph: Some(Glyph::Info),
            title: "알림 위치".into(),
            detail: "알림이 여기에 나타납니다.".into(),
            brief: "알림 위치".into(),
            ..Card::default()
        });
    }

    /// 카드를 띄웁니다. 방해 금지 모드가 아니면 열릴 때 Windows 알림음을 냅니다.
    fn show(&self, card: Card) {
        self.island.show(Card {
            chime: !crate::win::do_not_disturb(),
            ..card
        });
    }

    /// 받기 요청을 묻습니다. 답할 때까지 떠 있습니다.
    pub fn ask(&self, req: &IncomingInfo) {
        let id = req.id;
        let what = describe(&req.files);
        let size: u64 = req.files.iter().filter_map(|f| f.size).sum();
        let status = if size > 0 {
            format!("{} · {}", names(&req.files), crate::win::size(size))
        } else {
            names(&req.files)
        };
        self.show(Card {
            tag: format!("recv-{id}"),
            kind: Kind::Ask,
            glyph: Some(Glyph::device(&req.model)),
            picture: req.icon.clone(),
            title: req.sender.clone(),
            detail: format!("{what}{} 보내려고 합니다.", eul(&what)),
            status,
            brief: req.sender.clone(),
            buttons: vec![
                Button::new("거절", &format!("decline:{id}"), false),
                Button::new("항상 수락", &format!("always:{id}"), false),
                Button::new("수락", &format!("accept:{id}"), true),
            ],
            ..Card::default()
        });
    }

    /// 수락한 요청을 받는 중: 진행률과 멈춤 버튼이 있는 카드입니다.
    pub fn receiving(&self, req: &IncomingInfo) {
        let id = req.id;
        let total: u64 = req.files.iter().filter_map(|f| f.size).sum();
        self.show(Card {
            tag: format!("recv-{id}"),
            kind: Kind::Transfer,
            glyph: Some(Glyph::Receive),
            picture: req.icon.clone(),
            title: format!("{}에서 받는 중", req.sender),
            detail: format!("{} · {}", describe(&req.files), names(&req.files)),
            brief: "받는 중".into(),
            progress: Some(rate::bar(0, total, None, "받는 중…")),
            launch: Some(format!("recvdir:{id}")),
            cancel: Some(format!("stop:{id}")),
            ..Card::default()
        });
    }

    pub fn receive_progress(&self, id: u64, progress: Progress) {
        self.island.progress(&format!("recv-{id}"), progress);
    }

    /// 다 받았습니다: 받은 곳을 여는 버튼이 있는 마지막 카드로 바뀝니다.
    pub fn received(&self, id: u64, sender: &str, paths: &[PathBuf], icon: Option<&Path>, summary: &str) {
        let tag = format!("recv-{id}");
        let Some(first) = paths.first() else {
            self.remove(&tag);
            return;
        };
        // 하나면 그 파일을, 여럿이면 담긴 폴더를 엽니다.
        let open = if paths.len() > 1 { first.parent().unwrap_or(first) } else { first };
        let reveal = first.display().to_string();
        self.show(Card {
            tag,
            kind: Kind::Result,
            glyph: Some(Glyph::Done),
            tint: Tint::Green,
            picture: if thumbnailable(first) {
                Some(first.clone())
            } else {
                icon.map(Path::to_path_buf)
            },
            title: format!("{sender}에서 받았습니다"),
            detail: file_line(paths),
            status: summary.to_string(),
            brief: "받았습니다".into(),
            buttons: vec![
                Button::new("열기", &format!("open:{}", open.display()), true),
                Button::new("폴더 열기", &format!("reveal:{reveal}"), false),
            ],
            launch: Some(format!("reveal:{reveal}")),
            ..Card::default()
        });
    }

    /// 받다가 끊겼습니다.
    pub fn receive_failed(&self, id: u64, req: Option<&IncomingInfo>, sender: &str, reason: &str) {
        self.show(Card {
            tag: format!("recv-{id}"),
            kind: Kind::Result,
            glyph: Some(Glyph::Failed),
            tint: Tint::Red,
            title: format!("{sender}에서 받지 못했습니다"),
            detail: req.map(|r| format!("{} · {}", describe(&r.files), names(&r.files))).unwrap_or_default(),
            status: reason_text(reason),
            brief: "받지 못했습니다".into(),
            ..Card::default()
        });
    }

    /// 알림 한 장. buttons는 (글자, 누르면 허브로 갈 인자)이고 첫 버튼이 기본 버튼입니다.
    pub fn info(&self, tag: &str, title: &str, body: &str, buttons: &[(&str, &str)]) {
        let glyph = match tag {
            "adapter" => Glyph::NoWifi,
            t if t.starts_with("sign") || t.starts_with("apple") => Glyph::Lock,
            "send" => Glyph::Doc,
            _ => Glyph::Info,
        };
        self.show(Card {
            tag: tag.into(),
            kind: Kind::Info,
            glyph: Some(glyph),
            title: title.into(),
            detail: body.into(),
            brief: title.into(),
            buttons: button_row(buttons),
            ..Card::default()
        });
    }

    /// Apple ID 로그인이 진행 중입니다 ("signin" 카드, 진행 상황 글만 바뀝니다).
    pub fn signing_in(&self, status: &str) {
        self.show(Card {
            tag: "signin".into(),
            kind: Kind::Transfer,
            glyph: Some(Glyph::Lock),
            title: "Apple ID 로그인 중".into(),
            brief: "로그인 중".into(),
            progress: Some(rate::waiting(status)),
            ..Card::default()
        });
    }

    /// 로그인이 끝났습니다 (ok: 성공). 물어보고 진행하던 "signin" 카드가 결과로 바뀝니다.
    pub fn signed_in(&self, ok: bool, title: &str, body: &str, buttons: &[(&str, &str)]) {
        self.show(Card {
            tag: "signin".into(),
            kind: Kind::Result,
            glyph: Some(if ok { Glyph::Done } else { Glyph::Failed }),
            tint: if ok { Tint::Green } else { Tint::Red },
            title: title.into(),
            detail: body.into(),
            brief: title.into(),
            buttons: button_row(buttons),
            ..Card::default()
        });
    }

    /// 링크를 받았습니다: 여는 버튼이 있는 마지막 카드입니다. 지켜볼 전송이 없으니 받는 중 카드는
    /// 없습니다.
    pub fn link(&self, id: u64, sender: &str, url: &str) {
        let arg = format!("url:{url}");
        self.show(Card {
            tag: format!("recv-{id}"),
            kind: Kind::Result,
            glyph: Some(Glyph::Link),
            title: format!("{sender}에서 링크를 받았습니다"),
            detail: url.into(),
            brief: "링크".into(),
            buttons: vec![Button::new("열기", &arg, true)],
            launch: Some(arg),
            ..Card::default()
        });
    }

    /// 보내기 카드 (send-{job}): 상태가 바뀔 때마다 같은 카드가 바뀝니다. speed는 진행 중에,
    /// summary는 다 보냈을 때 씁니다. 취소하면 카드를 내립니다.
    pub fn send_state(&self, job: u64, target: &str, files: &[PathBuf], state: &SendState, speed: Option<f64>, summary: &str) {
        if *state == SendState::Cancelled {
            self.remove(&format!("send-{job}"));
            return;
        }
        self.show(send_card(job, target, files, state, speed, summary));
    }
}

/// (글자, 인자) 목록을 버튼 줄로. 첫 버튼이 기본 버튼입니다.
fn button_row(buttons: &[(&str, &str)]) -> Vec<Button> {
    buttons
        .iter()
        .enumerate()
        .map(|(i, (label, args))| Button::new(label, args, i == 0))
        .collect()
}

/// 보내기의 상태별 카드.
fn send_card(job: u64, target: &str, files: &[PathBuf], state: &SendState, speed: Option<f64>, summary: &str) -> Card {
    let going = format!("{target}에 보내는 중");
    let waiting = |brief: &str, status: &str| {
        (
            Kind::Transfer,
            Glyph::Send,
            Tint::Blue,
            going.clone(),
            brief.to_string(),
            String::new(),
            Some(rate::waiting(status)),
        )
    };
    let (kind, glyph, tint, title, brief, status, progress) = match state {
        SendState::Queued => waiting("준비 중", "USB 어댑터를 준비하는 중…"),
        SendState::Waiting => waiting("기기를 찾는 중", "기기를 찾는 중…"),
        SendState::Packing => waiting("준비 중", "보낼 파일을 준비하는 중…"),
        SendState::Asking => waiting("수락 대기 중", "수락을 기다리는 중…"),
        SendState::Accepted => waiting("보내는 중", "보내는 중…"),
        SendState::Progress { sent, total } => (
            Kind::Transfer,
            Glyph::Send,
            Tint::Blue,
            going.clone(),
            "보내는 중".into(),
            String::new(),
            Some(rate::bar(*sent, *total, speed, "보내는 중…")),
        ),
        SendState::Delivered => (
            Kind::Result,
            Glyph::Done,
            Tint::Green,
            format!("{target}에 보냈습니다"),
            "보냈습니다".into(),
            summary.to_string(),
            None,
        ),
        SendState::Declined => (
            Kind::Result,
            Glyph::Failed,
            Tint::Orange,
            format!("{target}에서 거절했습니다"),
            "거절됨".into(),
            String::new(),
            None,
        ),
        SendState::Failed { reason } => (
            Kind::Result,
            Glyph::Failed,
            Tint::Red,
            format!("{target}에 보내지 못했습니다"),
            "보내지 못했습니다".into(),
            reason_text(reason),
            None,
        ),
        SendState::Cancelled => (Kind::Info, Glyph::Info, Tint::Gray, String::new(), String::new(), String::new(), None),
    };
    let picture = files.first().filter(|f| thumbnailable(f)).cloned();
    // 진행 중인 보내기는 멈출 수 있습니다 (카드의 ■).
    let cancel = (kind == Kind::Transfer).then(|| format!("cancel:{job}"));
    Card {
        tag: format!("send-{job}"),
        kind,
        glyph: Some(glyph),
        tint,
        picture,
        title,
        detail: file_line(files),
        status,
        brief,
        progress,
        cancel,
        ..Card::default()
    }
}

/// 셸이 미리보기를 만들어 주는 사진이나 동영상.
fn thumbnailable(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "heic" | "heif" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "mov" | "mp4" | "m4v"
    )
}
