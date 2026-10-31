//! 키보드: 질문의 입력칸, 단축키(Win+Alt+A)로 버튼 고르기, 포커스를 가져오고 돌려주기.
use super::*;

/// Shift를 누르고 있습니다.
pub(super) fn shift() -> bool {
    unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 }
}

impl App {
    /// 입력한 글자 (WM_CHAR, UTF-16): Enter는 보내고, Esc는 닫고, Ctrl+A는 모두 선택, Ctrl+V는
    /// 붙여넣기.
    pub(super) fn typed(&mut self, unit: u16, now: Instant) {
        // 키보드로 버튼을 고르는 중엔 글자가 입력칸으로 가지 않는다 (Esc만 질문을 닫는다).
        if std::mem::take(&mut self.skip_char) || (self.nav.is_some() && unit != 0x1B) {
            return;
        }
        let Some(i) = self.asking_index() else { return };
        match unit {
            0x0D => return self.answer(true, now),
            0x1B => return self.answer(false, now),
            _ => {}
        }
        let mut pasted = if unit == 0x16 { clipboard(self.hwnd) } else { None };
        let Some(input) = self.cards[i].input.as_mut() else { return };
        match unit {
            0x08 => input.back(false),
            0x7F => input.back(true),
            0x01 => input.select_all(),
            0x16 => match &mut pasted {
                Some(text) => {
                    input.insert(text);
                    wipe(text);
                }
                None => return,
            },
            0xD800..=0xDBFF => {
                input.high = Some(unit);
                return;
            }
            0xDC00..=0xDFFF => match input.high.take().and_then(|high| char::decode_utf16([high, unit]).next()?.ok()) {
                Some(c) => input.insert(c.encode_utf8(&mut [0; 4])),
                None => return,
            },
            u if u < 0x20 => return,
            u => match char::from_u32(u32::from(u)) {
                Some(c) => input.insert(c.encode_utf8(&mut [0; 4])),
                None => return,
            },
        }
        self.edited(i, now);
    }

    /// 캐럿 키들. 나머지는 false.
    pub(super) fn key(&mut self, key: VIRTUAL_KEY, now: Instant) -> bool {
        let Some(i) = self.asking_index() else { return false };
        let Some(input) = self.cards[i].input.as_mut() else { return false };
        let n = input.len();
        let extend = shift();
        let (a, b) = input.selection();
        match key {
            // 선택이 있으면 화살표는 그쪽 끝으로 갑니다.
            VK_LEFT if a != b && !extend => input.go(a, false),
            VK_RIGHT if a != b && !extend => input.go(b, false),
            VK_LEFT => input.go(input.caret.saturating_sub(1), extend),
            VK_RIGHT => input.go(input.caret + 1, extend),
            VK_HOME => input.go(0, extend),
            VK_END => input.go(n, extend),
            VK_DELETE => input.delete(),
            _ => return false,
        }
        self.edited(i, now);
        true
    }

    /// 입력칸의 x(창 기준 pt)에, 그 자리의 글에서 캐럿이 갈 곳에 캐럿을 둡니다. extend면 선택이
    /// 거기까지 늘어납니다. 입력칸이 없으면 false.
    pub(super) fn place_caret(&mut self, x: f32, extend: bool, now: Instant) -> bool {
        let Some(i) = self.asking_index() else { return false };
        let Some((origin, inner)) = self.scene.top().and_then(|t| Some((t.origin.X, t.face.field_text?))) else {
            return false;
        };
        let secret = self.cards[i].card.field.as_ref().is_some_and(|f| f.secret);
        let Some(input) = self.cards[i].input.as_ref() else { return false };
        let shown = input.typing(secret, false).shown;
        let at = self.gfx.index_at(&shown, Style::Field, x - origin - inner.x + input.scroll);
        let Some(input) = self.cards[i].input.as_mut() else { return false };
        let anchor = if extend { input.anchor } else { at };
        if (input.caret, input.anchor) == (at, anchor) {
            return true;
        }
        input.go(at, extend);
        self.edited(i, now);
        true
    }

    pub(super) fn select_all(&mut self, now: Instant) {
        let Some(i) = self.asking_index() else { return };
        if let Some(input) = self.cards[i].input.as_mut() {
            input.select_all();
            self.edited(i, now);
        }
    }

    /// 떠 있는 질문에 답했습니다 (typed: 보냄, 아니면 닫음). 그사이 같은 태그로 온 것이 자리를
    /// 차지하고, 없으면 보낸 질문은 다음 것을 위해 잠깐 머물고 닫은 질문은 사라집니다.
    pub(super) fn answer(&mut self, typed: bool, now: Instant) {
        let Some(i) = self.asking_index() else { return };
        let e = &mut self.cards[i];
        let Some(input) = e.input.as_mut() else { return };
        let Some(reply) = input.reply.take() else { return };
        input.go(input.caret, false);
        let _ = reply.send(if typed { Answer::Typed(input.text.clone()) } else { Answer::Closed });
        match e.after.take() {
            Some(card) => {
                (e.open_until, e.ends) = timing(&card, now);
                e.card = card;
                e.input = None;
                self.version += 1;
                e.seq = self.version;
                self.chime_due |= e.ring();
            }
            None if typed => e.ends = Some(now + ANSWERED_FOR),
            None => {
                self.cards.remove(i);
                return self.reconcile(now);
            }
        }
        self.edited(i, now);
    }

