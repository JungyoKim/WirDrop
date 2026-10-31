//! 카드와 메뉴에 쓰는 한국어 문구: 받을 것의 요약, 조사, 끊긴 까닭.
use std::path::PathBuf;

use crate::ipc::FileInfo;

/// "사진 3장", "동영상 1개", "사진·동영상 2개", "파일 2개", "링크 1개".
pub fn describe(files: &[FileInfo]) -> String {
    let is = |f: &FileInfo, words: &[&str]| words.iter().any(|w| f.kind.to_ascii_lowercase().contains(w));
    let photos = files.iter().filter(|f| is(f, &["image", "jpeg", "heic", "png", "photo"])).count();
    let videos = files.iter().filter(|f| is(f, &["movie", "video", "mpeg-4", "quicktime"])).count();
    let links = files.iter().filter(|f| f.kind == "url").count();
    let n = files.len();
    if links == n {
        format!("링크 {n}개")
    } else if photos == n {
        format!("사진 {n}장")
    } else if videos == n {
        format!("동영상 {n}개")
    } else if photos + videos == n {
        format!("사진·동영상 {n}개")
    } else {
        format!("파일 {n}개")
    }
}

/// word의 마지막 글자에 받침이 있으면 "을", 없으면 "를".
pub fn eul(word: &str) -> &'static str {
    match word.chars().last() {
        Some(c @ '가'..='힣') if !(c as u32 - 0xac00).is_multiple_of(28) => "을",
        _ => "를",
    }
}

/// 받을 파일 이름: "IMG_1.HEIC", 여럿이면 "IMG_1.HEIC 외 2개".
pub fn names(files: &[FileInfo]) -> String {
    match files {
        [] => String::new(),
        [one] => one.name.clone(),
        [first, rest @ ..] => format!("{} 외 {}개", first.name, rest.len()),
    }
}

/// 보내거나 받은 파일 이름: "영상.mov", 여럿이면 "영상.mov 외 2개".
pub fn file_line(files: &[PathBuf]) -> String {
    let name = files
        .first()
        .and_then(|f| f.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if files.len() > 1 {
        format!("{name} 외 {}개", files.len() - 1)
    } else {
        name
    }
}

/// 전송이 멈춘 까닭을 짧은 한국어로. 우리 문구는 이미 한국어이고, owly와 Windows의 문구(영어, 또는
/// os error 번호가 붙은 한국어)는 쉬운 말로 바꿉니다.
pub fn reason_text(reason: &str) -> String {
    let r = reason.to_ascii_lowercase();
    let korean = reason.chars().any(|c| ('가'..='힣').contains(&c));
    if reason.is_empty() || (korean && !r.contains("os error")) {
        reason.to_string()
    } else if r.contains("acceptance timed out") {
        "상대가 응답하지 않았습니다.".into()
    } else if r.contains("timed out") || r.contains("os error 10060") {
        "응답이 없어 멈췄습니다.".into()
    } else if r.contains("upload refused") {
        "상대가 받지 않았습니다.".into()
    } else if r.contains("stopped sending") {
        "보내는 쪽이 멈췄습니다.".into()
    } else if r.contains("browsing stopped") {
        "기기 찾기가 멈췄습니다.".into()
    } else if r.contains("os error 112") || r.contains("no space") {
        "저장 공간이 부족합니다.".into()
    } else if [
        "os error 1005",
        "os error 10061",
        "connection",
        "closed",
        "eof",
        "close_notify",
        "broken pipe",
        "reset",
    ]
    .iter()
    .any(|k| r.contains(k))
    {
        "연결이 끊겼습니다.".into()
    } else {
        "전송이 중단되었습니다.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(kind: &str) -> FileInfo {
        FileInfo {
            name: "x".into(),
            size: None,
            kind: kind.into(),
        }
    }

    #[test]
    fn describes_offers() {
        assert_eq!(describe(&[f("public.jpeg"), f("public.heic")]), "사진 2장");
        assert_eq!(describe(&[f("com.apple.quicktime-movie")]), "동영상 1개");
        assert_eq!(describe(&[f("public.jpeg"), f("public.mpeg-4")]), "사진·동영상 2개");
        assert_eq!(describe(&[f("com.adobe.pdf")]), "파일 1개");
        assert_eq!(eul("사진 2장"), "을");
        assert_eq!(eul("파일 1개"), "를");
    }

    #[test]
    fn reasons_are_said_plainly() {
        assert_eq!(reason_text("Connection reset by peer"), "연결이 끊겼습니다.");
        assert_eq!(reason_text("엔진이 멈췄습니다."), "엔진이 멈췄습니다.");
        assert_eq!(reason_text("acceptance timed out"), "상대가 응답하지 않았습니다.");
    }
}
