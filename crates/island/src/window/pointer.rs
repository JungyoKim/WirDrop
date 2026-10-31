//! 마우스: 올려놓기, 누르기, 카드 열기, 오른쪽 클릭으로 치우기.
use super::*;

/// 마우스 메시지의 점. 창 왼쪽 위에서의 pt.
pub(super) fn point(lparam: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lparam.0 & 0xffff) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
    (x / scale, y / scale)
}

impl App {
    pub(super) fn button_at(&self, x: f32, y: f32) -> Option<usize> {
        let top = self.scene.top()?;
        let (fx, fy) = (x - top.origin.X, y - top.origin.Y);
        top.face.buttons.iter().position(|(r, _)| r.contains(fx, fy))
    }

    pub(super) fn mouse_move(&mut self, (x, y): (f32, f32), now: Instant) {
        let inside = self.island_rect(now).inflate(2.0).contains(x, y);
        if inside && !self.hover {
            self.hover = true;
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            self.reconcile(now);
        } else if !inside && self.hover {
            self.mouse_leave(now);
            return;
        }
        let hot = if inside { self.button_at(x, y) } else { None };
        if hot != self.hot {
            self.hot = hot;
            let _ = self.scene.hover(hot, self.pressed.is_some() && self.pressed == hot);
        }
    }

    pub(super) fn mouse_leave(&mut self, now: Instant) {
        if !self.hover {
            return;
        }
        self.hover = false;
        self.hot = None;
        self.pressed = None;
        let _ = self.scene.hover(None, false);
        self.linger(now);
    }

    /// 마우스나 키보드가 떠난 뒤: 떠 있던 카드는 조금 더 머물다가 제 일정대로 간다.
    pub(super) fn linger(&mut self, now: Instant) {
        // 떠 있던 것이 잠깐 더 머뭅니다.
        if let Some(up) = &self.up
            && let Some(e) = self.cards.iter_mut().find(|e| e.card.tag == up.tag)
        {
            if let Some(t) = &mut e.ends {
                *t = (*t).max(now + Duration::from_millis(2500));
            }
            if e.card.kind == Kind::Transfer && up.mode == Mode::Open {
                e.open_until = Some(e.open_until.unwrap_or(now).max(now + GRACE));
            }
        }
        self.reconcile(now);
    }

    /// 떠 있는 카드의 버튼 b를 눌렀습니다: 클릭, 또는 화면 읽기 프로그램의 Invoke.
    pub(super) fn press(&mut self, b: usize, now: Instant) {
        let Some(up) = &self.up else { return };
        let Some(i) = self.cards.iter().position(|e| e.card.tag == up.tag) else {
            return;
        };
        let Some(args) = self.scene.top().and_then(|t| t.face.buttons.get(b)).map(|(_, a)| a.clone()) else {
            return;
        };
        if args == layout::SUBMIT || args == layout::CLOSE {
            return self.answer(args == layout::SUBMIT, now);
        }
        let kind = self.cards[i].card.kind;
        if self.cards[i].acted && kind == Kind::Ask {
            return;
        }
        self.cards[i].acted = true;
        if matches!(kind, Kind::Result | Kind::Info) {
            self.cards.remove(i);
        }
        (self.on_action)(args);
        self.reconcile(now);
    }

    pub(super) fn click(&mut self, (x, y): (f32, f32), now: Instant) {
        let pressed = self.pressed.take();
        let _ = self.scene.hover(self.hot, false);
        let Some(up) = &self.up else { return };
        let Some(i) = self.cards.iter().position(|e| e.card.tag == up.tag) else {
            return;
        };
        let hit = self.button_at(x, y);
        if let Some(b) = hit.filter(|b| pressed == Some(*b)) {
            return self.press(b, now);
        }
        if hit.is_some() || !self.island_rect(now).contains(x, y) {
            return;
        }
        // 입력칸 안의 클릭은 입력칸의 것입니다 (누를 때 이미 캐럿을 거기 두었습니다).
        if self.field_at(x, y) {
            return;
        }
        // 카드 자체: 작으면 열고, 열려 있으면 그 launch를 실행하며 결과 카드는 함께 사라집니다.
        let mode = up.mode;
        let e = &mut self.cards[i];
        if mode == Mode::Small {
            e.open_until = Some(now + PEEK_FOR);
        } else if let Some(launch) = e.card.launch.clone() {
            if matches!(e.card.kind, Kind::Result | Kind::Info) {
                self.cards.remove(i);
            }
            (self.on_action)(launch);
        }
        self.reconcile(now);
    }

    /// 오른쪽 클릭은 떠 있는 것을 치웁니다: 결과나 알림은 사라지고, 전송은 작아집니다.
    pub(super) fn dismiss(&mut self, now: Instant) {
        let Some(up) = &self.up else { return };
        let Some(i) = self.cards.iter().position(|e| e.card.tag == up.tag) else {
            return;
        };
        match self.cards[i].card.kind {
            Kind::Result | Kind::Info => {
                self.cards.remove(i);
            }
            Kind::Transfer => self.cards[i].open_until = None,
            Kind::Ask => return,
        }
        self.hover = false;
        self.reconcile(now);
    }
}
