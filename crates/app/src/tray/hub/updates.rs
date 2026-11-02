//! 업데이트 (update.rs): 새 릴리스를 찾고, 아무 일도 없을 때 서비스가 갈아 끼우게 합니다.
use super::*;

impl Hub {
    /// 때가 되면 새 릴리스를 찾고, 조용할 때 들입니다. Program Files에 설치된 것만 업데이트합니다:
    /// 다른 곳에서 돌리는 것(빌드)은 하지 않습니다.
    pub(super) fn update_tick(&mut self) {
        if self.exiting || !crate::service::installed_here() {
            return;
        }
        let again = if self.update_failed { UPDATE_RETRY } else { UPDATE_EVERY };
        let due = match self.update_checked {
            None => self.started.elapsed() > UPDATE_FIRST,
            Some(t) => t.elapsed() > again,
        };
        if due && !self.checking_update && self.update.is_none() {
            self.checking_update = true;
            self.update_checked = Some(Instant::now());
            let events = self.events.clone();
            tokio::task::spawn_blocking(move || {
                let _ = events.send(HubEvent::UpdateChecked(crate::update::check()));
            });
        }
        if self.update.is_none() || self.updating {
            return;
        }
        // 주고받는 중이면 끝나기를 기다립니다. 켠 뒤로 아무것도 없었으면 (다시 켠 직후) 바로 들입니다.
        if self.busy() {
            self.busy_at = Some(Instant::now());
            return;
        }
        if self.busy_at.is_none_or(|t| t.elapsed() >= UPDATE_QUIET) {
            self.begin_update();
        }
    }

    /// 보내기, 받기, 묻기, 로그인 가운데 무언가 진행 중입니다.
    pub(super) fn busy(&self) -> bool {
        !self.asks.is_empty() || self.jobs.values().any(|j| !j.state.is_final()) || self.signing_in || self.menu_busy
    }

    /// 엔진을 놓아 주고 서비스가 업데이트를 놓게 합니다.
    pub(super) fn begin_update(&mut self) {
        let Some(manifest) = &self.update else { return };
        if self.updating {
            return;
        }
        tracing::info!("updating to {}", manifest.version);
        self.updating = true;
        self.to_engine(Msg::Shutdown);
        let events = self.events.clone();
        let session = crate::ipc::current_session();
        tokio::task::spawn_blocking(move || {
            let _ = events.send(HubEvent::Updated(crate::service::update(session)));
        });
    }

    /// 업데이트 실행이 끝났습니다: 새 버전으로 넘어가거나, 하던 대로 갑니다.
    pub(super) fn updated(&mut self, result: io::Result<u32>) {
        match result {
            Ok(crate::service::UPDATED) => {
                // 새 WirDrop.exe는 이것이 있던 자리에 있고, 이것이 끝나길 기다립니다.
                let started = std::process::Command::new(crate::paths::exe())
                    .args(["--autostart", "--after", &std::process::id().to_string()])
                    .spawn();
                match started {
                    Ok(_) => {
                        tracing::info!("updated: starting the new version");
                        self.exit();
                        return;
                    }
                    Err(e) => tracing::error!("updated, but the new version won't start: {e}"),
                }
            }
            Ok(crate::service::UP_TO_DATE) => {
                tracing::info!("update: nothing newer after all");
                self.update_failed = false;
            }
            Ok(code) => {
                tracing::warn!("update failed ({code}): logs\\update.log");
                self.update_failed = true;
            }
            Err(e) => {
                tracing::warn!("update failed: {e}");
                self.update_failed = true;
            }
        }
        self.updating = false;
        self.update = None;
        self.busy_at = None;
        self.launched = None;
        self.ensure_engine();
        self.push();
    }
}
