//! Apple의 GrandSlam 로그인 (gsa.apple.com): 암호로 SRP, 계정이 원하면 기기(또는 문자 메시지)의
//! 확인 코드, 그다음 로그인이 남긴 세션으로 받는 앱 토큰. 그래서 암호는 한 번만 필요합니다.
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use aes_gcm::aead::{Aead, KeyInit, Payload, consts::U16};
use base64::Engine;
use hmac::{Hmac, Mac};
use plist::{Dictionary, Value};
use sha2::Sha256;

use crate::anisette::Anisette;
use crate::srp;

const SERVICE: &str = "https://gsa.apple.com/grandslam/GsService2";
const AUTH: &str = "https://gsa.apple.com/auth";
const AGENT: &str = "akd/1.0 CFNetwork/3860.100.1 Darwin/25.7.0";
/// 로그인하는 쪽: anisette를 만든 AOSKit의 iCloud for Windows.
pub const CLIENT_INFO: &str = "<PC> <Windows;10.0.26100;> <com.apple.AuthKit/1 (com.apple.iCloud/7.21)>";
/// aidc.apple.com이 받는 토큰의 앱 (Mac에서는 sharingd의 것).
const AIRDROP_APP: &str = "com.apple.gs.authagent.auth";
/// 확인 코드 직후 GrandSlam이 바쁠 때 다시 로그인하는 간격.
const BUSY_TRIES: u32 = 6;
/// GrandSlam이 503으로 답하는 동안 AirDrop 앱 토큰을 새로 청하기 전마다 기다리는 시간. 로그인 직후
/// 몇 분 동안 그렇습니다 (재 보니 60초에도 그랬고 4분에는 괜찮음). 암호 단계는 이미 끝났고, 몇 분
/// 된 세션은 바로 토큰을 받습니다.
const TOKEN_WAITS: [u64; 7] = [15, 20, 30, 40, 60, 60, 60];

/// 로그인이 남기는 것을 해독한 세션 데이터("spd"): adsid, GsIdmsToken, 암호 없이 앱 토큰을 사는 키
/// sk와 c, 계정의 이름들, iCloud용 PET.
pub struct Session(Dictionary);

impl Session {
    /// [Session::kept]로 저장한 세션.
    pub fn from_kept(kept: Dictionary) -> Session {
        Session(kept)
    }

    /// 로그인을 유지하려고 남기는 것: adsid, GsIdmsToken, sk, c.
    pub fn kept(&self) -> Dictionary {
        ["adsid", "GsIdmsToken", "sk", "c"]
            .into_iter()
            .filter_map(|k| Some((k.to_string(), self.0.get(k)?.clone())))
            .collect()
    }

    fn text(&self, key: &str) -> Result<&str, String> {
        self.0
            .get(key)
            .and_then(Value::as_string)
            .ok_or_else(|| format!("the session has no {key}"))
    }

    fn data(&self, key: &str) -> Result<&[u8], String> {
        self.0
            .get(key)
            .and_then(Value::as_data)
            .ok_or_else(|| format!("the session has no {key}"))
    }

    /// 로그인이 준 문자열, 없으면 "" (fn, ln: 계정 주인의 이름; acname: Apple ID).
    pub fn field(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_string).unwrap_or_default()
    }

    /// PET: iCloud 토큰을 사는, 5분 동안 암호를 대신하는 것.
    pub fn pet(&self) -> Option<&str> {
        let tokens = self.0.get("t")?.as_dictionary()?;
        tokens.get("com.apple.gs.idms.pet")?.as_dictionary()?.get("token")?.as_string()
    }

    /// X-Apple-Identity-Token: "adsid:GsIdmsToken"의 base64.
    fn identity(&self) -> Result<String, String> {
        Ok(base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.text("adsid")?, self.text("GsIdmsToken")?)))
    }
}

