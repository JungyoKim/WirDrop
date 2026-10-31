//! AirDrop의 연락처 확인에 쓰는 iCloud 연락처. iCloud for Windows처럼 가져옵니다: 로그인의 PET으로
//! loginDelegates에서 iCloud(MobileMe) 토큰을 사서 이 Windows 사용자용 DPAPI로 보호해 두고,
//! 그것으로 CardDAV로 주소록을 읽습니다. 전화번호와 이메일 주소의 SHA-256 해시만, Apple이 해시할
//! 법한 표기마다 씁니다 (identity\contact_hashes.txt). 이름과 카드 자체는 절대 남기지 않습니다.
use std::collections::BTreeSet;
use std::path::Path;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::anisette::Anisette;

const TOKEN_FILE: &str = "contacts.token";
const HASHES_FILE: &str = "contact_hashes.txt";
const CARDDAV: &str = "https://contacts.icloud.com";

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn uuid_v4() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// loginDelegates가 받는 anisette 헤더.
fn anisette_headers(anisette: &mut Anisette) -> Result<Vec<(String, String)>, String> {
    Ok(anisette
        .headers()?
        .into_iter()
        .filter_map(|(k, v)| match k {
            "X-Mme-Device-Id" => Some(("X-Mme-Device-ID".to_string(), v)),
            "X-Apple-I-SRL-NO" | "X-Apple-Locale" => None,
            _ => Some((k.to_string(), v)),
        })
        .collect())
}

fn text(v: Option<&plist::Value>) -> Option<String> {
    let v = v?;
    v.as_string()
        .map(str::to_string)
        .or_else(|| v.as_unsigned_integer().map(|n| n.to_string()))
        .or_else(|| v.as_signed_integer().map(|n| n.to_string()))
}

/// PET으로 하는 loginDelegates: 계정의 dsid와 iCloud(MobileMe) 인증 토큰.
pub async fn delegates(user: &str, pet: &str, anisette: &mut Anisette) -> Result<(String, String), String> {
    let mut mobileme = plist::Dictionary::new();
    mobileme.insert("com.apple.mobileme".into(), plist::Value::Dictionary(plist::Dictionary::new()));
    let mut body = plist::Dictionary::new();
    body.insert("apple-id".into(), user.into());
    body.insert("client-id".into(), uuid_v4().into());
    body.insert("delegates".into(), plist::Value::Dictionary(mobileme));
    body.insert("password".into(), pet.into());
    let mut xml = Vec::new();
    plist::to_writer_xml(&mut xml, &plist::Value::Dictionary(body)).map_err(|e| e.to_string())?;
    let headers = anisette_headers(anisette)?;
    let mut last = String::from("no endpoint tried");
    for url in [
        "https://setup.icloud.com/setup/prefpane/loginDelegates",
        "https://setup.icloud.com/setup/iosbuddy/loginDelegates",
    ] {
        let mut req = crate::http::client()
            .post(url)
            .header("Authorization", format!("Basic {}", b64(&format!("{user}:{pet}"))))
            .header("Content-Type", "text/plist")
            .header("Accept", "*/*")
            .header("X-MMe-Client-Info", crate::gsa::CLIENT_INFO)
            .header("X-Apple-I-Locale", anisette.locale().to_string())
            .body(xml.clone());
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last = format!("{url}: {e}");
                continue;
            }
        };
        let status = res.status();
        let bytes = res.bytes().await.map_err(|e| e.to_string())?;
        let Ok(v) = plist::from_bytes::<plist::Value>(&bytes) else {
            last = format!("{url}: HTTP {status}, not a plist");
            continue;
        };
        let d = v.as_dictionary().cloned().unwrap_or_default();
        let mme = d
            .get("delegates")
            .and_then(|v| v.as_dictionary())
            .and_then(|v| v.get("com.apple.mobileme"))
            .and_then(|v| v.as_dictionary());
        let token = mme
            .and_then(|m| m.get("service-data"))
            .and_then(|v| v.as_dictionary())
            .and_then(|s| s.get("tokens"))
            .and_then(|v| v.as_dictionary())
            .and_then(|t| text(t.get("mmeAuthToken")));
        let dsid = text(d.get("dsid")).or_else(|| {
            mme.and_then(|m| m.get("service-data"))
                .and_then(|v| v.as_dictionary())
                .and_then(|s| text(s.get("dsid")))
        });
        match (dsid, token) {
            (Some(dsid), Some(token)) => {
                say!("iCloud token from {}", url.rsplit('/').nth(1).unwrap_or(url));
                return Ok((dsid, token));
            }
            _ => {
                let status_message = text(d.get("status-message")).or_else(|| mme.and_then(|m| text(m.get("status-message"))));
                last = format!("{url}: HTTP {status}, status {:?} {:?}", text(d.get("status")), status_message);
            }
        }
    }
    Err(last)
}

