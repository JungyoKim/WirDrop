//! 받기: 요청을 묻고(또는 규칙대로 답하고), 받는 동안 카드를 움직이고, 끝을 알립니다.
use super::*;

impl Hub {
    pub(super) fn incoming(&mut self, req: IncomingInfo) {
        let id = req.id;
        let decision = if req.own {
            Some(true)
        } else {
            self.settings.note_person(&req.person, &req.sender, &req.model, req.apple_id);
            self.save();
            match self.settings.person(&req.person).map(|p| p.rule).unwrap_or_default() {
                Rule::Accept => Some(true),
                Rule::Block => Some(false),
                Rule::Default => None,
            }
        };
        tracing::info!("request {id} from {} (own: {}): {decision:?}", req.sender, req.own);
        match decision {
            Some(accept) => {
                self.to_engine(Msg::Answer { id, accept });
                if accept {
                    // 카드를 지금 띄웁니다: 보내는 쪽이 파일을 준비하는 동안 사용자는 이미
                    // 기다렸습니다 (74 MB 영상에 iPhone 7이 14초).
                    let mut ask = Ask::new(req, None);
                    ask.card = Some(Instant::now());
                    let links = ask.info.files.iter().all(|f| f.kind == "url");
                    if let Some(c) = &self.cards
                        && !links
                    {
                        c.receiving(&ask.info);
                    }
                    self.asks.insert(id, ask);
                }
            }
            None => {
                if let Some(c) = &self.cards {
                    c.ask(&req);
                }
                // 아일랜드가 없으면 트레이 아이콘의 메뉴가 대신 묻습니다.
                self.offer(&format!("recv-{id}"), menu::ask_entries(&req));
                self.asks.insert(id, Ask::new(req, Some(Instant::now())));
                self.push();
            }
        }
    }

    pub(super) fn answer(&mut self, id: u64, accept: bool, always: bool) {
        let Some(ask) = self.asks.get_mut(&id) else { return };
        if ask.asked.take().is_none() {
            return;
        }
        // 아이콘 메뉴에 질문이 떠 있었다면 답했으니 닫습니다.
        self.ui.close_entries();
        if always {
            let key = ask.info.person.clone();
            self.settings.set_rule(&key, Rule::Accept);
            self.save();
        }
        self.to_engine(Msg::Answer { id, accept });
        if accept {
            if let Some(ask) = self.asks.get_mut(&id) {
                ask.card = Some(Instant::now());
                // 링크의 질문 카드는 마지막 카드([Cards::link])가 바꿀 때까지 둡니다.
                if let Some(c) = &self.cards
                    && !ask.info.files.iter().all(|f| f.kind == "url")
                {
                    c.receiving(&ask.info);
                }
            }
        } else {
            self.asks.remove(&id);
            if let Some(c) = &self.cards {
                c.remove(&format!("recv-{id}"));
            }
        }
        self.push();
    }

    pub(super) fn receive_progress(&mut self, id: u64, done: u64) {
        let Some(ask) = self.asks.get_mut(&id) else { return };
        ask.rate.add(done);
        if ask.drawn.is_some_and(|t| t.elapsed() < DRAW_EVERY) {
            return;
        }
        ask.drawn = Some(Instant::now());
        if let Some(c) = &self.cards {
            c.receive_progress(id, rate::bar(done, ask.total(), ask.rate.speed(), t!("받는 중…", "Receiving…")));
        }
    }

    pub(super) fn received(&mut self, id: u64, paths: Vec<PathBuf>) {
        let ask = self.asks.remove(&id);
        let sender = ask.as_ref().map(|a| a.info.sender.clone()).unwrap_or_else(|| "iPhone".into());
        tracing::info!("received {} file(s) from {sender}", paths.len());
        let bytes = match ask.as_ref().map(Ask::total) {
            Some(n) if n > 0 => n,
            _ => paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum(),
        };
        let card = ask.as_ref().and_then(|a| a.card);
        let summary = rate::summary(bytes, card.map(|t| t.elapsed()).unwrap_or_default());
        let icon = ask.and_then(|a| a.info.icon);
        if let Some(c) = &self.cards {
            c.received(id, &sender, &paths, icon.as_deref(), &summary);
        }
        if let Some(first) = paths.first() {
            let open = if paths.len() > 1 { first.parent().unwrap_or(first) } else { first };
            let (open, reveal) = (format!("open:{}", open.display()), format!("reveal:{}", first.display()));
            let what = text::file_line(&paths);
            let buttons = [(t!("열기", "Open"), open.as_str()), (t!("폴더 열기", "Show in Folder"), reveal.as_str())];
            self.notice(
                &format!("recv-{id}-end"),
                menu::RECEIVE_GLYPH,
                &tf!("{sender}에서 받았습니다", "Received from {sender}"),
                &what,
                &buttons,
            );
        }
        // 붙여 넣을 수 있게 클립보드에도. 그림을 푸느라 카드와 알림이 늦지 않도록 따로 돕니다.
        if self.settings.clipboard {
            crate::clipboard::copy_received(self.ui.hwnd(), paths.clone());
        }
        if self.open_when_done.remove(&id) {
            open_files(&paths);
        }
        self.received_files.insert(id, paths);
        while self.received_files.len() > 50 {
            self.received_files.pop_first();
        }
        self.push();
    }

    /// 요청 id의 받기를 멈춥니다 (카드의 ■): 엔진이 연결을 끊으면 그때까지 받은 것도 함께 사라지고
    /// 카드도 내려갑니다.
    pub(super) fn cancel_receive(&mut self, id: u64) {
        if self.asks.remove(&id).is_none() {
            return;
        }
        tracing::info!("request {id}: receiving cancelled");
        self.to_engine(Msg::CancelReceive { id });
        self.open_when_done.remove(&id);
        if let Some(c) = &self.cards {
            c.remove(&format!("recv-{id}"));
        }
        self.push();
    }
}

/// 파일 하나면 그 앱으로 열고, 여럿이면 폴더를 엽니다.
pub(super) fn open_files(paths: &[PathBuf]) {
    let Some(first) = paths.first() else { return };
    let target = if paths.len() > 1 { first.parent().unwrap_or(first) } else { first };
    crate::win::open(&target.display().to_string());
}
