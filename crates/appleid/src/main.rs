//! appleid: Mac이 AirDrop을 위해 하듯 이 PC를 Apple ID에 로그인시키고, WirDrop을 위해 로그인을
//! 유지합니다. 실행 방법은 셋이고, 모두 APPLEID_OUT이 가리키는 로그인 폴더를 씁니다.
//!
//! - `appleid --json`: 로그인합니다. 표준 입력으로 {"email","password","model"}을, 확인 코드를
//!   물으면 {"code"}를 받고, 표준 출력에는 한 줄에 JSON 이벤트 하나를 씁니다:
//!   {"ev":"status","msg"}, {"ev":"need_code","sent"} (sent: Apple이 계정의 기기에 코드를
//!   띄웠는지), 그다음 {"ev":"done","identity","status"}, {"ev":"later"} (로그인했지만 Apple이 아직
//!   AirDrop 토큰을 주지 않음: 저장된 세션으로 `--refresh`가 마무리), 또는 {"ev":"error","msg"}.
//! - `appleid --refresh`: AirDrop 인증서와 확인 기록을 만료 전에 저장된 세션으로, 암호 없이
//!   갱신합니다. 종료 코드 0: 갱신했거나 할 것이 없음, 2: 로그인이 사라져 다시 해야 함, 1: 그 밖
//!   (나중에 다시).
//! - `appleid --contacts`: 저장된 토큰으로 iCloud 연락처를 다시 읽습니다.
//!
//! APPLEID_LOG는 진행을 덧붙여 쓸 파일입니다 (비밀은 절대 쓰지 않습니다).
//! APPLEID_SEPARATE_USER=1이면 새 로그인이 PC에 자기 로컬 사용자를 받습니다 (다른 사람의 Apple
//! ID용).

/// 진행 한 줄 ([status] 참고).
macro_rules! say {
    ($($t:tt)*) => { $crate::status(&format!($($t)*)) };
}

mod aidc;
mod anisette;
mod contacts;
mod gsa;
mod http;
mod name;
mod secret;
mod srp;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anisette::Anisette;
use serde_json::{Value, json};

/// 사람이 아니라 WirDrop과 JSON 이벤트로 이야기합니다 (--json).
static JSON: AtomicBool = AtomicBool::new(false);

/// 로그인 세션과 AirDrop 앱 토큰. 보호해서 둡니다 (secret 참고).
const SESSION: &str = "session.plist";
const APP_TOKEN: &str = "gs-token.txt";
/// WirDrop도 status.json도 말하지 않을 때 iPhone에 보일 PC의 모델.
const DEFAULT_MODEL: &str = "MacPro7,1";
/// 확인 기록(30일 유효)과 인증서(1년)를 만료보다 얼마나 앞서 갱신하는지.
const RECORD_AHEAD: u64 = 10 * 86_400;
const CERTIFICATE_AHEAD: u64 = 30 * 86_400;

/// 진행 한 줄: 로그에, 그리고 WirDrop에는 status 이벤트로, 콘솔에는 그대로.
pub fn status(message: &str) {
    if JSON.load(Ordering::Relaxed) {
        event(json!({ "ev": "status", "msg": message }));
    } else {
        println!("{message}");
    }
    log_line(message);
}

/// APPLEID_LOG의 한 줄.
pub fn log_line(line: &str) {
    if let Some(path) = std::env::var_os("APPLEID_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{} {line}", anisette::utc_now());
    }
}

fn event(e: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{e}").and_then(|()| out.flush());
}

/// 표준 입력으로 WirDrop이 보낸 JSON 한 줄.
fn read_request() -> Value {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    serde_json::from_str(line.trim()).unwrap_or(Value::Null)
}

