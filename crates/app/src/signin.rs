//! 창 없는 Apple ID 로그인: 아일랜드가 Apple ID, 암호, 확인 코드를 입력받고 진행과 결과를 알리며,
//! 실제 로그인은 appleid.exe(appleid 크레이트)가 합니다.
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tokio::sync::mpsc::UnboundedSender;
use wirdrop_island::{Answer, Card, Field, Glyph, Handle, Kind};

use crate::tray::hub::HubEvent;

/// 허브에 알리는 로그인 진행.
#[derive(Debug)]
pub enum Progress {
    /// 진행 중: 지금 하는 일.
    Status(String),
    /// 이 로그인 폴더에서 label이라는 Apple ID에 로그인했습니다.
    Done { label: String },
    /// root에서 label이라는 Apple ID에 로그인했지만 Apple이 아직 AirDrop 인증서를 주지 않았습니다:
    /// 몇 분 안에 갱신(appleid --refresh)이 받아 옵니다.
    Later { root: PathBuf, label: String },
    /// 실패했습니다. root는 다시 해 볼 폴더 (Apple ID를 하나 더 추가하던 중이면 None).
    Failed { root: Option<PathBuf>, reason: String },
    /// 입력을 닫았습니다.
    Cancelled,
}

/// 로그인이 잘 끝난 방식.
enum Ended {
    Done(String),
    Later(PathBuf, String),
}

/// 로그인하지 못한 까닭과, 요청한 폴더가 아닐 때 다시 해 볼 로그인 폴더: 이미 로그인한 Apple ID면
/// 그 Apple ID의 폴더 (한 번 더 로그인하면 쓸모없는 두 번째 신원이 생기고, Apple은 그때마다
/// 인증서를 발급합니다).
struct Failure {
    reason: String,
    again: Option<PathBuf>,
}

impl Failure {
    /// 이미 이 PC에 로그인한 Apple ID여서 멈췄습니다.
    fn already(root: &Path) -> Failure {
        Failure {
            reason: t!("이미 로그인된 Apple ID입니다.", "This Apple ID is already signed in.").into(),
            again: Some(root.to_path_buf()),
        }
    }
}

