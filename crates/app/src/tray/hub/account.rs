//! Apple ID: 아일랜드에서 묻는 로그인, 그리고 Apple 기기처럼 로그인을 살려 두는 갱신과 iCloud 연락처 읽기.
use super::*;

impl Hub {
    pub(super) fn sign_in(&mut self, root: Option<PathBuf>) {
        if self.signing_in {
            self.notify(
                "signin-busy",
                t!("이미 로그인하는 중입니다", "Already signing in"),
                t!("지금 하는 로그인이 끝나면 다시 해 주세요.", "Try again when this sign-in is done."),
                &[],
            );
            return;
        }
        self.signing_in = true;
        tracing::info!("Apple ID sign-in: {}", if root.is_some() { "again" } else { "one more" });
        let events = self.events.clone();
        let Some(island) = self.cards.as_ref().map(Cards::handle) else {
            self.signing_in = false;
            self.notify(
                "signin-end",
                t!("Apple ID에 로그인할 수 없습니다", "Can't sign in to Apple ID"),
                t!(
                    "아일랜드를 띄우지 못했습니다. WirDrop을 다시 시작해 주세요.",
                    "The island didn't open. Restart WirDrop."
                ),
                &[],
            );
            return;
        };
        std::thread::spawn(move || crate::signin::run(root, events, island));
    }

    pub(super) fn sign_in_progress(&mut self, progress: crate::signin::Progress) {
        use crate::signin::Progress;
        match &progress {
            Progress::Status(_) => {}
            Progress::Done { .. } => tracing::info!("Apple ID sign-in: done"),
            Progress::Later { root, .. } => {
                tracing::info!("Apple ID sign-in: signed in; the certificate comes with the renewal");
                // 갱신이 지금부터 PENDING_EVERY마다 마저 받습니다.
                self.pending_signins.insert(root.clone(), 0);
                self.renew_run = Some(Instant::now());
            }
            Progress::Failed { reason, .. } => tracing::warn!("Apple ID sign-in failed: {reason}"),
            Progress::Cancelled => tracing::info!("Apple ID sign-in: the dialog was closed"),
        }
        let (signed_in, not_signed_in, again_label) = (
            t!("Apple ID에 로그인했습니다", "Signed in to Apple ID"),
            t!("Apple ID에 로그인하지 못했습니다", "Couldn't sign in to Apple ID"),
            t!("다시 로그인", "Sign In Again"),
        );
        let later = |label: &str| {
            tf!(
                "{label} · 인증서는 몇 분 안에 자동으로 받습니다.",
                "{label} · The certificate arrives in a few minutes."
            )
        };
        match &progress {
            Progress::Done { label } => self.notice("signin-end", menu::ACCOUNT_GLYPH, signed_in, label, &[]),
            Progress::Later { label, .. } => {
                self.notice("signin-end", menu::ACCOUNT_GLYPH, signed_in, &later(label), &[]);
            }
            Progress::Failed { root, reason } => {
                let again = format!("signin:{}", root.as_ref().map_or("new".to_string(), |r| r.display().to_string()));
                self.notice("signin-end", menu::WARNING_GLYPH, not_signed_in, reason, &[(again_label, &again)]);
            }
            _ => {}
        }
        let Some(c) = &self.cards else {
            if !matches!(progress, Progress::Status(_)) {
                self.signing_in = false;
            }
            return;
        };
        match progress {
            Progress::Status(status) => c.signing_in(&status),
            Progress::Done { label } => {
                self.signing_in = false;
                c.signed_in(true, signed_in, &label, &[]);
                self.restart_engine();
            }
            Progress::Later { label, .. } => {
                self.signing_in = false;
                c.signed_in(true, signed_in, &later(&label), &[]);
            }
            Progress::Failed { root, reason } => {
                self.signing_in = false;
                let again = format!("signin:{}", root.as_ref().map_or("new".to_string(), |r| r.display().to_string()));
                c.signed_in(false, not_signed_in, &reason, &[(again_label, again.as_str())]);
            }
            Progress::Cancelled => {
                self.signing_in = false;
                c.remove("signin");
            }
        }
    }