/// GrandSlam 요청(그 "Request" 사전)을 보내고 "Response" 사전, 또는 알려 준 오류를 돌려줍니다.
async fn call(request: Dictionary) -> Result<Dictionary, Fault> {
    let mut header = Dictionary::new();
    header.insert("Version".into(), "1.0.1".into());
    let mut envelope = Dictionary::new();
    envelope.insert("Header".into(), Value::Dictionary(header));
    envelope.insert("Request".into(), Value::Dictionary(request));
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(envelope)).map_err(|e| Fault::Other(e.to_string()))?;
    let res = crate::http::client()
        .post(SERVICE)
        .header("Content-Type", "text/x-xml-plist")
        .header("Accept", "*/*")
        .header("User-Agent", AGENT)
        .header("X-MMe-Client-Info", CLIENT_INFO)
        .body(body)
        .send()
        .await
        .map_err(|e| Fault::Other(e.to_string()))?;
    let status = res.status();
    let bytes = res.bytes().await.map_err(|e| Fault::Other(e.to_string()))?;
    let Ok(Value::Dictionary(mut reply)) = plist::from_bytes::<Value>(&bytes) else {
        // plist 대신 오류 페이지: GrandSlam이 바쁨(503) 같은 것.
        return Err(Fault::Busy(status.as_u16()));
    };
    let Some(Value::Dictionary(response)) = reply.remove("Response") else {
        return Err(Fault::Other(format!("HTTP {status}, no Response")));
    };
    check(&response)?;
    Ok(response)
}

/// GrandSlam 호출이 실패한 까닭.
#[derive(Debug)]
pub enum Fault {
    /// GrandSlam 자체의 오류: 코드(ec)와 메시지(em).
    Refused(i64, String),
    /// plist가 오지 않았고 HTTP 상태는 이것.
    Busy(u16),
    Other(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Refused(code, message) => write!(f, "{message} ({code})"),
            Fault::Busy(status) => write!(f, "Apple's sign-in server is busy (HTTP {status})"),
            Fault::Other(message) => f.write_str(message),
        }
    }
}

impl From<Fault> for String {
    fn from(f: Fault) -> String {
        f.to_string()
    }
}

/// Status 또는 사전 자체에 있는 GrandSlam의 상태: ec 0이면 괜찮습니다.
fn check(reply: &Dictionary) -> Result<(), Fault> {
    let status = reply.get("Status").and_then(Value::as_dictionary).unwrap_or(reply);
    match status.get("ec").and_then(Value::as_signed_integer) {
        Some(0) | None => Ok(()),
        Some(code) => Err(Fault::Refused(
            code,
            status.get("em").and_then(Value::as_string).unwrap_or_default().to_string(),
        )),
    }
}

/// 암호 뒤에 계정이 원하는 것.
#[derive(Debug, PartialEq)]
enum Next {
    Nothing,
    /// 계정의 기기에서 받는 코드 (au trustedDeviceSecondaryAuth, 또는 hsc 409).
    DeviceCode,
    /// 문자 메시지로 받는 코드 (au secondaryAuth).
    TextCode,
}

