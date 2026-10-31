//! 섬 안에서 무엇이 어디에 놓이는지(pt, DIP): Dynamic Island에 대한 Apple의 수치. 카드를 그릴 때와
//! 마우스로 맞힐 때 똑같이 씁니다.
use std::path::PathBuf;

use crate::{Card, Field, Glyph, Kind, Tint};

/// 붙어 있는 화면 가장자리(iPhone에서는 위)와의 거리.
pub const EDGE: f32 = 11.0;
/// 열린 크기, 430pt iPhone 기준 (Live Activity의 확장 표시: 높이 84~160).
pub const WIDE: f32 = 408.0;
pub const MIN_H: f32 = 84.0;
pub const MAX_H: f32 = 160.0;
/// 글만 있는 카드(막대도 버튼도 없음)는 높이 MIN_H인 알약 하나이고, 글은 위아래에서 적어도 이만큼
/// 떨어집니다.
pub const PILL_INSET: f32 = 12.0;
/// 열린 섬의 모서리 (Apple의 44).
pub const CORNER: f32 = 44.0;
/// 열렸을 때 내용 여백: 44 - 22 = 22, 모서리와 동심인 44pt 앞 항목의 반지름.
pub const M: f32 = 22.0;
/// 작은 크기, compact 표시처럼 (높이 36.67, 앞 보기 폭 52.33).
pub const SMALL_H: f32 = 36.67;
/// 작을 때 여백: 18.33 - 6.33 = 12, 24pt 앞 항목의 반지름.
pub const SM: f32 = 6.33;
/// 그림자와 목표를 지나치는 스프링을 위해 창 안에서 섬 둘레에 두는 여유.
pub const SHADE: f32 = 24.0;
pub const CANVAS_W: f32 = WIDE + 2.0 * SHADE;
pub const CANVAS_H: f32 = MAX_H + 2.0 * SHADE;
/// 카드 입력칸의 버튼이 (앱이 아니라) 섬에 주는 것: 입력한 것을 보내기, 또는 닫기.
pub const SUBMIT: &str = "\u{1}submit";
pub const CLOSE: &str = "\u{1}close";
/// 닫기(X) 버튼: 이 폭의 원판.
const CLOSE_SIZE: f32 = 30.0;
/// 열렸을 때 전송의 링 (안에 멈춤 버튼): 앞 항목만큼 넓습니다.
pub const CONTROL: f32 = 44.0;

