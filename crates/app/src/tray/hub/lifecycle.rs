//! 엔진과 USB 어댑터의 수명: 띄우기(UAC), 끊김과 재시도, 어댑터를 꽂고 뺄 때, 1초마다 하는 일.
use super::*;

impl Hub {
    /// 엔진이 돌아야 하는데 안 돌면 (UAC를 거쳐) 띄웁니다.
    pub(super) fn ensure_engine(&mut self) {
        if self.engine.is_some() || self.exiting || self.updating {
            return;
        }
        if self.adapter.is_none() {
            self.phase = Phase::NoAdapter;
            return;
        }
        if self.declined {
            self.phase = Phase::NeedsSetup;
            return;
        }
        if self.launched.is_some_and(|t| t.elapsed() < LAUNCH_FOR) {
            return;
        }
        tracing::info!("starting the engine");
        self.launched = Some(Instant::now());
        self.phase = Phase::Launching;
        let events = self.events.clone();
        tokio::task::spawn_blocking(move || {
            let _ = events.send(HubEvent::Launched(elevate::start_engine()));
        });
    }

    /// 탐색기 메뉴 패키지를 UAC를 거쳐 등록합니다 (COM 서버가 있는 서명 없는 패키지는 관리자가
    /// 등록해야 합니다).
    pub(super) fn register_menu(&mut self) {
        if self.menu_busy {
            return;
        }
        self.menu_busy = true;
        let events = self.events.clone();
        tokio::task::spawn_blocking(move || {
            let result = match elevate::run_elevated("--setup --menu on", true) {
                Ok(0) => Ok(()),
                Ok(code) => Err(tf!(
                    "설정 오류 ({code}). logs\\setup.log를 확인해 주세요.",
                    "Setup error ({code}). See logs\\setup.log."
                )),
                Err(e) if e.raw_os_error() == Some(elevate::ERROR_CANCELLED) => {
                    Err(t!("관리자 권한을 받지 못했습니다.", "Administrator permission wasn't given.").into())
                }
                Err(e) => Err(e.to_string()),
            };
            let _ = events.send(HubEvent::MenuDone(result));
        });
    }

    pub(super) fn menu_done(&mut self, result: Result<(), String>) {
        self.menu_busy = false;
        match result {
            Ok(()) => tracing::info!("Explorer menu registered"),
            Err(e) => {
                tracing::warn!("Explorer menu: {e}");
                self.notify("menu", t!("탐색기 메뉴를 켜지 못했습니다", "Couldn't turn on the Explorer menu"), &e, &[]);
            }
        }
        self.push();
    }

    pub(super) fn disconnected(&mut self, id: u64) {
        let Some(client) = self.clients.remove(&id) else { return };
        if self.engine != Some(id) {
            return;
        }
        tracing::warn!("the engine (pid {}) disconnected", client.pid);
        self.engine = None;
        self.launched = None;
        self.peers = 0;
        self.targets.clear();
        for (job, j) in self.jobs.iter_mut() {
            if !j.state.is_final() && j.state != SendState::Queued {
                j.state = SendState::Failed {
                    reason: text::ENGINE_STOPPED.into(),
                };
                if let Some(c) = &self.cards {
                    c.send_state(*job, &j.target.name, &j.files, &j.state, None, "");
                }
            }
        }
        self.phase = if self.adapter.is_some() { Phase::Failed } else { Phase::NoAdapter };
        self.failed_at = Some(Instant::now());
        // 어댑터가 꽂혀 있는 동안 on_tick이 다시 띄웁니다.
        self.push();
    }

    pub(super) fn restart_engine(&mut self) {
        self.retries = 0;
        self.declined = false;
        if !self.to_engine(Msg::Config { cfg: self.engine_config() }) {
            self.launched = None;
            self.ensure_engine();
        } else {
            self.to_engine(Msg::Reconnect);
        }
        self.push();
    }

    /// 잠시 찾습니다. 메뉴와 기기 고르기에 기기가 차도록.
    pub(super) fn wake(&mut self) {
        let now = Instant::now();
        let since = match self.search {
            Some((since, until)) if until > now => since,
            _ => now,
        };
        self.search = Some((since, now + SEARCH_FOR));
        if !self.to_engine(Msg::Discover { secs: SEARCH_FOR.as_secs() }) {
            self.ensure_engine();
        }
    }

    pub(super) fn on_tick(&mut self) {
        self.update_tick();
        self.sync_contacts();
        self.renew_sign_in();
        self.check_devices();
        if self.phase == Phase::Launching && self.launched.is_some_and(|t| t.elapsed() > LAUNCH_FOR) {
            self.phase = Phase::Failed;
            tracing::warn!("the engine didn't start in time");
            self.push();
        }
        // 어댑터가 꽂혀 있는 동안 WirDrop은 연결을 유지합니다: 실패한 링크는 다시 해 보고, 사라진
        // 엔진은 다시 띄우되 오래 실패할수록 덜 자주 합니다. 관리자 권한을 거절했으면 사용자를
        // 기다립니다 (메뉴의 상태 줄).
        if self.phase == Phase::Failed
            && self.adapter.is_some()
            && !self.declined
            && !self.exiting
            && self.failed_at.is_some_and(|t| t.elapsed() > retry_after(self.retries))
        {
            self.retries += 1;
            self.failed_at = Some(Instant::now());
            if self.engine.is_some() {
                tracing::info!("reconnecting the adapter ({})", self.retries);
                self.to_engine(Msg::Reconnect);
            } else {
                tracing::info!("starting the engine again ({})", self.retries);
                self.launched = None;
                self.ensure_engine();
            }
        }
        let expired: Vec<u64> = self
            .asks
            .iter()
            .filter(|(_, a)| a.asked.is_some_and(|t| t.elapsed() > ASK_FOR))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.answer(id, false, false);
        }
    }

    /// USB 변화가 멎은 뒤([DEVICES_SETTLE]) 어댑터가 꽂혔는지 빠졌는지 봅니다. 꽂혔으면 링크를
    /// 시작합니다 (엔진은 WinUSB가 잡을 때까지 잠깐 기다립니다).
    pub(super) fn check_devices(&mut self) {
        if !self.devices_dirty.is_some_and(|t| t.elapsed() >= DEVICES_SETTLE) {
            return;
        }
        self.devices_dirty = None;
        // 막 꽂은 BE6500UA는 가상 CD로 먼저 뜹니다: 꺼내면 곧 무선 랜으로 다시 잡혀 다음 확인에서 보입니다.
        crate::driver::eject_driver_cd();
        let adapter = crate::win::find_adapter();
        if adapter == self.adapter {
            return;
        }
        tracing::info!("adapter: {:?} -> {adapter:?}", self.adapter);
        let plugged = adapter.is_some();
        self.adapter = adapter;
        if plugged {
            self.retries = 0;
            // 어댑터를 꽂는 것은 WirDrop을 쓰겠다는 뜻입니다.
            self.declined = false;
            if self.engine.is_some() && self.phase != Phase::Ready {
                self.to_engine(Msg::Reconnect);
            } else {
                self.launched = None;
                self.ensure_engine();
            }
        } else if self.engine.is_none() {
            self.phase = Phase::NoAdapter;
        }
        self.push();
    }
}

/// 실패한 링크가 다음 시도까지 기다리는 시간: 5초에서 두 배씩, 1분까지.
pub(super) fn retry_after(tries: u32) -> Duration {
    Duration::from_secs((5u64 << tries.min(4)).min(60))
}
