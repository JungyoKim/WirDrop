//! aidc.apple.com의 AirDrop 인증서. Mac의 sharingd처럼 청합니다: 우리 키와 그
//! 요청(createCertificate), 1~2분 뒤 Apple의 서명(fetchCertificate), 그리고 PC를 Apple ID에 묶는
//! 확인 기록(getMyInfo의 aidvr). WirDrop 엔진이 읽는 신원 폴더에 들어갑니다: certificate.der,
//! intermediate.der, key.pk8.der, validation_record.cms, hashes.txt, status.json.
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;
use serde_json::{Value, json};
use x509_cert::builder::{Builder, RequestBuilder};
use x509_cert::der::{Decode, EncodePem};

use crate::anisette::Anisette;

const SERVICES: &str = "https://aidc.apple.com/aidcert/services/";
const AGENT: &str = "sharingd/2.0 CFNetwork/1498.700.2 Darwin/23.6.0";
/// aidc가 이 PC의 anisette와 함께 받는, iCloud for Windows 플랫폼의 sharingd.
const CLIENT_INFO: &str = "<PC> <Windows;10.0.26100;> <com.apple.AuthKit/1 (com.apple.sharingd/2.0)>";
/// 포기하기 전까지 인증서를 찾아보는 간격.
const FETCH_TRIES: u32 = 12;

/// 이 AirDrop 앱 토큰의 Apple ID로 쓰는 aidc.apple.com.
pub struct Aidc {
    token: String,
}

impl Aidc {
    pub fn new(token: &str) -> Aidc {
        Aidc { token: token.to_string() }
    }

    /// aidc 서비스를 부릅니다. responseCode가 0이면 그 답.
    pub async fn call(&self, service: &str, body: Value, anisette: &mut Anisette) -> Result<Value, String> {
        let mut req = crate::http::client()
            .post(format!("{SERVICES}{service}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", AGENT)
            .header("X-Apple-GS-Token", &self.token)
            .header("X-MMe-Client-Info", CLIENT_INFO)
            .header("X-Apple-I-Locale", anisette.locale().to_string());
        for (k, v) in anisette.headers()? {
            match k {
                "X-Mme-Device-Id" => req = req.header("X-Mme-Device-ID", v),
                "X-Apple-I-SRL-NO" | "X-Apple-Locale" => {}
                _ => req = req.header(k, v),
            }
        }
        let res = req.body(body.to_string()).send().await.map_err(|e| e.to_string())?;
        let status = res.status();
        let text = res.text().await.map_err(|e| e.to_string())?;
        let answer: Value = serde_json::from_str(&text).map_err(|_| format!("{service}: HTTP {status}, not JSON"))?;
        say!(
            "{service}: HTTP {} code {} {}",
            status.as_u16(),
            answer["responseCode"],
            answer["responseMessage"]
        );
        match answer["responseCode"].as_i64() {
            Some(0) => Ok(answer),
            _ => Err(format!("{service} refused: {} {}", answer["responseCode"], answer["responseMessage"])),
        }
    }

    /// model(iPhone이 보는 이 PC의 Mac)용 새 키, 인증서, 기록을 dir에.
    pub async fn issue(&self, dir: &Path, model: &str, anisette: &mut Anisette) -> Result<(), String> {
        let (key, csr) = key_and_request()?;
        let created = self.call("createCertificate", json!({ "csr": csr }), anisette).await?;
        let ticket = created["certificate"]["certificateToken"]
            .as_str()
            .ok_or("no certificate token")?
            .to_string();
        let mut wait = created["delay"].as_u64().unwrap_or(90_000);
        let mut issued = None;
        for attempt in 1..=FETCH_TRIES {
            say!("waiting {} s for Apple to sign the certificate (attempt {attempt})", wait / 1000);
            tokio::time::sleep(Duration::from_millis(wait + 2_000)).await;
            let fetched = self.call("fetchCertificate", json!({ "certificateToken": ticket }), anisette).await?;
            if fetched["certificate"]["status"].as_str() == Some("ISSUED") {
                issued = Some(fetched);
                break;
            }
            wait = fetched["delay"].as_u64().filter(|d| *d > 0).unwrap_or(30_000);
        }
        let issued = issued.ok_or("Apple didn't sign the certificate in time")?;
        let info = self.call("getMyInfo", json!({}), anisette).await?;
        let pem = |v: &Value| {
            v.as_str()
                .ok_or("no certificate in the answer")
                .and_then(|s| der_of(s).ok_or("a certificate that isn't PEM"))
        };
        let certificate = pem(&issued["certificate"]["certificate"])?;
        let intermediate = pem(&issued["intermediateCaCertificate"])?;
        let record = record_of(&info)?;
        write_identity(dir, &key, &certificate, &intermediate, &record)?;
        write_status(dir, model)?;
        say!("AirDrop identity ready: {}", dir.display());
        Ok(())
    }

    /// dir 신원의 새 확인 기록. 키와 인증서는 그대로 둡니다.
    pub async fn renew_record(&self, dir: &Path, model: &str, anisette: &mut Anisette) -> Result<(), String> {
        let record = record_of(&self.call("getMyInfo", json!({}), anisette).await?)?;
        put(dir, "hashes.txt", record_hashes(&record)?.join("\n").as_bytes())?;
        put(dir, "validation_record.cms", &record)?;
        let status = write_status(dir, model)?;
        say!(
            "validation record renewed, good for {} days",
            status["record_valid_seconds"].as_u64().unwrap_or(0) / 86_400
        );
        Ok(())
    }
}

/// sharingd가 만드는 것처럼 새 RSA-2048 키(PKCS#8 DER)와 그 인증서 요청(PEM).
fn key_and_request() -> Result<(Vec<u8>, String), String> {
    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).map_err(|e| e.to_string())?;
    let der = key.to_pkcs8_der().map_err(|e| e.to_string())?.as_bytes().to_vec();
    let signer = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(key);
    let subject = x509_cert::name::Name::from_str("CN=AirDrop").map_err(|e| e.to_string())?;
    let request = RequestBuilder::new(subject, &signer)
        .map_err(|e| e.to_string())?
        .build::<rsa::pkcs1v15::Signature>()
        .map_err(|e| e.to_string())?;
    let pem = request.to_pem(x509_cert::der::pem::LineEnding::LF).map_err(|e| e.to_string())?;
    Ok((der, pem))
}

