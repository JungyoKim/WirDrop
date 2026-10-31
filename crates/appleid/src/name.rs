//! WirDrop 메뉴에 쓰는 계정 이름 (로그인의 다른 파일 옆 name.json): 그 사람의 이름과 로그인한 Apple
//! ID. 둘 다 비밀이 아니므로 평범한 JSON입니다.

use std::path::Path;

const NAME_FILE: &str = "name.json";

/// "홍길동" for a Korean, Chinese or Japanese name (family name first, no space), else "Jane Doe".
pub fn full_name(first: &str, last: &str) -> String {
    let (first, last) = (first.trim(), last.trim());
    let east_asian = |s: &str| {
        s.chars().any(|c| matches!(c, '\u{1100}'..='\u{11FF}' | '\u{3040}'..='\u{30FF}' | '\u{3130}'..='\u{318F}' | '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}'))
    };
    match (first.is_empty(), last.is_empty()) {
        (true, _) => last.to_string(),
        (_, true) => first.to_string(),
        _ if east_asian(first) || east_asian(last) => format!("{last}{first}"),
        _ => format!("{first} {last}"),
    }
}

/// 읽히는 그대로의 사람 이름. 비었거나 주소나 번호면 None.
fn usable(name: Option<&str>) -> Option<&str> {
    name.map(str::trim)
        .filter(|n| !n.is_empty() && !n.contains('@') && !n.chars().all(|c| c.is_ascii_digit() || c == '+'))
}

/// name과 account를 저장합니다 (어느 쪽이든 빠질 수 있음). keep_name이면 이미 저장한 이름은 그대로
/// 둡니다: 로그인이 주는 이름이 계정 자신의 것이고, 다른 데서 찾은 것은 빈자리만 채웁니다.
pub fn save(out: &Path, name: Option<&str>, account: Option<&str>, keep_name: bool) {
    let path = out.join(NAME_FILE);
    let mut v: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let had_name = v.get("name").and_then(|n| n.as_str()).is_some_and(|n| !n.is_empty());
    if let Some(name) = usable(name)
        && !(keep_name && had_name)
    {
        v.insert("name".into(), name.into());
    }
    if let Some(account) = account.map(str::trim).filter(|a| !a.is_empty()) {
        v.insert("account".into(), account.into());
    }
    let _ = std::fs::write(path, serde_json::Value::Object(v).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_names_as_they_read() {
        assert_eq!(full_name("길동", "홍"), "홍길동");
        assert_eq!(full_name("Jane", "Doe"), "Jane Doe");
        assert_eq!(full_name("", "Doe"), "Doe");
        assert_eq!(full_name(" Jane ", ""), "Jane");
        assert_eq!(usable(Some("someone@icloud.com")), None);
        assert_eq!(usable(Some("12345678")), None);
        assert_eq!(usable(Some(" Jane Doe ")), Some("Jane Doe"));
    }
}
