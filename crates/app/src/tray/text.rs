//! 카드와 메뉴에 쓰는 문구: 받을 것의 요약, 조사, 끊긴 까닭.
use std::path::PathBuf;

use crate::ipc::FileInfo;

/// "사진 3장", "동영상 1개", "사진·동영상 2개", "파일 2개", "링크 1개" (영어: "3 photos", ...).
pub fn describe(files: &[FileInfo]) -> String {
    let is = |f: &FileInfo, words: &[&str]| words.iter().any(|w| f.kind.to_ascii_lowercase().contains(w));
    let photos = files.iter().filter(|f| is(f, &["image", "jpeg", "heic", "png", "photo"])).count();
    let videos = files.iter().filter(|f| is(f, &["movie", "video", "mpeg-4", "quicktime"])).count();
    let links = files.iter().filter(|f| f.kind == "url").count();
    let n = files.len();
    let en = wirdrop_island::english();
    if links == n {
        if en { count(n, "link", "links") } else { format!("링크 {n}개") }
    } else if photos == n {
        if en { count(n, "photo", "photos") } else { format!("사진 {n}장") }
    } else if videos == n {
        if en { count(n, "video", "videos") } else { format!("동영상 {n}개") }
    } else if photos + videos == n {
        tf!("사진·동영상 {n}개", "{n} photos and videos")
    } else {
        if en { count(n, "file", "files") } else { format!("파일 {n}개") }
    }
}

/// 영어로 "1 file", "2 files".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// 받기 요청의 한 줄: "사진 3장을 보내려고 합니다", 영어로 "Wants to share 3 photos".
pub fn wants_to_share(what: &str) -> String {
    if wirdrop_island::english() {
        format!("Wants to share {what}")
    } else {
        format!("{what}{} 보내려고 합니다", eul(what))
    }
}

/// word의 마지막 글자에 받침이 있으면 "을", 없으면 "를".
fn eul(word: &str) -> &'static str {
    match word.chars().last() {
        Some(c @ '가'..='힣') if !(c as u32 - 0xac00).is_multiple_of(28) => "을",
        _ => "를",
    }
}

/// 받을 파일 이름: "IMG_1.HEIC", 여럿이면 "IMG_1.HEIC 외 2개" ("IMG_1.HEIC and 2 more").
pub fn names(files: &[FileInfo]) -> String {
    match files {
        [] => String::new(),
        [one] => one.name.clone(),
        [first, rest @ ..] => and_more(&first.name, rest.len()),
    }
}

/// 보내거나 받은 파일 이름: "영상.mov", 여럿이면 "영상.mov 외 2개".
pub fn file_line(files: &[PathBuf]) -> String {
    let name = files
        .first()
        .and_then(|f| f.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if files.len() > 1 { and_more(&name, files.len() - 1) } else { name }
}

fn and_more(first: &str, more: usize) -> String {
    tf!("{first} 외 {more}개", "{first} and {more} more")
}

/// 전송이 멈춘 까닭을 지금 언어의 짧은 말로. owly, 엔진, Windows의 문구(영어, 또는 os error 번호가
/// 붙은 Windows 언어의 글)를 쉬운 말로 바꿉니다.
pub fn reason_text(reason: &str) -> String {
    let r = reason.to_ascii_lowercase();
    // 폴더 안에 받는 기기가 못 받는 이름의 파일이 있습니다 (owly-airdrop: "<경로> has a name ...").
    for (mark, long) in [(" has a name too long", true), (" has a name the receiver can't take", false)] {
        if let Some(at) = reason.find(mark) {
            let path = std::path::Path::new(&reason[..at]);
            let name = path
                .file_name()
                .map_or_else(|| reason[..at].to_string(), |n| n.to_string_lossy().into_owned());
            return if long {
                tf!("이름이 너무 긴 파일이 있습니다: {name}", "A file's name is too long: {name}")
            } else {
                tf!("보낼 수 없는 이름의 파일이 있습니다: {name}", "A file's name can't be sent: {name}")
            };
        }
    }
    let said = if reason.is_empty() {
        return String::new();
    } else if r.contains("acceptance timed out") {
        t!("상대가 응답하지 않았습니다.", "The other device didn't answer.")
    } else if r.contains("timed out") || r.contains("os error 10060") {
        t!("응답이 없어 멈췄습니다.", "Stopped: no response.")
    } else if r.contains("upload refused") {
        t!("상대가 받지 않았습니다.", "The other device refused it.")
    } else if r.contains("stopped sending") {
        t!("보내는 쪽이 멈췄습니다.", "The sender stopped.")
    } else if r.contains("browsing stopped") {
        t!("기기 찾기가 멈췄습니다.", "Device search stopped.")
    } else if r.contains(crate::ipc::NOT_FOUND) {
        t!(
            "기기를 찾지 못했습니다. 화면을 켜고 AirDrop 수신을 확인해 주세요.",
            "Device not found. Wake its screen and check that AirDrop is on."
        )
    } else if r.contains(ENGINE_STOPPED) {
        t!("엔진이 멈췄습니다.", "The engine stopped.")
    } else if r.contains("os error 112") || r.contains("no space") {
        t!("저장 공간이 부족합니다.", "Not enough storage.")
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
        t!("연결이 끊겼습니다.", "The connection was lost.")
    } else {
        t!("전송이 중단되었습니다.", "The transfer stopped.")
    };
    said.to_string()
}

/// 트레이가 붙이는 까닭: 엔진이 멈췄습니다.
pub const ENGINE_STOPPED: &str = "engine stopped";

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
        assert_eq!(describe(&[f("public.jpeg"), f("public.heic")]), t!("사진 2장", "2 photos"));
        assert_eq!(describe(&[f("com.apple.quicktime-movie")]), t!("동영상 1개", "1 video"));
        assert_eq!(
            describe(&[f("public.jpeg"), f("public.mpeg-4")]),
            t!("사진·동영상 2개", "2 photos and videos")
        );
        assert_eq!(describe(&[f("com.adobe.pdf")]), t!("파일 1개", "1 file"));
        assert_eq!(eul("사진 2장"), "을");
        assert_eq!(eul("파일 1개"), "를");
    }

    #[test]
    fn reasons_are_said_plainly() {
        assert_eq!(
            reason_text("Connection reset by peer"),
            t!("연결이 끊겼습니다.", "The connection was lost.")
        );
        assert_eq!(reason_text(ENGINE_STOPPED), t!("엔진이 멈췄습니다.", "The engine stopped."));
        assert_eq!(
            reason_text("acceptance timed out"),
            t!("상대가 응답하지 않았습니다.", "The other device didn't answer.")
        );
        assert_eq!(
            reason_text("파일 쓰기 실패 (os error 112)"),
            t!("저장 공간이 부족합니다.", "Not enough storage.")
        );
        let long = r"C:\사진\여행 기록.jpg has a name too long for the receiver (300 bytes, at most 255)";
        assert_eq!(
            reason_text(long),
            tf!("이름이 너무 긴 파일이 있습니다: {}", "A file's name is too long: {}", "여행 기록.jpg")
        );
    }
}