    /// root의 Apple ID(이름 who)에 대한 appleid --refresh 뒤: 인증서를 기다리던 로그인을
    /// 마무리하거나, 로그인이 사라졌으면 다시 암호를 청합니다.
    pub(super) fn renewed(&mut self, code: i32, root: PathBuf, who: String) {
        tracing::info!("sign-in renewal of {}: exit {code}", root.display());
        let again = format!("signin:{}", root.display());
        let again_label = t!("다시 로그인", "Sign In Again");
        if code == 0 {
            // 갱신이 마무리한 로그인은 엔진에 없는 신원을 가져옵니다: 지금 있는 Apple ID들로 다시
            // 시작합니다 (같으면 아무것도 하지 않습니다).
            self.to_engine(Msg::Config { cfg: self.engine_config() });
            self.push();
        }
        if let Some(tries) = self.pending_signins.get_mut(&root) {
            *tries += 1;
            match code {
                0 if crate::paths::identity_dir(&root).is_some() => {
                    self.pending_signins.remove(&root);
                    let got = t!("AirDrop 인증서를 받았습니다", "Got the AirDrop certificate");
                    if let Some(c) = &self.cards {
                        c.signed_in(true, got, &who, &[]);
                    }
                    self.notice("signin-end", menu::ACCOUNT_GLYPH, got, &who, &[]);
                    return;
                }
                2 => {
                    self.pending_signins.remove(&root);
                }
                _ if *tries < PENDING_TRIES => return,
                _ => {
                    self.pending_signins.remove(&root);
                    self.notify(
                        "apple-id",
                        t!("AirDrop 인증서를 받지 못했습니다", "Couldn't get the AirDrop certificate"),
                        &tf!("{who} · 다시 로그인해 주세요.", "{who} · Sign in again."),
                        &[(again_label, &again)],
                    );
                    return;
                }
            }
        }
        if code == 2 {
            self.notify(
                "apple-id",
                t!("Apple ID에 다시 로그인해 주세요", "Sign in to Apple ID again"),
                &tf!("{who}의 인증을 갱신하지 못했습니다.", "Couldn't renew the sign-in for {who}."),
                &[(t!("로그인", "Sign In"), &again)],
            );
        }
    }

    /// 로그인이 남긴 토큰으로 (appleid --contacts, 암호 없이) Apple ID마다 iCloud 연락처를
    /// CONTACTS_EVERY마다 차례로 다시 읽습니다. 엔진이 새 contact_hashes.txt를 알아챕니다.
    pub(super) fn sync_contacts(&mut self) {
        if self.contacts_run.is_some_and(|t| t.elapsed() < CONTACTS_EVERY) {
            return;
        }
        let Some(exe) = crate::paths::appleid() else { return };
        let due: Vec<PathBuf> = crate::paths::apple_id_roots()
            .into_iter()
            .filter(|root| root.join("contacts.token").is_file())
            .filter(|root| {
                let age = std::fs::metadata(root.join(r"identity\contact_hashes.txt"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok());
                !age.is_some_and(|a| a < CONTACTS_EVERY)
            })
            .collect();
        self.contacts_run = Some(Instant::now());
        if due.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for root in due {
                let status = appleid(&exe, "--contacts", &root);
                tracing::info!("iCloud contacts in {} read: {status:?}", root.display());
            }
        });
    }

    /// Apple 기기처럼 Apple ID마다 로그인을 살려 둡니다: RENEW_EVERY마다 appleid가 만료가 가까운
    /// 것(30일짜리 확인 기록, 1년짜리 인증서)을 저장된 토큰으로, 암호 없이, Apple ID 하나씩
    /// 갱신합니다. 대부분은 갱신할 것이 없습니다. 새 파일은 엔진이 가져가고, 더는 갱신할 수 없는
    /// 로그인만 사용자에게 묻습니다.
    pub(super) fn renew_sign_in(&mut self) {
        let every = if self.pending_signins.is_empty() { RENEW_EVERY } else { PENDING_EVERY };
        if self.renew_run.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        let Some(exe) = crate::paths::appleid() else { return };
        self.renew_run = Some(Instant::now());
        let roots: Vec<(PathBuf, String)> = crate::paths::apple_id_roots()
            .into_iter()
            .enumerate()
            .filter(|(_, root)| root.join("gs-token.txt").is_file() || root.join("session.plist").is_file())
            .map(|(i, root)| {
                let label = crate::paths::apple_id_label(&root, i);
                (root, label)
            })
            .collect();
        let events = self.events.clone();
        std::thread::spawn(move || {
            for (root, label) in roots {
                let status = appleid(&exe, "--refresh", &root);
                let code = status.as_ref().ok().and_then(|s| s.code()).unwrap_or(1);
                let _ = events.send(HubEvent::Renewed(code, root, label));
            }
        });
    }
}

/// root에 로그인한 Apple ID(APPLEID_OUT)로 appleid.exe를 인자 하나와 함께 숨겨서 돌리고 기다립니다.
/// 다른 사람의 Apple ID는 자기 로컬 사용자(root의 anisette 폴더)를 씁니다.
pub(super) fn appleid(exe: &std::path::Path, arg: &str, root: &std::path::Path) -> io::Result<std::process::ExitStatus> {
    crate::win::hidden(&exe.display().to_string())
        .arg(arg)
        .env("APPLEID_OUT", root)
        .env("APPLEID_LOG", root.join("appleid.log"))
        .current_dir(exe.parent().unwrap_or(root))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
}