/// WirDrop(또는 콘솔의 사람)에게 확인 코드를 묻습니다. sent: Apple이 계정의 기기에 코드를 띄웠는지
/// (아니면 기기 설정에서 받습니다).
fn ask_code(sent: bool) -> String {
    if JSON.load(Ordering::Relaxed) {
        event(json!({ "ev": "need_code", "sent": sent }));
        let answer = read_request();
        return answer["code"].as_str().unwrap_or_default().trim().to_string();
    }
    print!("Verification code: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Some(out) = std::env::var_os("APPLEID_OUT").map(PathBuf::from) else {
        eprintln!("APPLEID_OUT names the sign-in's folder");
        std::process::exit(1);
    };
    let _ = std::fs::create_dir_all(&out);
    let code = match std::env::args().nth(1).as_deref() {
        Some("--json") => {
            JSON.store(true, Ordering::Relaxed);
            match sign_in(&out, read_request()).await {
                Ok(SignedIn::Done(dir)) => {
                    let status = std::fs::read_to_string(dir.join("status.json"))
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
                    event(json!({ "ev": "done", "identity": dir, "status": status }));
                    0
                }
                Ok(SignedIn::Later) => {
                    log_line("signed in; the AirDrop token comes later (--refresh)");
                    event(json!({ "ev": "later" }));
                    0
                }
                Err(e) => {
                    log_line(&format!("sign-in failed: {e}"));
                    event(json!({ "ev": "error", "msg": e }));
                    1
                }
            }
        }
        Some("--refresh") => refresh(&out).await,
        Some("--contacts") => match contacts::sync(&out).await {
            Ok(_) => 0,
            Err(e) => {
                say!("contacts not read: {e}");
                1
            }
        },
        _ => {
            eprintln!("appleid --json | --refresh | --contacts");
            1
        }
    };
    std::process::exit(code);
}

/// 오류가 아닌 로그인의 끝.
enum SignedIn {
    /// 이 폴더에 신원과 함께.
    Done(PathBuf),
    /// 로그인하고 세션도 저장했지만 Apple이 아직 AirDrop 토큰을 주지 않았습니다: --refresh가 저장된
    /// 세션으로 토큰과 인증서를 받습니다.
    Later,
}

/// WirDrop이 보낸 것으로 로그인하고, AirDrop 앱 토큰, 인증서, iCloud 연락처를 받습니다.
async fn sign_in(out: &Path, request: Value) -> Result<SignedIn, String> {
    let account = request["email"].as_str().unwrap_or_default().trim().to_string();
    let password = request["password"].as_str().unwrap_or_default().to_string();
    let model = request["model"].as_str().unwrap_or(DEFAULT_MODEL).to_string();
    if account.is_empty() || password.is_empty() {
        return Err("no Apple ID or password".into());
    }
    let mut anisette = Anisette::start(out).await?;
    say!("signing in");
    let session = gsa::sign_in(&account, &password, &mut anisette, &mut ask_code).await?;
    drop(password);
    say!("login ok");
    name::save(
        out,
        Some(&name::full_name(session.field("fn"), session.field("ln"))),
        Some(session.field("acname")),
        false,
    );
    // PET은 몇 분만 유효합니다: 느린 인증서보다 먼저 iCloud 토큰으로 바꿉니다.
    match session.pet() {
        Some(pet) => match contacts::delegates(&account, pet, &mut anisette).await {
            Ok((dsid, token)) => match contacts::save_token(out, &dsid, &token) {
                Ok(()) => say!("iCloud token saved"),
                Err(e) => say!("iCloud token not saved: {e}"),
            },
            Err(e) => say!("no iCloud token: {e}"),
        },
        None => say!("no PET from the sign-in, so no iCloud contacts"),
    }
    save_session(out, &session)?;
    let token = match gsa::airdrop_token(&session, &mut anisette).await {
        Ok(token) => token,
        Err(gsa::Fault::Busy(_)) => return Ok(SignedIn::Later),
        Err(e) => return Err(format!("Apple didn't give the AirDrop token: {e}")),
    };
    secret::write(&out.join(APP_TOKEN), token.as_bytes())?;
    let dir = out.join("identity");
    aidc::Aidc::new(&token).issue(&dir, &model, &mut anisette).await?;
    if let Err(e) = contacts::sync(out).await {
        say!("contacts not read: {e}");
    }
    Ok(SignedIn::Done(dir))
}

