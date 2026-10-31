//! 보내기: 작업을 엔진에 넘기고 상태를 카드로 보여 줍니다. 기기를 아직 안 골랐으면 마우스 자리에 기기 메뉴를 엽니다.
use super::*;

impl Hub {
    fn nothing_to_send(&self) {
        self.notify(
            "send",
            t!("보낼 파일이 없습니다", "Nothing to send"),
            t!("보낼 수 있는 파일이나 폴더가 없습니다.", "There are no files or folders to send."),
            &[],
        );
    }

    pub(super) fn new_job(&mut self, target: TargetRef, files: Vec<PathBuf>) {
        let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file() || f.is_dir()).collect();
        if files.is_empty() {
            self.nothing_to_send();
            return;
        }
        let job = self.next_job;
        self.next_job += 1;
        let model = self.targets.values().find(|t| t.name == target.name).and_then(|t| t.model.clone());
        self.settings.note_recent(&target.name, model);
        self.save();
        tracing::info!("job {job}: {} file(s) to {}", files.len(), target.name);
        let mut j = Job {
            target,
            files,
            state: SendState::Queued,
            rate: Rate::default(),
            accepted: None,
            bytes: 0,
        };
        if self.phase == Phase::Ready && self.engine.is_some() {
            self.to_engine(Msg::Send {
                job,
                target: j.target.clone(),
                files: j.files.clone(),
            });
            j.state = SendState::Waiting;
        } else {
            self.ensure_engine();
        }
        if let Some(c) = &self.cards {
            c.send_state(job, &j.target.name, &j.files, &j.state, None, "");
        }
        self.jobs.insert(job, j);
        while self.jobs.len() > 30 {
            let Some((&oldest, _)) = self.jobs.iter().find(|(_, j)| j.state.is_final()) else {
                break;
            };
            self.jobs.remove(&oldest);
        }
        self.push();
    }

    pub(super) fn flush_jobs(&mut self) {
        let queued: Vec<u64> = self.jobs.iter().filter(|(_, j)| j.state == SendState::Queued).map(|(k, _)| *k).collect();
        for job in queued {
            if let Some(j) = self.jobs.get_mut(&job) {
                j.state = SendState::Waiting;
                let msg = Msg::Send {
                    job,
                    target: j.target.clone(),
                    files: j.files.clone(),
                };
                self.to_engine(msg);
            }
        }
    }

    pub(super) fn job_update(&mut self, job: u64, state: SendState) {
        let Some(j) = self.jobs.get_mut(&job) else { return };
        if j.state.is_final() {
            return;
        }
        let mut speed = None;
        match &state {
            SendState::Accepted => j.accepted = Some(Instant::now()),
            SendState::Progress { sent, total } => {
                j.rate.add(*sent);
                j.bytes = *total;
                speed = j.rate.speed();
            }
            _ => {}
        }
        let summary = if state == SendState::Delivered {
            let bytes = if j.bytes > 0 { j.bytes } else { crate::paths::total_size(&j.files) };
            rate::summary(bytes, j.accepted.map(|t| t.elapsed()).unwrap_or_default())
        } else {
            String::new()
        };
        if let Some(c) = &self.cards {
            c.send_state(job, &j.target.name, &j.files, &state, speed, &summary);
        }
        let ended = match &state {
            SendState::Failed { reason } => Some((
                tf!("{}에 보내지 못했습니다", "Couldn't send to {}", j.target.name),
                text::reason_text(reason),
            )),
            SendState::Declined => Some((tf!("{}에서 거절했습니다", "{} declined", j.target.name), String::new())),
            _ => None,
        };
        j.state = state;
        if let Some((title, why)) = ended {
            self.notice(&format!("send-{job}-end"), menu::WARNING_GLYPH, &title, &why, &[]);
        }
        self.push();
    }

    pub(super) fn cancel(&mut self, job: u64) {
        let Some(j) = self.jobs.get_mut(&job) else { return };
        if j.state == SendState::Queued || self.engine.is_none() {
            j.state = SendState::Cancelled;
            if let Some(c) = &self.cards {
                c.remove(&format!("send-{job}"));
            }
            self.push();
        } else {
            self.to_engine(Msg::Cancel { job });
        }
    }

    /// 보이는 받는 기기 먼저, 그다음 잠들어 있을지 모를 최근 기기.
    pub(super) fn devices(&self) -> Vec<Device> {
        let mut list: Vec<Device> = Vec::new();
        for t in self.targets.values() {
            if !list.iter().any(|d| d.name == t.name) {
                list.push(Device {
                    id: Some(t.id.clone()),
                    name: t.name.clone(),
                    model: t.model.clone(),
                    visible: true,
                });
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        for r in &self.settings.recent {
            if !list.iter().any(|d| d.name.eq_ignore_ascii_case(&r.name)) {
                list.push(Device {
                    id: None,
                    name: r.name.clone(),
                    model: r.model.clone(),
                    visible: false,
                });
            }
        }
        list
    }

    /// 아직 기기를 고르지 않은 보낼 파일: 찾기를 시작하고 마우스 자리에 기기 메뉴를 엽니다.
    /// 메뉴에는 주변 기기가 오가는 대로 뜨고, 하나를 고르면 보냅니다.
    pub(super) fn share(&mut self, files: Vec<PathBuf>) {
        // 디스크에 없는 폴더(홈)의 빈 곳에서 고른 탐색기 AirDrop은 아무것도 넘기지 않습니다: 그때는
        // 보낼 파일을 묻습니다. 디스크에 있는 폴더면 그 폴더를 통째로 넘깁니다.
        if files.is_empty() {
            self.choose(None);
            return;
        }
        let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file() || f.is_dir()).collect();
        if files.is_empty() {
            self.nothing_to_send();
            return;
        }
        self.wake();
        self.ui.share(files);
    }

    /// 보낼 파일을 Windows 파일 대화상자로 folder에서 묻고 (따로 스레드에서), 그다음 마우스 자리에
    /// 기기 메뉴를 엽니다. 찾기는 지금 시작하므로 그때쯤 기기가 들어와 있습니다.
    pub(super) fn choose(&mut self, folder: Option<PathBuf>) {
        self.wake();
        let events = self.events.clone();
        std::thread::spawn(move || {
            let files = crate::win::pick_in(
                0,
                t!("AirDrop으로 보낼 파일", "Files to send with AirDrop"),
                crate::win::Pick::Files,
                folder.as_deref(),
            );
            if !files.is_empty() {
                let _ = events.send(HubEvent::Menu(MenuCmd::SendHere(files)));
            }
        });
    }
}