    /// 점(pt)이 떠 있는 카드의 입력칸 안에 있습니다.
    pub(super) fn field_at(&self, x: f32, y: f32) -> bool {
        self.scene
            .top()
            .and_then(|t| t.face.field.map(|f| f.contains(x - t.origin.X, y - t.origin.Y)))
            .unwrap_or(false)
    }

    /// 질문이 떠 있는 동안 (또는 단축키로 버튼에 왔을 때) 키보드를 가져오고, 끝나면 돌려줍니다.
    pub(super) fn keyboard(&mut self, now: Instant) {
        let buttons = self.up.as_ref().is_some_and(|u| u.mode == Mode::Open && !u.face.buttons.is_empty());
        let wanted = self.visible && (self.asking_index().is_some() || (self.navigating && buttons));
        match (wanted, self.keys) {
            (true, None) => {
                self.take_keys();
                // 키보드가 왔으면 캐럿과 함께 다시 그립니다 (두 번째에는 키를 이미 가져온 상태라
                // 되풀이되지 않습니다).
                if self.focused
                    && let Some(i) = self.asking_index()
                {
                    self.edited(i, now);
                }
            }
            (false, Some(before)) => self.give_keys(before),
            _ => {}
        }
    }

    /// 마우스가 올라와 있거나 키보드로 다니는 중: 떠 있는 카드가 그대로 머문다.
    pub(super) fn held(&self) -> bool {
        self.hover || self.navigating
    }