fn save_session(out: &Path, session: &gsa::Session) -> Result<(), String> {
    let mut bytes = Vec::new();
    plist::to_writer_binary(&mut bytes, &plist::Value::Dictionary(session.kept())).map_err(|e| e.to_string())?;
    secret::write(&out.join(SESSION), &bytes)
}

fn load_session(out: &Path) -> Result<gsa::Session, String> {
    match plist::from_bytes::<plist::Value>(&secret::read(&out.join(SESSION))?) {
        Ok(plist::Value::Dictionary(kept)) => Ok(gsa::Session::from_kept(kept)),
        _ => Err("the saved session is unreadable".into()),
    }
}

/// Apple 기기가 로그인을 유지하듯 때가 된 것을 갱신합니다: 옛 확인 기록이 끝나기 전에 새것을,
/// 인증서가 끝나기 전에 새것을. 만료된 앱 토큰은 저장된 세션으로 다시 받습니다. 종료 코드는
/// 크레이트 문서 참고.
async fn refresh(out: &Path) -> i32 {
    let dir = out.join("identity");
    let status: Value = std::fs::read_to_string(dir.join("status.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let model = status["model"].as_str().unwrap_or(DEFAULT_MODEL).to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record_ends = status["record_valid_as_of"].as_u64().unwrap_or(0) + status["record_valid_seconds"].as_u64().unwrap_or(0);
    let certificate_ends = status["cert_not_after"].as_u64().unwrap_or(0);
    let certificate_due = certificate_ends < now + CERTIFICATE_AHEAD;
    let due = std::env::var_os("APPLEID_FORCE").is_some() || certificate_due || record_ends < now + RECORD_AHEAD;
    if !due {
        say!(
            "nothing due: the record is good for {} more days, the certificate {}",
            (record_ends - now) / 86_400,
            (certificate_ends - now) / 86_400
        );
        return 0;
    }
    // 암호 뒤에 멈춘 로그인(GrandSlam이 바쁨)은 세션은 남겼지만 앱 토큰이 없습니다: 그 세션으로
    // 여기서 토큰을 받습니다.
    let saved = secret::read(&out.join(APP_TOKEN))
        .ok()
        .map(|t| String::from_utf8_lossy(&t).trim().to_string())
        .filter(|t| !t.is_empty());
    if saved.is_none() && !out.join(SESSION).is_file() {
        say!("no saved app token or session: sign in again");
        return 2;
    }
    let mut anisette = match Anisette::start(out).await {
        Ok(a) => a,
        Err(e) => {
            say!("anisette: {e}");
            return 1;
        }
    };
    let renew = async |token: &str, anisette: &mut Anisette| {
        let aidc = aidc::Aidc::new(token);
        if certificate_due {
            say!(
                "the certificate runs out in {} days: asking for a new one",
                certificate_ends.saturating_sub(now) / 86_400
            );
            aidc.issue(&dir, &model, anisette).await
        } else {
            aidc.renew_record(&dir, &model, anisette).await
        }
    };
    match &saved {
        Some(token) => match renew(token, &mut anisette).await {
            Ok(()) => {
                say!("sign-in renewed");
                return 0;
            }
            Err(first) => say!("with the saved app token: {first}; buying a new one"),
        },
        None => say!("no saved app token: buying one with the saved session"),
    }
    let Ok(session) = load_session(out) else {
        say!("no saved session: sign in again");
        return 2;
    };
    let token = match gsa::airdrop_token(&session, &mut anisette).await {
        Ok(token) => token,
        Err(gsa::Fault::Refused(code, message)) => {
            say!("the saved sign-in is gone ({message}, {code}): sign in again");
            return 2;
        }
        Err(e) => {
            say!("no new app token: {e}");
            return 1;
        }
    };
    if let Err(e) = secret::write(&out.join(APP_TOKEN), token.as_bytes()) {
        say!("app token not saved: {e}");
    }
    match renew(&token, &mut anisette).await {
        Ok(()) => {
            say!("sign-in renewed");
            0
        }
        Err(e) => {
            say!("not renewed: {e}");
            1
        }
    }
}