/// 이 Windows 사용자만 다시 읽을 수 있는 곳에 토큰을 둡니다.
pub fn save_token(out: &Path, dsid: &str, token: &str) -> Result<(), String> {
    let json = serde_json::json!({ "dsid": dsid, "token": token }).to_string();
    crate::secret::write(&out.join(TOKEN_FILE), json.as_bytes())
}

fn load_token(out: &Path) -> Result<(String, String), String> {
    let bytes = crate::secret::read(&out.join(TOKEN_FILE)).map_err(|_| "no iCloud contacts token: sign in again".to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    match (json["dsid"].as_str(), json["token"].as_str()) {
        (Some(d), Some(t)) => Ok((d.to_string(), t.to_string())),
        _ => Err("contacts token unreadable".into()),
    }
}

/// 이 로컬 이름을 가진 모든 요소의 안쪽 글. 네임스페이스 접두사는 상관없습니다.
fn elements<'a>(xml: &'a str, local: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = xml[at..].find('<').map(|i| at + i) {
        at = i + 1;
        let tag = &xml[i + 1..];
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let end = tag.find(['>', ' ', '/', '\t', '\r', '\n']).unwrap_or(tag.len());
        let name = &tag[..end];
        if name.rsplit(':').next() != Some(local) {
            continue;
        }
        let Some(open_end) = tag.find('>') else { break };
        if tag[..open_end].ends_with('/') {
            out.push("");
            continue;
        }
        let body = &tag[open_end + 1..];
        let close = format!("</{name}>");
        if let Some(c) = body.find(&close) {
            out.push(&body[..c]);
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(s);
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#13;", "\r")
        .replace("&#xD;", "\r")
        .replace("&amp;", "&")
}

async fn dav(method: &str, url: &str, auth: &str, depth: &str, body: &str) -> Result<(u16, String), String> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let res = crate::http::client()
        .request(m, url)
        .header("Authorization", auth)
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("X-MMe-Client-Info", crate::gsa::CLIENT_INFO)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    Ok((status, res.text().await.unwrap_or_default()))
}

fn absolute(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.starts_with("http") {
        return href.to_string();
    }
    let origin = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    format!("{origin}{href}")
}

/// CardDAV로 읽은 iCloud 주소록의 모든 전화번호와 이메일 주소, 그리고 CardDAV principal이 알려 주는
/// 계정 이름.
async fn identifiers(dsid: &str, token: &str) -> Result<(usize, Vec<(bool, String)>, Option<String>), String> {
    let pair = b64(&format!("{dsid}:{token}"));
    let principal_q = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
    let mut chosen = None;
    let mut tried = Vec::new();
    for auth in [format!("X-MobileMe-AuthToken {pair}"), format!("Basic {pair}")] {
        let (status, xml) = dav("PROPFIND", &format!("{CARDDAV}/"), &auth, "0", principal_q).await?;
        tried.push(status);
        if status == 207 {
            let principal = elements(&xml, "current-user-principal")
                .first()
                .and_then(|p| elements(p, "href").first().map(|h| h.trim().to_string()));
            chosen = Some((auth, principal.unwrap_or_else(|| format!("/{dsid}/principal/"))));
            break;
        }
    }
    let Some((auth, principal)) = chosen else {
        return Err(format!("CardDAV refused the iCloud token (HTTP {tried:?})"));
    };
    let home_q = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><c:addressbook-home-set/><d:displayname/></d:prop></d:propfind>"#;
    let principal_url = absolute(CARDDAV, &principal);
    let (status, xml) = dav("PROPFIND", &principal_url, &auth, "0", home_q).await?;
    let name = elements(&xml, "displayname").first().map(|n| unescape(n)).filter(|n| !n.is_empty());
    let home = elements(&xml, "addressbook-home-set")
        .first()
        .and_then(|h| elements(h, "href").first().map(|s| absolute(&principal_url, s)));
    let Some(home) = home else {
        return Err(format!("no address book home (HTTP {status})"));
    };
    let list_q = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;
    let (status, xml) = dav("PROPFIND", &home, &auth, "1", list_q).await?;
    let books: Vec<String> = elements(&xml, "response")
        .into_iter()
        .filter(|r| elements(r, "resourcetype").first().is_some_and(|t| t.contains("addressbook")))
        .filter_map(|r| elements(r, "href").first().map(|h| absolute(&home, h)))
        .collect();
    if books.is_empty() {
        return Err(format!("no address books (HTTP {status})"));
    }
    let cards_q = r#"<?xml version="1.0" encoding="utf-8"?><c:addressbook-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><c:address-data/></d:prop></c:addressbook-query>"#;
    let (mut cards, mut ids) = (0, Vec::new());
    for book in books {
        let (status, xml) = dav("REPORT", &book, &auth, "1", cards_q).await?;
        if status != 207 {
            say!("address book read failed: HTTP {status}");
            continue;
        }
        for card in elements(&xml, "address-data") {
            cards += 1;
            ids.extend(card_identifiers(&unescape(card)));
        }
    }
    Ok((cards, ids, name))
}

/// vCard의 TEL과 EMAIL 줄마다 (전화번호인지, 값).
fn card_identifiers(vcard: &str) -> Vec<(bool, String)> {
    let mut lines: Vec<String> = Vec::new();
    for raw in vcard.split('\n') {
        let raw = raw.trim_end_matches('\r');
        match (raw.strip_prefix(' ').or_else(|| raw.strip_prefix('\t')), lines.last_mut()) {
            (Some(more), Some(last)) => last.push_str(more),
            _ => lines.push(raw.to_string()),
        }
    }
    lines
        .iter()
        .filter_map(|l| {
            let (head, value) = l.split_once(':')?;
            let name = head.split(';').next()?.rsplit('.').next()?.to_ascii_uppercase();
            let value = value.trim();
            match name.as_str() {
                "TEL" if !value.is_empty() => Some((true, value.trim_start_matches("tel:").to_string())),
                "EMAIL" if !value.is_empty() => Some((false, value.to_string())),
                _ => None,
            }
        })
        .collect()
}

/// Apple이 전화번호를 해시할 법한 표기들: +82... (E.164), 82..., 국내 0..., 적힌 그대로의 숫자.
/// 국가 코드 없는 번호는 한국 번호로 봅니다.
fn phone_spellings(raw: &str) -> Vec<(&'static str, String)> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 5 {
        return Vec::new();
    }
    let e164 = if raw.trim_start().starts_with('+') {
        format!("+{digits}")
    } else if let Some(rest) = digits.strip_prefix("00") {
        format!("+{rest}")
    } else if let Some(rest) = digits.strip_prefix('0') {
        format!("+82{rest}")
    } else {
        format!("+{digits}")
    };
    let mut out = vec![("e164", e164.clone()), ("e164-no-plus", e164[1..].to_string()), ("as-written", digits)];
    if let Some(rest) = e164.strip_prefix("+82") {
        out.push(("national", format!("0{rest}")));
    }
    out
}