    /// 단축키(Win+Alt+A)는 아일랜드가 떠 있는 동안만 등록한다. 다른 앱이 이미 쓰고 있으면 등록되지
    /// 않고, 그때는 마우스와 화면 읽기 프로그램으로만 고를 수 있다.
    pub(super) fn sync_hotkey(&mut self) {
        let want = self.visible && self.up.is_some();
        if want == self.hotkey {
            return;
        }
        // SAFETY: 이 스레드의 창에 단축키를 걸고 푼다.
        unsafe {
            if !want {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY);
                self.hotkey = false;
            } else if let Err(e) = RegisterHotKey(Some(self.hwnd), HOTKEY, MOD_WIN | MOD_ALT | MOD_NOREPEAT, HOTKEY_KEY) {
                tracing::debug!("island: Win+Alt+A is taken: {e}");
            } else {
                self.hotkey = true;
            }
        }
    }

    /// 단축키를 눌렀다: 떠 있는 카드로 키보드를 가져온다. 작게 떠 있으면 펼치고, 버튼이 있으면
    /// 기본 버튼(없으면 첫 버튼)에 포커스 링을 두른다. 입력칸이 있으면 입력칸으로 간다.
    pub(super) fn summon(&mut self, now: Instant) {
        let Some(up) = &self.up else { return };
        if up.mode == Mode::Small
            && let Some(e) = self.cards.iter_mut().find(|e| e.card.tag == up.tag)
        {
            e.open_until = Some(now + PEEK_FOR);
            self.reconcile(now);
        }
        let Some(up) = &self.up else { return };
        let asking = self.asking_index().is_some();
        if up.face.buttons.is_empty() && !asking {
            return;
        }
        let primary = self
            .cards
            .iter()
            .find(|e| e.card.tag == up.tag)
            .and_then(|e| e.card.buttons.iter().find(|b| b.primary))
            .map(|b| b.args.as_str());
        let first = up.face.buttons.iter().position(|(_, args)| Some(args.as_str()) == primary).unwrap_or(0);
        self.navigating = !asking;
        if self.keys.is_none() {
            self.take_keys();
        } else {
            // SAFETY: 우리 창을 앞으로 (단축키를 받은 프로세스는 그럴 수 있다).
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                let _ = SetFocus(Some(self.hwnd));
            }
            self.note_focus();
        }
        if !self.focused {
            tracing::info!("island: Windows kept the keyboard where it was");
            self.navigating = false;
            self.keyboard(now);
            return;
        }
        if asking {
            self.set_nav(None, now);
            // 입력칸이 있는 질문: 키보드가 왔으니 캐럿이 보이게 다시 그린다.
            if let Some(i) = self.asking_index() {
                self.edited(i, now);
            }
        } else {
            self.set_nav(Some(first), now);
        }
    }

    /// 키보드로 카드 안을 다닌다: Tab과 Shift+Tab은 입력칸과 버튼을 돌고, ←→는 버튼 사이를
    /// 옮기고, Enter나 Space는 고른 버튼을 누르고, Esc는 키보드를 원래 창에 돌려준다.
    /// 처리한 키면 true.
    pub(super) fn navigate(&mut self, key: VIRTUAL_KEY, now: Instant) -> bool {
        if self.keys.is_none() {
            return false;
        }
        let Some(n) = self.up.as_ref().filter(|u| u.mode == Mode::Open).map(|u| u.face.buttons.len()) else {
            return false;
        };
        let field = self.asking_index().is_some() && self.up.as_ref().is_some_and(|u| u.face.field.is_some());
        match key {
            VK_TAB => {
                // 입력칸(있으면 0번)과 버튼들이 한 바퀴를 이룬다.
                let f = usize::from(field);
                let stops = n + f;
                if stops == 0 {
                    return false;
                }
                let at = if field { Some(self.nav.map_or(0, |b| b + 1)) } else { self.nav };
                let next = match (at, shift()) {
                    (Some(p), false) => (p + 1) % stops,
                    (Some(p), true) => (p + stops - 1) % stops,
                    (None, false) => 0,
                    (None, true) => stops - 1,
                };
                self.skip_char = true;
                self.set_nav(if field && next == 0 { None } else { Some(next - f) }, now);
                true
            }
            VK_LEFT | VK_RIGHT if self.nav.is_some() && n > 0 => {
                let b = self.nav.unwrap_or(0);
                let to = if key == VK_LEFT { (b + n - 1) % n } else { (b + 1) % n };
                self.set_nav(Some(to), now);
                true
            }
            VK_RETURN | VK_SPACE if self.nav.is_some() => {
                self.skip_char = true;
                if let Some(b) = self.nav {
                    self.press(b, now);
                }
                true
            }
            VK_ESCAPE if self.navigating => {
                self.skip_char = true;
                self.navigating = false;
                self.set_nav(None, now);
                self.linger(now);
                true
            }
            _ => false,
        }
    }

    /// 키보드로 고른 버튼을 바꾼다 (None: 입력칸 또는 없음). 입력칸의 캐럿도 그에 맞춰 보이거나 숨는다.
    pub(super) fn set_nav(&mut self, nav: Option<usize>, now: Instant) {
        if self.nav == nav {
            return;
        }
        self.nav = nav;
        tracing::debug!("island: keyboard on {nav:?}");
        let _ = self.scene.focus(nav);
        match self.asking_index() {
            Some(i) => self.edited(i, now),
            None => self.publish(),
        }
    }

    /// 입력한 키가 여기로 오는지. 우리 SetFocus 안에서 오는 WM_SETFOCUS는 그렇다고 말해 주지
    /// 못하므로 (그때 창은 바빠서 그것을 버립니다) Windows에 직접 묻습니다.
    pub(super) fn note_focus(&mut self) {
        self.focused = unsafe { GetForegroundWindow() == self.hwnd && GetFocus() == self.hwnd };
    }

    /// 입력한 키가 여기로 오는지 다시 봅니다 (may: 포커스가 방금 떠난 게 아니면). 바뀌었으면
    /// 입력칸의 캐럿을 그리거나 지웁니다.
    pub(super) fn refocus(&mut self, may: bool, now: Instant) {
        let before = self.focused;
        if may {
            self.note_focus();
        } else {
            self.focused = false;
        }
        if self.focused != before {
            tracing::debug!("island: focused {}", self.focused);
            if let Some(i) = self.asking_index() {
                self.edited(i, now);
            }
        }
        // 키보드로 다니다가 다른 창을 눌렀다: 링을 걷고 키보드를 돌려준다.
        if before && !self.focused && (self.navigating || self.nav.is_some()) {
            self.navigating = false;
            self.set_nav(None, now);
            self.keyboard(now);
        }
    }

    /// 창이 포커스(다른 때는 절대 받지 않음)와 전경을 가져옵니다: 질문이 WirDrop을 누른 뒤(메뉴)에
    /// 오면 Windows가 허락하고, 아니면 섬을 누르면 가져옵니다.
    pub(super) fn take_keys(&mut self) {
        unsafe {
            let before = GetForegroundWindow();
            self.keys = Some(if before == self.hwnd { 0 } else { before.0 as isize });
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
            if !SetForegroundWindow(self.hwnd).as_bool() {
                tracing::info!("island: Windows kept the keyboard where it was; a click on the island takes it");
            }
            let _ = SetFocus(Some(self.hwnd));
        }
        self.note_focus();
    }

    /// 다시 포커스를 받지 않는 상태로, 전경은 원래 창에 돌려줍니다.
    pub(super) fn give_keys(&mut self, before: isize) {
        self.keys = None;
        self.focused = false;
        self.navigating = false;
        self.nav = None;
        let _ = self.scene.focus(None);
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            let before = HWND(before as _);
            if GetForegroundWindow() == self.hwnd && !before.is_invalid() && IsWindow(Some(before)).as_bool() {
                let _ = SetForegroundWindow(before);
            }
        }
    }
}