/// password로 account에 로그인합니다. 계정이 확인 코드를 원하면 code()에 묻고 (Apple이 계정의
/// 기기로 보냈는지 함께 알림), Apple 기기처럼 확인된 상태로 로그인을 한 번 더 합니다.
pub async fn sign_in(account: &str, password: &str, anisette: &mut Anisette, code: &mut dyn FnMut(bool) -> String) -> Result<Session, String> {
    let (session, next) = password_round(account, password, anisette).await?;
    match next {
        Next::Nothing => return Ok(session),
        Next::DeviceCode => {
            let sent = push_code(&session, anisette).await;
            validate_code(&session, anisette, &code(sent)).await?;
        }
        Next::TextCode => {
            let phone = text_code(&session, anisette).await?;
            verify_text_code(&session, anisette, phone, &code(true)).await?;
        }
    }
    // 코드가 확인된 상태로 한 번 더. 코드 직후에는 GrandSlam이 한동안 바쁠 수 있습니다.
    let mut tries = 0;
    loop {
        match password_round(account, password, anisette).await {
            // 두 번째에 주는 것이 가진 전부입니다: 아직 무엇을 원한다고 하든.
            Ok((session, _)) => return Ok(session),
            Err(Fault::Busy(status)) if tries < BUSY_TRIES => {
                tries += 1;
                say!("Apple's sign-in server is busy (HTTP {status}); trying again in 30 s ({tries}/{BUSY_TRIES})");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// 암호로 하는 SRP 한 번: init, 그다음 complete. 남긴 세션과 계정이 다음에 원하는 것.
async fn password_round(account: &str, password: &str, anisette: &mut Anisette) -> Result<(Session, Next), Fault> {
    let client = srp::Client::new();
    let mut init = Dictionary::new();
    init.insert("A2k".into(), Value::Data(client.public().to_vec()));
    init.insert("cpd".into(), Value::Dictionary(anisette.client_data().map_err(Fault::Other)?));
    init.insert("o".into(), "init".into());
    init.insert("ps".into(), Value::Array(vec!["s2k".into(), "s2k_fo".into()]));
    init.insert("u".into(), account.into());
    let challenge = call(init).await?;
    let field = |key: &str| challenge.get(key).ok_or_else(|| Fault::Other(format!("the challenge has no {key}")));
    let salt = field("s")?.as_data().ok_or_else(|| Fault::Other("s".into()))?;
    let b_pub = field("B")?.as_data().ok_or_else(|| Fault::Other("B".into()))?;
    let rounds = field("i")?.as_signed_integer().ok_or_else(|| Fault::Other("i".into()))?;
    let c = field("c")?.as_string().ok_or_else(|| Fault::Other("c".into()))?;
    let protocol = challenge.get("sp").and_then(Value::as_string).unwrap_or("s2k");
    let key = srp::password_key(password, protocol, salt, u32::try_from(rounds).map_err(|_| Fault::Other("i".into()))?);
    let proof = client.prove(account, &key, salt, b_pub).map_err(Fault::Other)?;

    let mut complete = Dictionary::new();
    complete.insert("M1".into(), Value::Data(proof.m1.clone()));
    complete.insert("c".into(), c.into());
    complete.insert("cpd".into(), Value::Dictionary(anisette.client_data().map_err(Fault::Other)?));
    complete.insert("o".into(), "complete".into());
    complete.insert("u".into(), account.into());
    let done = call(complete).await?;
    let m2 = done.get("M2").and_then(Value::as_data).ok_or_else(|| Fault::Other("no M2".into()))?;
    if !proof.server_proved(m2) {
        return Err(Fault::Other("Apple's server didn't prove itself".into()));
    }
    let spd = done
        .get("spd")
        .and_then(Value::as_data)
        .ok_or_else(|| Fault::Other("no session data".into()))?;
    let session = open_session_data(&proof.key, spd).map_err(Fault::Other)?;
    let status = done.get("Status").and_then(Value::as_dictionary).cloned().unwrap_or_default();
    log_status(&status);
    let next = match status.get("au").and_then(Value::as_string) {
        Some("secondaryAuth") => Next::TextCode,
        Some(_) => Next::DeviceCode,
        None if status.get("hsc").and_then(Value::as_signed_integer) == Some(409) => Next::DeviceCode,
        None => Next::Nothing,
    };
    Ok((session, next))
}

/// SRP 키 K로 만든 키로 AES-256-CBC 암호화된 세션 데이터.
fn open_session_data(srp_key: &[u8], sealed: &[u8]) -> Result<Session, String> {
    let derive = |label: &[u8]| {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(srp_key).expect("HMAC takes any key length");
        mac.update(label);
        mac.finalize().into_bytes()
    };
    let (key, iv) = (derive(b"extra data key:"), derive(b"extra data iv:"));
    let plain = cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, &iv[..16])
        .map_err(|e| e.to_string())?
        .decrypt_padded_vec_mut::<Pkcs7>(sealed)
        .map_err(|_| "the session data didn't decrypt".to_string())?;
    match plist::from_bytes::<Value>(&plain) {
        Ok(Value::Dictionary(d)) => Ok(Session(d)),
        _ => Err("the session data isn't a dictionary".into()),
    }
}

/// 로그인 상태의 로그 줄: 이름과 짧은 값만, 토큰은 절대 쓰지 않습니다.
fn log_status(status: &Dictionary) {
    let short: Vec<String> = status
        .iter()
        .filter(|(_, v)| v.as_string().is_none_or(|s| s.len() < 40))
        .map(|(k, v)| {
            format!(
                "{k}={}",
                v.as_string()
                    .map(str::to_string)
                    .or_else(|| v.as_signed_integer().map(|n| n.to_string()))
                    .unwrap_or_else(|| "…".into())
            )
        })
        .collect();
    crate::log_line(&format!("sign-in status: {}", short.join(", ")));
}

/// 확인 코드 요청의 헤더: anisette, 그리고 묻는 쪽.
fn code_headers(session: &Session, anisette: &mut Anisette) -> Result<Vec<(String, String)>, String> {
    let mut headers: Vec<(String, String)> = anisette.headers()?.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    headers.extend([
        ("User-Agent".to_string(), AGENT.to_string()),
        ("X-MMe-Client-Info".into(), CLIENT_INFO.into()),
        ("Accept-Language".into(), "en-us".into()),
        ("X-Apple-Identity-Token".into(), session.identity()?),
        ("Loc".into(), anisette.locale().to_string()),
    ]);
    Ok(headers)
}

fn request(method: reqwest::Method, url: &str, headers: &[(String, String)]) -> reqwest::RequestBuilder {
    headers
        .iter()
        .fold(crate::http::client().request(method, url), |r, (k, v)| r.header(k.as_str(), v.as_str()))
}

/// Apple에 계정의 기기들에 코드를 띄워 달라고 합니다: 띄웠는지 돌려줍니다. 띄우지 않으면 기기
/// 설정에서 코드를 받을 수 있으니 까닭만 로그에 남깁니다.
async fn push_code(session: &Session, anisette: &mut Anisette) -> bool {
    let sent = async {
        let headers = code_headers(session, anisette)?;
        let res = request(reqwest::Method::GET, &format!("{AUTH}/verify/trusteddevice"), &headers)
            .header("Content-Type", "text/x-xml-plist")
            .header("Accept", "text/x-xml-plist")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", res.status()))
        }
    };
    match sent.await {
        Ok(()) => {
            say!("a verification code is on the way to the account's devices");
            true
        }
        Err(e) => {
            say!("Apple didn't send a code to the devices ({e}); one can be had in a device's settings");
            false
        }
    }
}

/// 기기의 코드를 GrandSlam에 건넵니다.
async fn validate_code(session: &Session, anisette: &mut Anisette, code: &str) -> Result<(), String> {
    let headers = code_headers(session, anisette)?;
    let res = request(reqwest::Method::GET, &format!("{SERVICE}/validate"), &headers)
        .header("Content-Type", "text/x-xml-plist")
        .header("Accept", "text/x-xml-plist")
        .header("security-code", code.trim())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    match plist::from_bytes::<Value>(&bytes) {
        Ok(Value::Dictionary(reply)) => check(&reply).map_err(|e| format!("the verification code was refused: {e}")),
        _ => Err(format!("the verification code was refused (HTTP {status})")),
    }
}

/// Apple에 계정의 첫 신뢰 전화번호로 코드를 문자로 보내 달라고 합니다: 그 번호의 ID.
async fn text_code(session: &Session, anisette: &mut Anisette) -> Result<u64, String> {
    let headers = code_headers(session, anisette)?;
    let phones: serde_json::Value = request(reqwest::Method::GET, AUTH, &headers)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .unwrap_or_default();
    let phone = phones["trustedPhoneNumbers"][0]["id"].as_u64().unwrap_or(1);
    let res = request(reqwest::Method::PUT, &format!("{AUTH}/verify/phone/"), &headers)
        .json(&serde_json::json!({ "phoneNumber": { "id": phone }, "mode": "sms" }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("Apple didn't text a verification code (HTTP {})", res.status()));
    }
    say!("a verification code is on the way by text message");
    Ok(phone)
}

/// 문자 메시지로 온 코드를 Apple에 건넵니다.
async fn verify_text_code(session: &Session, anisette: &mut Anisette, phone: u64, code: &str) -> Result<(), String> {
    let headers = code_headers(session, anisette)?;
    let body = serde_json::json!({ "phoneNumber": { "id": phone }, "mode": "sms", "securityCode": { "code": code.trim() } });
    let res = request(reqwest::Method::POST, &format!("{AUTH}/verify/phone/securitycode"), &headers)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(format!("the verification code was refused (HTTP {})", res.status()))
    }
}

/// 세션으로 (암호 없이) 받는 AirDrop 앱 토큰: aidc.apple.com의 X-Apple-GS-Token, "adsid:token"의
/// base64. GrandSlam이 바쁘면 (오류 페이지, 503) 5분쯤까지 다시 청합니다 ([TOKEN_WAITS] 참고).
pub async fn airdrop_token(session: &Session, anisette: &mut Anisette) -> Result<String, Fault> {
    let mut waits = TOKEN_WAITS.iter();
    loop {
        match buy_airdrop_token(session, anisette).await {
            Err(Fault::Busy(status)) => {
                let Some(&wait) = waits.next() else { return Err(Fault::Busy(status)) };
                say!("Apple isn't giving the AirDrop token yet (HTTP {status}); asking again in {wait} s");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
            other => return other,
        }
    }
}

/// 새 anisette로 하는 apptokens 요청 한 번.
async fn buy_airdrop_token(session: &Session, anisette: &mut Anisette) -> Result<String, Fault> {
    let adsid = session.text("adsid").map_err(Fault::Other)?;
    let sk = session.data("sk").map_err(Fault::Other)?;
    let mut checksum = <Hmac<Sha256> as Mac>::new_from_slice(sk).expect("HMAC takes any key length");
    for part in [b"apptokens".as_slice(), adsid.as_bytes(), AIRDROP_APP.as_bytes()] {
        checksum.update(part);
    }
    let mut ask = Dictionary::new();
    ask.insert("app".into(), Value::Array(vec![AIRDROP_APP.into()]));
    ask.insert("c".into(), Value::Data(session.data("c").map_err(Fault::Other)?.to_vec()));
    ask.insert("checksum".into(), Value::Data(checksum.finalize().into_bytes().to_vec()));
    ask.insert("cpd".into(), Value::Dictionary(anisette.client_data().map_err(Fault::Other)?));
    ask.insert("o".into(), "apptokens".into());
    ask.insert("t".into(), session.text("GsIdmsToken").map_err(Fault::Other)?.into());
    ask.insert("u".into(), adsid.into());
    let reply = call(ask).await?;
    let sealed = reply
        .get("et")
        .and_then(Value::as_data)
        .ok_or_else(|| Fault::Other("no app tokens in the answer".into()))?;
    let tokens = open_app_tokens(sk, sealed).map_err(Fault::Other)?;
    let token = tokens
        .get("t")
        .and_then(Value::as_dictionary)
        .and_then(|t| t.get(AIRDROP_APP))
        .and_then(Value::as_dictionary)
        .and_then(|a| a.get("token"))
        .and_then(Value::as_string)
        .ok_or_else(|| Fault::Other("no AirDrop app token in the answer".into()))?;
    say!("app token ok ({} chars)", token.len());
    Ok(base64::engine::general_purpose::STANDARD.encode(format!("{adsid}:{token}")))
}

/// GrandSlam이 봉인한 앱 토큰: 3바이트 태그 (연관 데이터이기도 함), 16바이트 IV, 그다음 세션 키
/// sk로 AES-256-GCM.
fn open_app_tokens(sk: &[u8], sealed: &[u8]) -> Result<Dictionary, String> {
    if sealed.len() < 3 + 16 + 16 {
        return Err("the app tokens are cut short".into());
    }
    let cipher = aes_gcm::AesGcm::<aes::Aes256, U16>::new_from_slice(sk).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(
            sealed[3..19].into(),
            Payload {
                msg: &sealed[19..],
                aad: &sealed[..3],
            },
        )
        .map_err(|_| "the app tokens didn't decrypt".to_string())?;
    match plist::from_bytes::<Value>(&plain) {
        Ok(Value::Dictionary(d)) => Ok(d),
        _ => Err("the app tokens aren't a dictionary".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    fn plist_bytes(d: &Dictionary) -> Vec<u8> {
        let mut out = Vec::new();
        plist::to_writer_binary(&mut out, &Value::Dictionary(d.clone())).unwrap();
        out
    }

    #[test]
    fn opens_session_data() {
        let srp_key = [3u8; 32];
        let mut spd = Dictionary::new();
        spd.insert("adsid".into(), "000123-00-abc".into());
        spd.insert("sk".into(), Value::Data(vec![1; 32]));
        let derive = |label: &[u8]| {
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&srp_key).unwrap();
            mac.update(label);
            mac.finalize().into_bytes()
        };
        let (key, iv) = (derive(b"extra data key:"), derive(b"extra data iv:"));
        let sealed = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv[..16])
            .unwrap()
            .encrypt_padded_vec_mut::<Pkcs7>(&plist_bytes(&spd));
        let session = open_session_data(&srp_key, &sealed).unwrap();
        assert_eq!(session.text("adsid").unwrap(), "000123-00-abc");
        assert_eq!(session.kept().len(), 2);
        assert!(open_session_data(&[4u8; 32], &sealed).is_err());
    }

    #[test]
    fn opens_app_tokens() {
        let sk = [5u8; 32];
        let mut app = Dictionary::new();
        app.insert("token".into(), "tok".into());
        let mut t = Dictionary::new();
        t.insert(AIRDROP_APP.into(), Value::Dictionary(app));
        let mut tokens = Dictionary::new();
        tokens.insert("t".into(), Value::Dictionary(t));
        let cipher = aes_gcm::AesGcm::<aes::Aes256, U16>::new_from_slice(&sk).unwrap();
        let (tag, iv) = (*b"XYZ", [9u8; 16]);
        let body = cipher
            .encrypt(
                iv.as_slice().into(),
                Payload {
                    msg: &plist_bytes(&tokens),
                    aad: &tag,
                },
            )
            .unwrap();
        let sealed = [tag.as_slice(), &iv, &body].concat();
        let opened = open_app_tokens(&sk, &sealed).unwrap();
        assert!(opened.get("t").is_some());
        assert!(open_app_tokens(&[6u8; 32], &sealed).is_err());
    }

    #[test]
    fn reads_grandslam_errors() {
        let mut status = Dictionary::new();
        status.insert("ec".into(), Value::Integer((-20101).into()));
        status.insert("em".into(), "nope".into());
        let mut reply = Dictionary::new();
        reply.insert("Status".into(), Value::Dictionary(status));
        assert!(matches!(check(&reply), Err(Fault::Refused(-20101, _))));
        assert!(check(&Dictionary::new()).is_ok());
    }
}