/// size 크기의 섬 사각형을 창 안에서 position의 기준점에 붙여 둔 것.
pub fn place(position: crate::Position, w: f32, h: f32) -> Rect {
    let (fx, fy) = position.anchor();
    let at = |f: f32, canvas: f32| {
        if f == 0.0 {
            EDGE
        } else if f == 1.0 {
            canvas - EDGE
        } else {
            canvas / 2.0
        }
    };
    Rect::new(at(fx, CANVAS_W) - fx * w, at(fy, CANVAS_H) - fy * h, w, h)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Small,
    Open,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn shift(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inflate(&self, d: f32) -> Rect {
        Rect {
            x: self.x - d,
            y: self.y - d,
            w: self.w + 2.0 * d,
            h: self.h + 2.0 * d,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    /// 15 semibold
    Title,
    /// 13 medium
    Body,
    /// 12 medium, 고정폭 숫자
    Small,
    /// 15 semibold, 고정폭 숫자
    Number,
    /// 15 semibold
    Button,
    /// 13 semibold
    Brief,
    /// 15 regular: 입력칸에 입력한 글
    Field,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    White,
    Secondary,
    Tertiary,
    Tint(Tint),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Text {
        rect: Rect,
        text: String,
        style: Style,
        ink: Ink,
        right: bool,
        lines: u32,
    },
    /// 아이콘의 잉크를 rect에 맞춘 것.
    Icon {
        rect: Rect,
        glyph: Glyph,
        ink: Ink,
    },
    /// 채운 원.
    Disc {
        rect: Rect,
        alpha: f32,
    },
    /// rect를 채우도록 자른 파일 미리보기. 없으면 원판 위의 아이콘.
    Picture {
        rect: Rect,
        radius: f32,
        path: PathBuf,
        glyph: Glyph,
        tint: Tint,
    },
    Capsule {
        rect: Rect,
        primary: bool,
        tint: Tint,
        label: String,
    },
    /// 멈춤 버튼의 사각형, 강조색 (전송의 링 안).
    Stop {
        rect: Rect,
        tint: Tint,
    },
    /// 입력칸의 상자(rect)와, 그 안에 입력한 것(비밀이면 점)을 inner에 한 줄로 scroll만큼 왼쪽으로
    /// 밀어 그린 것. selection: 글을 따라 from부터 to까지. 비었으면 안내 문구.
    Field {
        rect: Rect,
        inner: Rect,
        text: String,
        placeholder: String,
        scroll: f32,
        selection: Option<(f32, f32)>,
    },
}

/// 섬에 보이는 카드 입력칸의 입력.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typing {
    /// 입력한 것 (비밀이면 점).
    pub shown: String,
    /// shown 안의 글자 위치로 캐럿과 선택의 다른 끝.
    pub caret: usize,
    pub anchor: usize,
    /// 스크롤한 정도(pt): 캐럿이 상자 밖으로 나가지 않으면 그대로 둡니다.
    pub scroll: f32,
    /// 섬이 키보드를 가지고 있어 캐럿이 보입니다.
    pub focused: bool,
}

/// 배치를 마친 카드: 그 카드에 맞는 섬의 크기와 모서리, 그릴 것, 움직이는 것 (막대, 링, 스피너는
/// 합성기가 스스로 움직입니다).
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub items: Vec<Item>,
    pub buttons: Vec<(Rect, String)>,
    pub bar: Option<Rect>,
    pub ring: Option<Rect>,
    pub spinner: Option<Rect>,
    pub fraction: f32,
    pub tint: Tint,
    /// 입력칸의 상자와 캐럿 (섬이 키보드를 가졌을 때).
    pub field: Option<Rect>,
    pub caret: Option<Rect>,
    /// 입력칸의 글이 있는 곳 (상자에서 안쪽 여백을 뺀 곳)과 왼쪽으로 스크롤한 정도.
    pub field_text: Option<Rect>,
    pub field_scroll: f32,
}

impl Face {
    /// 모양과 움직이는 부분이 같습니다: 바뀐 것을 그 자리에 그릴 수 있습니다 (캐럿이 움직임).
    pub fn same_shape(&self, other: &Face) -> bool {
        self.w == other.w
            && self.h == other.h
            && self.bar == other.bar
            && self.ring == other.ring
            && self.spinner == other.spinner
            && self.buttons == other.buttons
            && self.field == other.field
    }
}

/// DirectWrite의 글 재기 (테스트에서는 대역).
pub trait Measure {
    /// 한 줄에 놓인 text의 폭.
    fn width(&self, text: &str, style: Style) -> f32;
    /// 한 줄의 높이.
    fn line(&self, style: Style) -> f32;
    /// text가 width 안에서 차지하는 줄 수, 최대 max.
    fn lines(&self, text: &str, style: Style, width: f32, max: u32) -> u32;
    /// 한 줄의 text를 따라 x에 가장 가까운 글자 사이 (글자 단위).
    fn index_at(&self, text: &str, style: Style, x: f32) -> usize;
}

/// typing: 카드에 입력칸이 있으면 거기 입력한 것.
pub fn face(card: &Card, mode: Mode, m: &dyn Measure, typing: Option<&Typing>) -> Face {
    match (mode, &card.field) {
        (Mode::Open, Some(field)) if card.kind == Kind::Ask => input(card, field, typing, m),
        (Mode::Open, _) => open(card, m),
        (Mode::Small, _) => small(card, m),
    }
}

fn glyph_of(card: &Card) -> Glyph {
    card.glyph.unwrap_or(match card.kind {
        Kind::Ask => Glyph::AirDrop,
        Kind::Transfer => Glyph::Receive,
        Kind::Result => Glyph::Done,
        Kind::Info => Glyph::Info,
    })
}

/// 앞 항목: 그림, 또는 아이콘 (원이면 크게, 아니면 원판 위에).
fn leading(items: &mut Vec<Item>, card: &Card, rect: Rect) {
    let glyph = glyph_of(card);
    if let Some(path) = &card.picture {
        // 대신하는 아이콘처럼 (그리고 연락처 사진처럼) 둥글게.
        items.push(Item::Picture {
            rect,
            radius: rect.w / 2.0,
            path: path.clone(),
            glyph,
            tint: card.tint,
        });
        // 무슨 일인지 그림 모서리의 배지로.
        if rect.w > 30.0 && card.kind != Kind::Ask {
            let b = Rect::new(rect.right() - 15.0, rect.bottom() - 15.0, 22.0, 22.0);
            items.push(Item::Disc {
                rect: b.inflate(1.5),
                alpha: -1.0,
            });
            items.push(Item::Icon {
                rect: b,
                glyph: if glyph.round() { glyph } else { Glyph::Done },
                ink: Ink::Tint(card.tint),
            });
        }
    } else if glyph.round() {
        items.push(Item::Icon {
            rect,
            glyph,
            ink: Ink::Tint(card.tint),
        });
    } else {
        items.push(Item::Disc { rect, alpha: 0.16 });
        let g = rect.w * 0.55;
        items.push(Item::Icon {
            rect: Rect::new(rect.x + (rect.w - g) / 2.0, rect.y + (rect.h - g) / 2.0, g, g),
            glyph,
            ink: Ink::White,
        });
    }
}

fn percent(f: f32) -> String {
    format!("{}%", (f.clamp(0.0, 1.0) * 100.0).floor() as u32)
}

/// 희미한 원판 위의 X를 rect에: 질문의 닫기 버튼, 전송의 멈춤.
fn close_button(items: &mut Vec<Item>, rect: Rect) {
    items.push(Item::Disc { rect, alpha: 0.16 });
    let g = rect.w * 11.0 / 30.0;
    items.push(Item::Icon {
        rect: Rect::new(rect.x + (rect.w - g) / 2.0, rect.y + (rect.h - g) / 2.0, g, g),
        glyph: Glyph::Cross,
        ink: Ink::Secondary,
    });
}

/// 한 줄을 묻는 카드: 아이콘, 제목, 설명과 오른쪽의 닫기 버튼, 그 아래 입력칸과 보내기 버튼.
fn input(card: &Card, field: &Field, typing: Option<&Typing>, m: &dyn Measure) -> Face {
    let w = WIDE;
    let mut items = Vec::new();
    leading(&mut items, card, Rect::new(M, M, 44.0, 44.0));
    let close = Rect::new(w - M - CLOSE_SIZE, M + 22.0 - CLOSE_SIZE / 2.0, CLOSE_SIZE, CLOSE_SIZE);
    let text_x = M + 44.0 + 12.0;
    let text_w = close.x - 8.0 - text_x;
    let lines: Vec<(Style, Ink, &String)> = [(Style::Title, Ink::White, &card.title), (Style::Body, Ink::Secondary, &card.detail)]
        .into_iter()
        .filter(|l| !l.2.is_empty())
        .collect();
    const GAP: f32 = 2.0;
    let block = lines.iter().map(|l| m.line(l.0)).sum::<f32>() + GAP * (lines.len().max(1) - 1) as f32;
    let mut y = M + (44.0 - block) / 2.0;
    for (style, ink, text) in lines {
        let h = m.line(style);
        items.push(Item::Text {
            rect: Rect::new(text_x, y, text_w, h + 1.0),
            text: text.clone(),
            style,
            ink,
            right: false,
            lines: 1,
        });
        y += h + GAP;
    }
    close_button(&mut items, close);

    let row = M + 44.0 + 14.0;
    let label = if field.action.is_empty() {
        crate::t("확인", "OK")
    } else {
        field.action.as_str()
    };
    let bw = (m.width(label, Style::Button) + 40.0).max(76.0);
    let send = Rect::new(w - M - bw, row, bw, 44.0);
    let rect = Rect::new(M, row, send.x - 8.0 - M, 44.0);
    const PAD: f32 = 16.0;
    let lh = m.line(Style::Field);
    let inner = Rect::new(rect.x + PAD, row + (44.0 - lh) / 2.0, rect.w - 2.0 * PAD, lh);
    let t = typing.cloned().unwrap_or_default();
    let along = |chars: usize| m.width(&t.shown.chars().take(chars).collect::<String>(), Style::Field);
    let at = along(t.caret);
    // 스크롤한 그대로 두되, 그러면 캐럿이 상자 밖으로 나가면 (또는 끝에 빈자리가 생기면) 맞춥니다.
    let room = inner.w - 2.0;
    let total = along(usize::MAX);
    let scroll = t.scroll.max(at - room).min(at).min((total - room).max(0.0)).max(0.0);
    let selection = (t.anchor != t.caret).then(|| {
        let other = along(t.anchor);
        (at.min(other), at.max(other))
    });
    items.push(Item::Field {
        rect,
        inner,
        text: t.shown.clone(),
        placeholder: field.placeholder.clone(),
        scroll,
        selection,
    });
    let caret = (t.focused && selection.is_none()).then(|| Rect::new(inner.x + at - scroll - 1.0, inner.y + 1.0, 2.0, lh - 2.0));
    items.push(Item::Capsule {
        rect: send,
        primary: true,
        tint: Tint::Blue,
        label: label.to_string(),
    });
    let h = row + 44.0 + M;
    Face {
        w,
        h,
        radius: CORNER.min(h / 2.0),
        items,
        buttons: vec![(close, CLOSE.to_string()), (send, SUBMIT.to_string())],
        bar: None,
        ring: None,
        spinner: None,
        fraction: 0.0,
        tint: card.tint,
        field: Some(rect),
        caret,
        field_text: Some(inner),
        field_scroll: scroll,
    }
}

fn open(card: &Card, m: &dyn Measure) -> Face {
    let w = WIDE;
    let mut items = Vec::new();
    let text_x = M + 44.0 + 12.0;
    let progress = card.progress.as_ref().filter(|_| card.kind == Kind::Transfer);
    let fraction = progress.and_then(|p| p.fraction);
    // iPhone의 AirDrop처럼 진행 중인 모습: 오른쪽에 진행 링(보일 진행이 없으면 돌아감)과 그 안의
    // 멈춤 버튼, 옆에 퍼센트. 기다리는 동안에도 자리는 같아서 퍼센트가 나타나도 글이 움직이지
    // 않습니다.
    let pw = m.width("100%", Style::Number);
    let reserve = if progress.is_some() { CONTROL + 10.0 + pw + 12.0 } else { 0.0 };
    let text_w = w - text_x - M - reserve;
    let waiting = progress.is_some_and(|p| p.fraction.is_none());
    let status = match progress {
        // 얼마나 빨리 얼마만큼, 또는 들어가는 만큼만.
        Some(p) if !waiting => {
            let both = [p.amount.as_str(), p.status.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            if m.width(&both, Style::Small) <= text_w {
                both
            } else {
                p.status.clone()
            }
        }
        Some(p) => p.status.clone(),
        None => card.status.clone(),
    };
    let shows_status = !status.is_empty();
    // 글만 있음 (옆의 링 포함): 아래에 버튼이 없습니다.
    let alone = card.buttons.is_empty();
    // 설명은 자리가 있으면 (아래에 상태 줄이 없으면) 버튼이 있든 없든 두 번째 줄을 씁니다: "…"로
    // 잘린 까닭은 아무 말도 하지 못합니다.
    let detail_lines = if !shows_status {
        m.lines(&card.detail, Style::Body, text_w, 2)
    } else {
        1
    };
    let mut lines: Vec<(Style, Ink, String, u32)> = vec![(Style::Title, Ink::White, card.title.clone(), 1)];
    if !card.detail.is_empty() {
        lines.push((Style::Body, Ink::Secondary, card.detail.clone(), detail_lines));
    }
    if shows_status {
        let ink = if card.kind == Kind::Result && card.tint != Tint::Green {
            Ink::Tint(card.tint)
        } else {
            Ink::Tertiary
        };
        lines.push((Style::Small, ink, status, 1));
    }
    const GAP: f32 = 2.0;
    let block_h: f32 = lines.iter().map(|(s, _, _, n)| m.line(*s) * *n as f32).sum::<f32>() + GAP * (lines.len() as f32 - 1.0);
    let row_h = block_h.max(44.0);
    // 글만 있으면 줄 수(최대 3)와 상관없이 알약 하나: 그 사이 높이에 열린 섬의 모서리를 쓰면 알약도
    // 카드도 아닙니다.
    let pill = alone && row_h + 2.0 * PILL_INSET <= MIN_H;
    let top = if pill { (MIN_H - row_h) / 2.0 } else { M };
    leading(&mut items, card, Rect::new(M, top + (row_h - 44.0) / 2.0, 44.0, 44.0));
    let mut y = top + (row_h - block_h) / 2.0;
    for (style, ink, text, n) in lines {
        let h = m.line(style) * n as f32;
        items.push(Item::Text {
            rect: Rect::new(text_x, y, text_w, h + 1.0),
            text,
            style,
            ink,
            right: false,
            lines: n,
        });
        y += h + GAP;
    }
    let row_mid = top + row_h / 2.0;
    let control = progress.map(|_| Rect::new(w - M - CONTROL, row_mid - CONTROL / 2.0, CONTROL, CONTROL));
    let (ring, spinner) = if waiting { (None, control) } else { (control, None) };
    if let (Some(c), Some(f)) = (control, fraction) {
        let lh = m.line(Style::Number);
        items.push(Item::Text {
            rect: Rect::new(c.x - 10.0 - pw, row_mid - lh / 2.0, pw, lh + 1.0),
            text: percent(f),
            style: Style::Number,
            ink: Ink::Tint(card.tint),
            right: true,
            lines: 1,
        });
    }
    let mut buttons = Vec::new();
    if let (Some(c), Some(args)) = (control, &card.cancel) {
        let s = CONTROL * 0.38;
        items.push(Item::Stop {
            rect: Rect::new(c.x + (c.w - s) / 2.0, c.y + (c.h - s) / 2.0, s, s),
            tint: card.tint,
        });
        buttons.push((c, args.clone()));
    }
    let mut y = top + row_h;
    if !card.buttons.is_empty() {
        y += 14.0;
        let mut x = M;
        for (b, bw) in card.buttons.iter().zip(button_widths(m, &card.buttons, w - 2.0 * M)) {
            let rect = Rect::new(x, y, bw, 44.0);
            x += bw + 8.0;
            // Apple처럼 동작 버튼은 파란색입니다.
            items.push(Item::Capsule {
                rect,
                primary: b.primary,
                tint: Tint::Blue,
                label: b.label.clone(),
            });
            buttons.push((rect, b.args.clone()));
        }
        y += 44.0;
    }
    let h = if pill { MIN_H } else { (y + M).clamp(MIN_H, MAX_H) };
    Face {
        w,
        h,
        radius: CORNER.min(h / 2.0),
        items,
        buttons,
        bar: None,
        ring,
        spinner,
        fraction: fraction.unwrap_or(0.0),
        tint: card.tint,
        field: None,
        caret: None,
        field_text: None,
        field_scroll: 0.0,
    }
}

/// 버튼 줄의 너비들 (사이 8, 모두 합쳐 row). 글자가 모두 들어가면 같은 너비로, 하나라도 넘치면
/// 글자 길이에 맞추고 남는 자리는 똑같이 나눕니다 (영어의 "Always Accept" 같은 것).
fn button_widths(m: &dyn Measure, buttons: &[crate::Button], row: f32) -> Vec<f32> {
    let n = buttons.len() as f32;
    let room = row - 8.0 * (n - 1.0);
    let need: Vec<f32> = buttons.iter().map(|b| m.width(&b.label, Style::Button) + 32.0).collect();
    if need.iter().all(|w| *w <= room / n) {
        return vec![room / n; buttons.len()];
    }
    let total: f32 = need.iter().sum();
    if total >= room {
        return need.iter().map(|w| w * room / total).collect();
    }
    need.iter().map(|w| w + (room - total) / n).collect()
}

fn small(card: &Card, m: &dyn Measure) -> Face {
    let h = SMALL_H;
    let mut items = Vec::new();
    let progress = card.progress.as_ref().filter(|_| card.kind == Kind::Transfer);
    let pct = progress.and_then(|p| p.fraction).map(percent);
    let pw = pct.as_ref().map_or(0.0, |_| m.width("100%", Style::Brief));
    let brief = if card.brief.is_empty() { &card.title } else { &card.brief };
    let bw = m.width(brief, Style::Brief);
    let lead = SM + 24.0 + 9.0;
    let trail = 9.0 + if pw > 0.0 { pw + 7.0 } else { 0.0 } + 24.0 + SM;
    let w = (lead + bw + trail).clamp(230.0, 340.0);
    leading(&mut items, card, Rect::new(SM, SM, 24.0, 24.0));
    let lh = m.line(Style::Brief);
    items.push(Item::Text {
        rect: Rect::new(lead, (h - lh) / 2.0, w - lead - trail, lh),
        text: brief.clone(),
        style: Style::Brief,
        ink: Ink::White,
        right: false,
        lines: 1,
    });
    let end = Rect::new(w - SM - 24.0, SM, 24.0, 24.0);
    let (mut ring, mut spinner) = (None, None);
    match progress.map(|p| p.fraction) {
        Some(Some(_)) => ring = Some(end),
        Some(None) => spinner = Some(end),
        None => {
            let glyph = match card.kind {
                Kind::Result if card.tint == Tint::Green => Some(Glyph::Done),
                Kind::Result => Some(Glyph::Failed),
                _ => None,
            };
            if let Some(glyph) = glyph {
                items.push(Item::Icon {
                    rect: end,
                    glyph,
                    ink: Ink::Tint(card.tint),
                });
            }
        }
    }
    if let Some(text) = pct {
        items.push(Item::Text {
            rect: Rect::new(end.x - 7.0 - pw, (h - lh) / 2.0, pw, lh),
            text,
            style: Style::Brief,
            ink: Ink::Tint(card.tint),
            right: true,
            lines: 1,
        });
    }
    Face {
        w,
        h,
        radius: h / 2.0,
        items,
        buttons: Vec::new(),
        bar: None,
        ring,
        spinner,
        fraction: progress.and_then(|p| p.fraction).unwrap_or(0.0),
        tint: card.tint,
        field: None,
        caret: None,
        field_text: None,
        field_scroll: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Button, Progress};

    struct Fixed;
    impl Measure for Fixed {
        fn width(&self, text: &str, _: Style) -> f32 {
            text.chars().count() as f32 * 7.0
        }
        fn line(&self, style: Style) -> f32 {
            match style {
                Style::Title | Style::Number | Style::Button | Style::Field => 19.0,
                Style::Body | Style::Brief => 16.0,
                Style::Small => 15.0,
            }
        }
        fn lines(&self, text: &str, s: Style, width: f32, max: u32) -> u32 {
            ((self.width(text, s) / width).ceil() as u32).clamp(1, max)
        }
        fn index_at(&self, text: &str, _: Style, x: f32) -> usize {
            ((x / 7.0).round().max(0.0) as usize).min(text.chars().count())
        }
    }

    fn ask() -> Card {
        Card {
            tag: "recv-1".into(),
            kind: Kind::Ask,
            title: "홍길동의 iPhone".into(),
            detail: "사진 3장을 보내려고 합니다.".into(),
            status: "IMG_0001.HEIC 외 2개 · 12.3 MB".into(),
            buttons: vec![
                Button::new("거절", "decline:1", false),
                Button::new("항상 수락", "always:1", false),
                Button::new("수락", "accept:1", true),
            ],
            ..Card::default()
        }
    }

    #[test]
    fn open_cards_fit_apples_heights() {
        let f = face(&ask(), Mode::Open, &Fixed, None);
        assert!(f.h >= MIN_H && f.h <= MAX_H, "{}", f.h);
        assert_eq!(f.buttons.len(), 3);
        // 버튼은 모서리와 동심입니다: 안쪽 22, 높이 44.
        let last = f.buttons[2].0;
        assert!((f.w - last.right() - M).abs() < 0.01 && (f.h - last.bottom() - M).abs() < 0.01);
    }

    #[test]
    fn a_transfer_is_a_pill_with_a_ring_and_its_stop_open_and_a_ring_small() {
        let card = Card {
            kind: Kind::Transfer,
            title: "홍길동의 iPhone에서 받는 중".into(),
            detail: "사진 3장".into(),
            brief: "받는 중".into(),
            progress: Some(Progress {
                fraction: Some(0.42),
                amount: "12 MB / 30 MB".into(),
                status: "8 MB/s".into(),
            }),
            cancel: Some("stop:1".into()),
            ..Card::default()
        };
        let open = face(&card, Mode::Open, &Fixed, None);
        assert!(open.bar.is_none() && (open.h, open.radius) == (MIN_H, MIN_H / 2.0));
        // 링과 그 안의 멈춤은 버튼 하나로, 오른쪽에 앞 항목만큼 높게.
        let ring = open.ring.unwrap();
        assert_eq!(open.buttons, vec![(ring, "stop:1".to_string())]);
        assert!((open.w - ring.right() - M).abs() < 0.01 && (ring.w, ring.h) == (CONTROL, CONTROL));
        assert!(
            open.items
                .iter()
                .any(|i| matches!(i, Item::Text { text, .. } if text == "12 MB / 30 MB · 8 MB/s"))
        );
        // 퍼센트는 링 바로 왼쪽.
        assert!(
            open.items
                .iter()
                .any(|i| matches!(i, Item::Text { text, rect, right: true, .. } if text == "42%" && rect.right() <= ring.x))
        );
        // 기다리는 동안은 퍼센트가 없고 글은 자리를 지킵니다.
        let waiting = Card {
            progress: Some(Progress::default()),
            ..card.clone()
        };
        let w = face(&waiting, Mode::Open, &Fixed, None);
        assert!(!w.items.iter().any(|i| matches!(i, Item::Text { text, .. } if text.ends_with('%'))));
        let title_w = |f: &Face| {
            f.items.iter().find_map(|i| match i {
                Item::Text {
                    rect, style: Style::Title, ..
                } => Some(rect.w),
                _ => None,
            })
        };
        assert_eq!(title_w(&w), title_w(&open));
        let small = face(&card, Mode::Small, &Fixed, None);
        assert!(small.ring.is_some() && small.buttons.is_empty() && small.w >= 230.0 && (small.h - SMALL_H).abs() < 0.01);
        assert!((small.fraction - 0.42).abs() < 1e-6);
    }

    #[test]
    fn no_end_known_spins() {
        let card = Card {
            kind: Kind::Transfer,
            progress: Some(Progress::default()),
            title: "로그인 중".into(),
            ..Card::default()
        };
        assert!(face(&card, Mode::Open, &Fixed, None).spinner.is_some());
        assert!(face(&card, Mode::Small, &Fixed, None).spinner.is_some());
    }

    #[test]
    fn a_field_fits_and_keeps_its_caret_in_the_box() {
        let card = Card {
            kind: Kind::Ask,
            title: "Apple ID 로그인".into(),
            detail: "암호는 이 PC에 저장되지 않습니다".into(),
            field: Some(Field {
                placeholder: "암호".into(),
                secret: true,
                action: "로그인".into(),
                ..Field::default()
            }),
            ..Card::default()
        };
        let shown = "•".repeat(60);
        let typing = Typing {
            caret: 60,
            anchor: 60,
            shown,
            focused: true,
            scroll: 0.0,
        };
        let f = face(&card, Mode::Open, &Fixed, Some(&typing));
        assert!(f.h <= MAX_H, "{}", f.h);
        let (field, caret) = (f.field.unwrap(), f.caret.unwrap());
        assert!(caret.x > field.x && caret.right() < field.right(), "{caret:?} in {field:?}");
        assert_eq!(f.buttons.iter().map(|b| b.1.as_str()).collect::<Vec<_>>(), [CLOSE, SUBMIT]);
        // 키보드가 없으면 캐럿도 없습니다.
        let idle = Typing { focused: false, ..typing };
        assert!(face(&card, Mode::Open, &Fixed, Some(&idle)).caret.is_none());
    }

    #[test]
    fn a_field_keeps_its_scroll_until_the_caret_would_leave_it() {
        let card = Card {
            kind: Kind::Ask,
            title: "Apple ID 로그인".into(),
            field: Some(Field {
                placeholder: "Apple ID".into(),
                action: "다음".into(),
                ..Field::default()
            }),
            ..Card::default()
        };
        let shown = "x".repeat(60);
        let at = |caret: usize, anchor: usize, scroll: f32| {
            let t = Typing {
                shown: shown.clone(),
                caret,
                anchor,
                scroll,
                focused: true,
            };
            face(&card, Mode::Open, &Fixed, Some(&t))
        };
        // 끝에서는 끝이 보이게 스크롤합니다.
        let end = at(60, 60, 0.0);
        let s = end.field_scroll;
        assert!(s > 0.0);
        // 조금 왼쪽을 눌렀습니다: 글은 그 자리에 있습니다.
        assert_eq!(at(55, 55, s).field_scroll, s);
        // 왼쪽 가장자리를 넘어갔습니다: 캐럿까지 되돌아 스크롤합니다.
        assert_eq!(at(2, 2, s).field_scroll, 14.0);
        // 선택이 있으면: 캐럿 없이 글을 따라 선택한 구간.
        let f = at(10, 4, 0.0);
        assert!(f.caret.is_none());
        assert!(
            f.items
                .iter()
                .any(|i| matches!(i, Item::Field { selection: Some((a, b)), .. } if (*a, *b) == (28.0, 70.0)))
        );
    }

    #[test]
    fn a_long_reason_over_a_button_takes_two_lines() {
        let card = Card {
            kind: Kind::Result,
            tint: Tint::Red,
            title: "Apple ID에 로그인하지 못했습니다".into(),
            detail: "Apple 서버에 연결하지 못했습니다. 인터넷 연결을 확인하고 잠시 뒤 다시 시도해 주세요.".into(),
            buttons: vec![Button::new("다시 로그인", "signin:new", true)],
            ..Card::default()
        };
        let f = face(&card, Mode::Open, &Fixed, None);
        assert!(f.items.iter().any(|i| matches!(
            i,
            Item::Text {
                style: Style::Body,
                lines: 2,
                ..
            }
        )));
        assert!(f.h <= MAX_H, "{}", f.h);
    }

    #[test]
    fn text_alone_is_one_pill_however_many_lines() {
        let card = |status: &str, detail: &str| Card {
            kind: Kind::Result,
            title: "홍길동의 iPhone에서 받지 못했습니다".into(),
            detail: detail.into(),
            status: status.into(),
            ..Card::default()
        };
        let long = "user@example.com · iPhone과 iPad에 이 PC가 내 기기로 보입니다.";
        for c in [card("", "사진 3장"), card("연결이 끊겼습니다.", "사진 3장"), card("", long)] {
            let f = face(&c, Mode::Open, &Fixed, None);
            assert_eq!((f.h, f.radius), (MIN_H, MIN_H / 2.0), "{:?}", c.status);
            // 모든 것이 위아래로 PILL_INSET 안쪽에.
            for item in &f.items {
                if let Item::Text { rect, .. } = item {
                    assert!(rect.y >= PILL_INSET - 0.01 && rect.bottom() <= MIN_H - PILL_INSET + 1.01, "{rect:?}");
                }
            }
        }
        // 아래에 상태 줄이 없는 긴 설명은 두 줄을 씁니다.
        let f = face(&card("", long), Mode::Open, &Fixed, None);
        assert!(f.items.iter().any(|i| matches!(i, Item::Text { lines: 2, .. })));
        // 막대나 버튼이 있으면 카드가 됩니다.
        let with = Card {
            buttons: vec![Button::new("열기", "open", true)],
            ..card("", "사진 3장")
        };
        assert!(face(&with, Mode::Open, &Fixed, None).h > MIN_H);
    }
}
