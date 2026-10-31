//! 전송 속도, 그리고 진행 막대가 보여 줄 것 (비율, 양, 속도와 남은 시간).
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use wirdrop_island::Progress;

use crate::win::size;

/// 최근 몇 초의 진행: 바뀌는 속도를 튀지 않게 따라갑니다.
#[derive(Default)]
pub struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

/// 속도를 재는 구간.
const WINDOW: Duration = Duration::from_secs(3);

impl Rate {
    pub fn add(&mut self, done: u64) {
        let now = Instant::now();
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW) {
            self.samples.pop_front();
        }
    }

    /// 초당 바이트. 잴 만큼 모이기 전엔 None.
    pub fn speed(&self) -> Option<f64> {
        let (&(t0, b0), &(t1, b1)) = (self.samples.front()?, self.samples.back()?);
        let secs = t1.duration_since(t0).as_secs_f64();
        (secs >= 0.4).then(|| b1.saturating_sub(b0) as f64 / secs)
    }
}

/// 진행 막대: 비율, 오른쪽의 양, 아래의 속도와 남은 시간. 속도를 알기 전엔 idle을 보여 줍니다.
pub fn bar(done: u64, total: u64, speed: Option<f64>, idle: &str) -> Progress {
    let fraction = (total > 0).then(|| (done as f64 / total as f64).min(1.0) as f32);
    let amount = if total > 0 {
        format!("{} / {}", size(done.min(total)), size(total))
    } else {
        size(done)
    };
    let status = match speed.filter(|s| *s >= 1.0) {
        Some(s) if total > done => {
            let left = duration((total - done) as f64 / s);
            tf!("{}/s · {left} 남음", "{}/s · {left} left", size(s as u64))
        }
        Some(s) => format!("{}/s", size(s as u64)),
        None => idle.to_string(),
    };
    Progress { fraction, amount, status }
}

/// 끝을 모르는 진행 (도는 링): 글만 보입니다.
pub fn waiting(status: &str) -> Progress {
    Progress {
        fraction: None,
        amount: String::new(),
        status: status.into(),
    }
}

/// 끝난 전송의 요약 "79.8 MB · 6.3초 · 평균 12.7 MB/s" ("79.8 MB · 6.3 s · avg 12.7 MB/s"). took은
/// 수락부터 마지막 바이트까지입니다.
pub fn summary(bytes: u64, took: Duration) -> String {
    let secs = took.as_secs_f64();
    if bytes == 0 {
        String::new()
    } else if secs < 0.5 {
        size(bytes)
    } else {
        let (amount, took, speed) = (size(bytes), elapsed(secs), size((bytes as f64 / secs) as u64));
        tf!("{amount} · {took} · 평균 {speed}/s", "{amount} · {took} · avg {speed}/s")
    }
}

/// 1분 안쪽은 "6.3초" ("6.3 s"), 그 위는 [duration]처럼.
fn elapsed(secs: f64) -> String {
    if secs < 59.95 {
        tf!("{secs:.1}초", "{secs:.1} s")
    } else {
        duration(secs)
    }
}

/// "8초", "1분 20초", "1시간 5분" ("8 s", "1 min 20 s", "1 h 5 min").
fn duration(secs: f64) -> String {
    let s = secs.ceil().max(1.0) as u64;
    let (m, h) = (s / 60, s / 3600);
    match s {
        0..60 => tf!("{s}초", "{s} s"),
        60..3600 if s.is_multiple_of(60) => tf!("{m}분", "{m} min"),
        60..3600 => tf!("{m}분 {}초", "{m} min {} s", s % 60),
        _ => tf!("{h}시간 {}분", "{h} h {} min", s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_has_the_time() {
        let s = summary(79_751_788, Duration::from_millis(6340));
        assert!(s.contains(t!("6.3초", "6.3 s")) && s.contains(t!("평균", "avg")), "{s}");
        assert!(summary(79_751_788, Duration::from_secs(95)).contains(t!("1분 35초", "1 min 35 s")));
    }

    #[test]
    fn a_bar_says_how_far_and_how_fast() {
        let p = bar(25, 100, Some(10.0), "");
        assert_eq!(p.fraction, Some(0.25));
        assert!(p.status.contains(t!("남음", "left")), "{}", p.status);
        assert_eq!(bar(0, 0, None, "").fraction, None);
    }
}
