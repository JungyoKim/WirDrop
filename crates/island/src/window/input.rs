//! 섬의 한 줄 입력칸: 입력, 선택, 캐럿, 클립보드. 입력한 비밀은 사본을 남기지 않습니다.
use super::*;

/// 입력 중인 질문의 입력칸. 버퍼는 커지지 않고 (그래서 입력한 것의 사본이 남지 않습니다) 입력칸이
/// 사라질 때 지웁니다.
pub(super) struct Input {
    pub(super) text: String,
    /// text 안의 글자 위치.
    pub(super) caret: usize,
    /// 선택의 다른 끝 (선택이 없으면 캐럿과 같음): 다음에 입력하는 것이 둘 사이를 바꿉니다.
    pub(super) anchor: usize,
    /// 칸 안에서 글이 왼쪽으로 스크롤된 정도(pt). 캐럿이 움직일 때 맞춥니다.
    pub(super) scroll: f32,
    /// 답이 갈 곳. 답하면 None.
    pub(super) reply: Option<Sender<Answer>>,
    /// 아래 반쪽을 기다리는 UTF-16 상위 대리 문자.
    pub(super) high: Option<u16>,
}

impl Input {
    pub(super) fn new(value: &str, reply: Sender<Answer>) -> Input {
        let mut text = String::with_capacity(MAX_TYPED * 4);
        text.extend(value.chars().take(MAX_TYPED));
        let end = text.chars().count();
        Input {
            caret: end,
            anchor: end,
            scroll: 0.0,
            text,
            reply: Some(reply),
            high: None,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// 글자 위치의 바이트 위치.
    pub(super) fn at(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    pub(super) fn clear(&mut self) {
        wipe(&mut self.text);
        self.caret = 0;
        self.anchor = 0;
    }

    /// 글자 단위의 선택 (비었으면 선택 없음).
    pub(super) fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// 선택한 것을 빼냅니다. 선택이 없으면 false.
    pub(super) fn cut(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        if (a, b) == (0, self.len()) {
            self.clear();
        } else {
            let (x, y) = (self.at(a), self.at(b));
            self.text.replace_range(x..y, "");
        }
        (self.caret, self.anchor) = (a, a);
        true
    }

    pub(super) fn insert(&mut self, s: &str) {
        self.cut();
        let room = MAX_TYPED.saturating_sub(self.len());
        for c in s.chars().filter(|c| !c.is_control()).take(room) {
            let at = self.at(self.caret);
            self.text.insert(at, c);
            self.caret += 1;
        }
        self.anchor = self.caret;
    }

    /// Backspace. Ctrl+Backspace는 캐럿 앞의 모든 것을 지웁니다.
    pub(super) fn back(&mut self, all_before: bool) {
        if self.cut() {
            return;
        }
        let from = if all_before { 0 } else { self.caret.saturating_sub(1) };
        let (a, b) = (self.at(from), self.at(self.caret));
        self.text.replace_range(a..b, "");
        (self.caret, self.anchor) = (from, from);
    }

    pub(super) fn delete(&mut self) {
        if self.cut() {
            return;
        }
        if self.caret < self.len() {
            let (a, b) = (self.at(self.caret), self.at(self.caret + 1));
            self.text.replace_range(a..b, "");
        }
    }

    /// 캐럿을 옮깁니다. extend(Shift, 끌기)면 선택의 다른 끝은 그대로 둡니다.
    pub(super) fn go(&mut self, to: usize, extend: bool) {
        self.caret = to.min(self.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub(super) fn select_all(&mut self) {
        (self.anchor, self.caret) = (0, self.len());
    }

    pub(super) fn typing(&self, secret: bool, focused: bool) -> Typing {
        let shown = if secret { "•".repeat(self.len()) } else { self.text.clone() };
        Typing {
            shown,
            caret: self.caret,
            anchor: self.anchor,
            scroll: self.scroll,
            focused: focused && self.reply.is_some(),
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        wipe(&mut self.text);
    }
}

/// 비밀을 버리기 전에 버퍼 전체를 덮어씁니다.
pub(super) fn wipe(s: &mut String) {
    // SAFETY: 벡터 자신의 포인터로 용량 전체에 0을 쓰고, 길이는 0으로 둡니다.
    unsafe {
        let v = s.as_mut_vec();
        std::ptr::write_bytes(v.as_mut_ptr(), 0, v.capacity());
        v.set_len(0);
    }
}

/// 클립보드의 글: 앞뒤 공백을 뺀 첫 줄.
pub(super) fn clipboard(hwnd: HWND) -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while n < 4096 && *p.add(n) != 0 {
                n += 1;
            }
            let mut all = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            let line = all.lines().next().unwrap_or_default().trim().to_string();
            wipe(&mut all);
            Some(line)
        });
        let _ = CloseClipboard();
        text
    }
}
