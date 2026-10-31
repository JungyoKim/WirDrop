//! 화면 문구의 언어: Windows 표시 언어가 한국어면 한국어, 아니면 영어이고, WIRDROP_LANG=ko|en으로
//! 정할 수 있습니다 ([wirdrop_island::english]). 문구는 두 언어를 나란히 적습니다.
//!
//!     t!("받기", "Receive")
//!     tf!("{sender}에서 받는 중", "Receiving from {sender}")

/// 지금 언어의 문구 (한국어, 영어 순).
macro_rules! t {
    ($ko:expr, $en:expr $(,)?) => {
        if wirdrop_island::english() { $en } else { $ko }
    };
}

/// 지금 언어의 format! (한국어, 영어 순). 두 형식은 같은 값을 씁니다.
macro_rules! tf {
    ($ko:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if wirdrop_island::english() { format!($en $(, $arg)*) } else { format!($ko $(, $arg)*) }
    };
}