/// PEM 안의 DER.
fn der_of(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.lines().map(str::trim).filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}

/// getMyInfo의 확인 기록 (aidvr, base64 CMS).
fn record_of(info: &Value) -> Result<Vec<u8>, String> {
    let aidvr = info["aidvr"].as_str().ok_or("no validation record in the answer")?;
    base64::engine::general_purpose::STANDARD.decode(aidvr).map_err(|e| e.to_string())
}

/// 확인 기록 안에 Apple이 서명한 plist.
fn record_plist(record: &[u8]) -> Result<plist::Dictionary, String> {
    let start = record.windows(5).position(|w| w == b"<?xml").ok_or("the record has no plist")?;
    let end = record.windows(8).rposition(|w| w == b"</plist>").ok_or("the record's plist has no end")? + 8;
    match plist::from_bytes::<plist::Value>(&record[start..end]) {
        Ok(plist::Value::Dictionary(d)) => Ok(d),
        _ => Err("the record's plist isn't a dictionary".into()),
    }
}

/// hashes.txt의 줄들: 기록에 있는 그대로 계정의 이메일 주소와 전화번호 해시.
fn record_hashes(record: &[u8]) -> Result<Vec<String>, String> {
    let d = record_plist(record)?;
    let mut lines = Vec::new();
    for (key, kind) in [("ValidatedEmailHashes", "email"), ("ValidatedPhoneHashes", "phone")] {
        for hash in d.get(key).and_then(plist::Value::as_array).into_iter().flatten() {
            if let Some(bytes) = hash.as_data() {
                lines.push(format!("{kind} {}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()));
            } else if let Some(text) = hash.as_string() {
                lines.push(format!("{kind} {}", text.trim().to_lowercase()));
            }
        }
    }
    Ok(lines)
}

/// dir의 name을 통째로 씁니다: 옆에 쓴 뒤 옮겨, 읽는 쪽이 반쪽을 보지 않게.
fn put(dir: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let new = dir.join(format!("{name}.new"));
    std::fs::write(&new, data)
        .and_then(|()| std::fs::rename(&new, dir.join(name)))
        .map_err(|e| format!("{name}: {e}"))
}

/// 신원 폴더: 이 Windows 사용자와 WirDrop 엔진이 도는 SYSTEM만 읽을 수 있고, 다른 누구도 못
/// 읽습니다.
fn write_identity(dir: &Path, key: &[u8], certificate: &[u8], intermediate: &[u8], record: &[u8]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let user = std::env::var("USERNAME").unwrap_or_default();
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            &format!("{user}:(OI)(CI)F"),
            "/grant:r",
            "*S-1-5-18:(OI)(CI)R",
            "/Q",
        ])
        .creation_flags(0x0800_0000)
        .output();
    let hashes = record_hashes(record)?;
    put(dir, "key.pk8.der", key)?;
    put(dir, "certificate.der", certificate)?;
    put(dir, "intermediate.der", intermediate)?;
    put(dir, "hashes.txt", hashes.join("\n").as_bytes())?;
    put(dir, "validation_record.cms", record)?;
    let emails = hashes.iter().filter(|l| l.starts_with("email")).count();
    say!("identity written ({emails} email, {} phone hashes)", hashes.len() - emails);
    Ok(())
}

/// 신원 옆의 status.json: 모델, 그리고 인증서와 기록이 끝나는 때 (Unix 초). --refresh가 이것으로
/// 판단합니다.
pub fn write_status(dir: &Path, model: &str) -> Result<Value, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let der = std::fs::read(dir.join("certificate.der")).map_err(|e| e.to_string())?;
    let certificate = x509_cert::Certificate::from_der(&der).map_err(|e| e.to_string())?;
    let not_after = certificate.tbs_certificate.validity.not_after.to_unix_duration().as_secs();
    let record = record_plist(&std::fs::read(dir.join("validation_record.cms")).map_err(|e| e.to_string())?)?;
    let as_of = record
        .get("ValidAsOf")
        .and_then(plist::Value::as_date)
        .and_then(|d| std::time::SystemTime::from(d).duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let lasts = record
        .get("SuggestValidDuration")
        .and_then(plist::Value::as_unsigned_integer)
        .unwrap_or(0);
    let status = json!({
        "model": model, "issued_at": now, "cert_not_after": not_after,
        "record_valid_as_of": as_of, "record_valid_seconds": lasts,
    });
    put(dir, "status.json", status.to_string().as_bytes())?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_request_for_its_key() {
        let (key, pem) = key_and_request().unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE REQUEST-----"));
        assert!(der_of(&pem).is_some_and(|d| d.len() > 200));
        assert!(key.len() > 1000);
    }

    #[test]
    fn reads_a_record() {
        let record = b"\x30\x82junk<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ValidatedEmailHashes</key><array><data>AAEC</data></array><key>ValidatedPhoneHashes</key><array><string> ABCD </string></array></dict></plist>\x00\x01";
        assert_eq!(record_hashes(record).unwrap(), vec!["email 000102", "phone abcd"]);
    }
}