impl From<String> for Failure {
    fn from(reason: String) -> Failure {
        Failure { reason, again: None }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Failure {
        reason.to_string().into()
    }
}

/// root의 Apple ID에 다시, 또는 None이면 Apple ID 하나를 더 로그인합니다. 묻는 것은 아일랜드가 합니다.
/// 질문과 appleid가 끝날 때까지 막히므로 따로 스레드에서 돌리세요.
pub fn run(root: Option<PathBuf>, events: UnboundedSender<HubEvent>, island: Handle) {
    let send = |p: Progress| {
        let _ = events.send(HubEvent::SignIn(p));
    };
    let end = match sign_in(root.as_deref(), &send, &island) {
        Ok(Some(Ended::Done(label))) => Progress::Done { label },
        Ok(Some(Ended::Later(root, label))) => Progress::Later { root, label },
        Ok(None) => Progress::Cancelled,
        Err(f) => Progress::Failed {
            root: f.again.or(root),
            reason: f.reason,
        },
    };
    send(end);
}

fn sign_in(root: Option<&Path>, send: &dyn Fn(Progress), island: &Handle) -> Result<Option<Ended>, Failure> {
    let exe = crate::paths::appleid().ok_or(t!("로그인 도구(appleid.exe)가 없습니다.", "The sign-in tool (appleid.exe) is missing."))?;
    let known = root.and_then(crate::paths::apple_id_email);
    let Some((email, mut password)) = ask_island(island, known.as_deref(), root)? else {
        return Ok(None);
    };
    let email = email.trim().to_string();
    if email.is_empty() || password.is_empty() {
        wipe(&mut password);
        return Err(t!("Apple ID와 암호를 입력해 주세요.", "Enter your Apple ID and password.").into());
    }
    let target = root.map(Path::to_path_buf).unwrap_or_else(crate::paths::new_apple_id_root);
    let _ = std::fs::create_dir_all(&target);
    send(Progress::Status(t!("로그인하는 중…", "Signing in…").into()));

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new(&exe);
    if crate::paths::is_more_apple_id(&target) {
        // 다른 사람의 Apple ID: Mac의 두 번째 사용자처럼 따로 로컬 사용자를 씁니다.
        command.env("APPLEID_SEPARATE_USER", "1");
    }
    let mut child = command
        .arg("--json")
        .env("APPLEID_OUT", &target)
        .env("APPLEID_LOG", target.join("appleid.log"))
        .current_dir(exe.parent().unwrap_or(&target))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| tf!("로그인 도구를 시작하지 못했습니다. ({e})", "The sign-in tool didn't start. ({e})"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut request = serde_json::json!({ "email": email, "password": password, "model": "MacPro7,1" }).to_string();
    wipe(&mut password);
    let written = writeln!(stdin, "{request}").and_then(|()| stdin.flush());
    wipe(&mut request);
    written.map_err(|e| e.to_string())?;

    let mut outcome: Result<Ended, Failure> = Err(t!("로그인 도구가 중간에 멈췄습니다.", "The sign-in tool stopped.").into());
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        match v["ev"].as_str() {
            Some("status") => {
                if let Some(status) = status_text(&text("msg")) {
                    send(Progress::Status(status.into()));
                }
            }
            Some("need_code") => {
                let sent = v["sent"].as_bool().unwrap_or(true);
                let Some(code) = island_code(island, sent) else {
                    // 코드 없이 닫았습니다: 로그인을 여기서 멈춥니다.
                    drop(stdin);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                };
                let _ = writeln!(stdin, "{}", serde_json::json!({ "code": code })).and_then(|()| stdin.flush());
                send(Progress::Status(t!("확인 코드를 확인하는 중…", "Checking the code…").into()));
            }
            Some("done") => {
                outcome = finish(&target, &email, root.is_none()).map(|()| Ended::Done(email.clone()));
                break;
            }
            Some("later") => {
                crate::paths::save_apple_id_email(&target, &email);
                outcome = Ok(Ended::Later(target.clone(), email.clone()));
                break;
            }
            Some("error") => {
                tracing::warn!("appleid: {}", text("msg"));
                outcome = Err(failure_text(&text("msg")).into());
                break;
            }
            _ => {}
        }
    }
    drop(stdin);
    let _ = child.wait();
    outcome.map(Some)
}

/// 아일랜드에 입력한 Apple ID(다시 로그인이면 이미 아는 것)와 암호. 무언가 입력할 때까지 다시 묻고,
/// 닫으면 None. 이 PC에 이미 로그인한 Apple ID면 (root의 것이 아니라면) 암호를 묻기 전에 멈춥니다.
fn ask_island(island: &Handle, known: Option<&str>, root: Option<&Path>) -> Result<Option<(String, String)>, Failure> {
    let email = match known {
        Some(email) => email.to_string(),
        None => loop {
            let Some(typed) = ask_line(
                island,
                t!("Apple ID 로그인", "Sign in to Apple ID"),
                t!("암호는 이 PC에 저장되지 않습니다", "Your password isn't stored on this PC"),
                t!("Apple ID (이메일 또는 전화번호)", "Apple ID (email or phone number)"),
                false,
                t!("다음", "Next"),
            ) else {
                return Ok(None);
            };
            let typed = typed.trim();
            if !typed.is_empty() {
                break typed.to_string();
            }
        },
    };
    if let Some(other) = already_here(&email, root) {
        return Err(Failure::already(&other));
    }
    let detail = if known.is_some() {
        tf!("{email}에 다시 로그인합니다", "Signing in to {email} again")
    } else {
        email.clone()
    };
    let (title, placeholder, action) = (t!("Apple ID 암호", "Apple ID password"), t!("암호", "Password"), t!("로그인", "Sign In"));
    loop {
        let Some(mut password) = ask_line(island, title, &detail, placeholder, true, action) else {
            return Ok(None);
        };
        if !password.is_empty() {
            return Ok(Some((email, password)));
        }
        wipe(&mut password);
    }
}

/// email Apple ID가 root가 아닌 다른 폴더에 이미 로그인해 있으면 그 로그인 폴더.
fn already_here(email: &str, root: Option<&Path>) -> Option<PathBuf> {
    signed_in_as(email).filter(|other| Some(other.as_path()) != root)
}

/// 아일랜드에 입력한 확인 코드 (공백은 뺍니다). 입력할 때까지 다시 묻고, 닫으면 None. sent: Apple이
/// 계정의 기기에 코드를 띄웠습니다.
fn island_code(island: &Handle, sent: bool) -> Option<String> {
    let hint = if sent {
        t!("Apple 기기에 표시된 6자리 코드", "The 6-digit code on your Apple device")
    } else {
        t!(
            "iPhone 설정 > 이름 > 로그인 및 보안 > 확인 코드 받기",
            "iPhone Settings > your name > Sign-In & Security > Get Verification Code"
        )
    };
    loop {
        let typed = ask_line(
            island,
            t!("확인 코드", "Verification code"),
            hint,
            t!("6자리 코드", "6-digit code"),
            false,
            t!("확인", "Verify"),
        )?;
        let code: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
        if !code.is_empty() {
            return Some(code);
        }
    }
}

/// 로그인 카드("signin": 진행과 결과도 이 자리에 뜹니다)에서 한 줄을 묻습니다. 닫거나 아일랜드가
/// 사라지면 None.
fn ask_line(island: &Handle, title: &str, detail: &str, placeholder: &str, secret: bool, action: &str) -> Option<String> {
    let answer = island.ask(Card {
        tag: "signin".into(),
        kind: Kind::Ask,
        glyph: Some(Glyph::Lock),
        title: title.into(),
        detail: detail.into(),
        field: Some(Field {
            placeholder: placeholder.into(),
            secret,
            action: action.into(),
            ..Field::default()
        }),
        ..Card::default()
    });
    match answer.recv() {
        Ok(Answer::Typed(text)) => Some(text),
        _ => None,
    }
}

/// appleid의 진행 줄을 사람이 읽을 말로. 새로 알릴 것이 없는 줄은 None.
fn status_text(msg: &str) -> Option<&'static str> {
    Some(match msg {
        m if m.contains("anisette") => t!("Apple에 연결하는 중…", "Connecting to Apple…"),
        m if m.starts_with("downloading Apple iCloud") => {
            t!(
                "iCloud 구성 요소를 받는 중… (처음 한 번, 160 MB)",
                "Downloading iCloud components… (once, 160 MB)"
            )
        }
        m if m.starts_with("unpacking iCloud") => t!("iCloud 구성 요소를 푸는 중…", "Unpacking iCloud components…"),
        "signing in" => t!("로그인하는 중…", "Signing in…"),
        "login ok" => t!("로그인했습니다. 인증서를 받는 중…", "Signed in. Getting the certificate…"),
        m if m.starts_with("Apple's sign-in server is busy") => t!("Apple 서버 응답을 기다리는 중…", "Waiting for Apple's server…"),
        m if m.starts_with("Apple isn't giving the AirDrop token yet") => {
            t!(
                "Apple이 준비하는 동안 기다리는 중… (최대 5분)",
                "Waiting for Apple to get ready… (up to 5 min)"
            )
        }
        m if m.starts_with("iCloud contacts token") || m.starts_with("iCloud token") => t!("연락처를 가져오는 중…", "Getting contacts…"),
        m if m.starts_with("waiting") && m.contains("certificate") => {
            t!("Apple이 인증서를 발급하는 중… (1~2분)", "Apple is issuing the certificate… (1–2 min)")
        }
        m if m.starts_with("AirDrop identity ready") => t!("인증서를 받았습니다", "Got the certificate"),
        _ => return None,
    })
}