fn email_spellings(raw: &str) -> Vec<(&'static str, String)> {
    let t = raw.trim().trim_start_matches("mailto:").to_string();
    vec![("lowercase", t.to_lowercase()), ("as-written", t)]
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// 저장된 토큰으로 주소록을 읽고 identity\contact_hashes.txt를 씁니다.
pub async fn sync(out: &Path) -> Result<String, String> {
    let (dsid, token) = load_token(out)?;
    let (cards, ids, name) = identifiers(&dsid, &token).await?;
    say!("account name from iCloud: {}", if name.is_some() { "found" } else { "none" });
    crate::name::save(out, name.as_deref(), None, true);
    let own: BTreeSet<String> = std::fs::read_to_string(out.join("identity").join("hashes.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_lowercase))
        .collect();
    let (mut hashes, mut matched) = (BTreeSet::new(), BTreeSet::new());
    for (phone, value) in &ids {
        let spellings = if *phone { phone_spellings(value) } else { email_spellings(value) };
        for (name, s) in spellings {
            let h = sha256_hex(&s);
            if own.contains(&h) {
                matched.insert(format!("{} {name}", if *phone { "phone" } else { "email" }));
            }
            hashes.insert(h);
        }
    }
    let dir = out.join("identity");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body: Vec<String> = hashes.iter().cloned().collect();
    std::fs::write(dir.join(HASHES_FILE), body.join("\n")).map_err(|e| e.to_string())?;
    let phones = ids.iter().filter(|(p, _)| *p).count();
    let summary = format!(
        "contacts: {cards} cards, {phones} phone numbers, {} email addresses, {} hashes; own record matched by: {}",
        ids.len() - phones,
        hashes.len(),
        if matched.is_empty() {
            "nothing (no card of yours?)".to_string()
        } else {
            matched.into_iter().collect::<Vec<_>>().join(", ")
        }
    );
    say!("{summary}");
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_carddav_report() {
        let xml = "<?xml version=\"1.0\"?><multistatus xmlns=\"DAV:\"><response><href>/123/carddavhome/card/a.vcf</href><propstat><prop>\
            <address-data xmlns=\"urn:ietf:params:xml:ns:carddav\">BEGIN:VCARD&#13;\nVERSION:3.0&#13;\nFN:Kim&#13;\nitem1.TEL;type=CELL;type=pref:010-1234-\n 5678&#13;\nEMAIL;type=INTERNET:Kim@Example.com&#13;\nEND:VCARD</address-data>\
            </prop></propstat></response></multistatus>";
        let cards = elements(xml, "address-data");
        assert_eq!(cards.len(), 1);
        let ids = card_identifiers(&unescape(cards[0]));
        assert_eq!(ids, vec![(true, "010-1234-5678".to_string()), (false, "Kim@Example.com".to_string())]);
        let home = "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:propstat><d:prop><c:addressbook-home-set xmlns:c=\"x\"><d:href>https://p1-contacts.icloud.com:443/123/carddavhome/</d:href></c:addressbook-home-set></d:prop></d:propstat></d:response></d:multistatus>";
        let h = elements(home, "addressbook-home-set");
        assert_eq!(elements(h[0], "href"), vec!["https://p1-contacts.icloud.com:443/123/carddavhome/"]);
        assert_eq!(
            absolute("https://p1-contacts.icloud.com:443/123/carddavhome/", "/123/carddavhome/card/"),
            "https://p1-contacts.icloud.com:443/123/carddavhome/card/"
        );
    }

    #[test]
    fn spells_phone_numbers_every_way() {
        let all: Vec<String> = phone_spellings("010-1234-5678").into_iter().map(|(_, s)| s).collect();
        assert_eq!(all, ["+821012345678", "821012345678", "01012345678", "01012345678"]);
        assert_eq!(phone_spellings("+82 10-1234-5678")[0].1, "+821012345678");
        assert_eq!(phone_spellings("+1 (415) 555-0100")[0].1, "+14155550100");
        assert_eq!(email_spellings(" Kim@Example.com ")[0].1, "kim@example.com");
    }
}