/// appleid가 로그인하지 못한 까닭을 지금 언어의 짧은 말로 (appleid의 원문은 로그에 남습니다).
fn failure_text(msg: &str) -> String {
    let said = if msg.contains("server is busy") {
        t!(
            "Apple 서버가 응답하지 않습니다. 잠시 뒤 다시 해 주세요.",
            "Apple's server isn't answering. Try again later."
        )
    } else if ["(-20101)", "(-20970)", "(-22406)"].iter().any(|c| msg.contains(c)) {
        t!("Apple ID나 암호가 맞지 않습니다.", "The Apple ID or password is wrong.")
    } else if msg.contains("verification code was refused") || msg.contains("(-21669)") {
        t!("확인 코드가 맞지 않습니다.", "The verification code is wrong.")
    } else if msg.contains("anisette") || msg.contains("iCloud") {
        t!(
            "Apple 구성 요소를 준비하지 못했습니다. 잠시 뒤 다시 해 주세요.",
            "Apple components didn't get ready. Try again later."
        )
    } else {
        t!("로그인하지 못했습니다. 잠시 뒤 다시 해 주세요.", "Couldn't sign in. Try again later.")
    };
    said.into()
}

/// 이 이메일 주소나 전화번호로 로그인한 Apple ID가 이 PC에 있으면 그 로그인 폴더.
fn signed_in_as(email: &str) -> Option<PathBuf> {
    let email = email.trim();
    crate::paths::apple_id_roots()
        .into_iter()
        .find(|root| crate::paths::apple_id_email(root).is_some_and(|e| e.eq_ignore_ascii_case(email)))
}

/// appleid가 root의 Apple ID에 로그인한 뒤: 이름으로 쓰려고 로그인한 주소를 기억합니다. 새로 더한
/// Apple ID가 알고 보니 다른 주소로 이미 로그인해 있었다면 다시 지웁니다.
fn finish(root: &Path, email: &str, added: bool) -> Result<(), Failure> {
    crate::paths::save_apple_id_email(root, email);
    let account = |r: &Path| {
        let record = std::fs::read(crate::paths::identity_dir(r)?.join("validation_record.cms")).ok()?;
        crate::record::alt_dsid(&record)
    };
    let Some(this) = account(root).filter(|_| added) else { return Ok(()) };
    if let Some(other) = crate::paths::apple_id_roots()
        .into_iter()
        .find(|other| other != root && account(other).as_deref() == Some(this.as_str()))
    {
        let _ = std::fs::remove_dir_all(root);
        return Err(Failure::already(&other));
    }
    Ok(())
}

/// 비밀을 버리기 전에 덮어씁니다.
fn wipe(secret: &mut String) {
    // SAFETY: 0 바이트는 올바른 UTF-8입니다.
    unsafe { secret.as_bytes_mut().fill(0) };
    secret.clear();
}
