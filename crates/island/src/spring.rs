//! SwiftUI가 정의하는 스프링 (응답 시간과 감쇠 비율, 질량 1). 합성기가 스스로 재생하도록 키
//! 프레임으로 뽑으므로, 섬이 움직이는 동안 우리 쪽에서는 아무것도 돌지 않습니다. 새 목표로 보낸
//! 스프링은 지금 자리에서 지금 속도로 출발합니다.
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// 감쇠 없는 주기(초).
    pub response: f64,
    /// 1이면 넘치지 않고 멈추고, 작을수록 튑니다.
    pub damping: f64,
}

impl Spring {
    /// 열기 (boring.notch의 값, iPhone의 섬과 잘 맞습니다): 조금 튑니다.
    pub const OPEN: Spring = Spring {
        response: 0.42,
        damping: 0.8,
    };
    /// 닫기: 튀지 않습니다.
    pub const CLOSE: Spring = Spring {
        response: 0.45,
        damping: 1.0,
    };
    /// 내용과 작은 변화 (SwiftUI의 .smooth보다 조금 빠르게).
    pub const SMOOTH: Spring = Spring {
        response: 0.35,
        damping: 1.0,
    };

    /// 멈춰 있던 x0에서 속도 v0으로 출발하고 t초 뒤의 정지점으로부터의 변위와 속도.
    pub fn at(&self, x0: f64, v0: f64, t: f64) -> (f64, f64) {
        let w0 = std::f64::consts::TAU / self.response;
        let z = self.damping.clamp(0.05, 1.0);
        if z > 0.9999 {
            let b = v0 + w0 * x0;
            let e = (-w0 * t).exp();
            (e * (x0 + b * t), e * (b - w0 * (x0 + b * t)))
        } else {
            let wd = w0 * (1.0 - z * z).sqrt();
            let b = (v0 + z * w0 * x0) / wd;
            let e = (-z * w0 * t).exp();
            let (s, c) = (wd * t).sin_cos();
            (e * (x0 * c + b * s), e * ((b * wd - z * w0 * x0) * c - (x0 * wd + z * w0 * b) * s))
        }
    }

    /// 정지점에서 허용 오차 안에 머물기까지의 초, 최대 2 (Apple의 섬 애니메이션 한도).
    pub fn settle(&self, x0: f64, v0: f64, tolerance: f64) -> f64 {
        const STEP: f64 = 1.0 / 240.0;
        let mut last = 0.0;
        let mut t = 0.0;
        while t <= 2.0 {
            let (x, v) = self.at(x0, v0, t);
            if x.abs() > tolerance || v.abs() > tolerance * 10.0 {
                last = t;
            }
            t += STEP;
        }
        (last + STEP).min(2.0)
    }

    /// 멈춰 있다가 출발하는 것을 위한 0에서 1까지의 키 프레임 (진행 0~1, 값).
    pub fn unit_frames(&self) -> (Vec<(f32, f32)>, f64) {
        let mut m = Motion::rest(0.0);
        match m.go(1.0, *self, 0.002, Instant::now()) {
            Go::Frames(frames, secs) => (frames, secs),
            Go::Jump(_) => (vec![(0.0, 0.0), (1.0, 1.0)], 0.01),
        }
    }
}

/// 스프링으로 움직이는 값 하나: 어디로 가는지와 지금 스프링이 어떻게 출발했는지를 둬서 언제든 값과
/// 속도를 알 수 있습니다.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    target: f64,
    x0: f64,
    v0: f64,
    start: Instant,
    spring: Spring,
    length: f64,
}

/// 합성기가 가진 값의 사본에 할 일.
pub enum Go {
    /// 설정합니다: 이미 거기 있습니다 (또는 충분히 가깝습니다).
    Jump(f32),
    /// 이 키 프레임(진행, 값)을 이만큼의 초 동안 재생합니다.
    Frames(Vec<(f32, f32)>, f64),
}

/// 초당 키 프레임 수: 합성기는 그 사이를 직선으로 잇습니다.
const FRAMES_PER_SECOND: f64 = 60.0;

impl Motion {
    pub fn rest(value: f64) -> Self {
        Motion {
            target: value,
            x0: 0.0,
            v0: 0.0,
            start: Instant::now(),
            spring: Spring::SMOOTH,
            length: 0.0,
        }
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// now의 값과 속도.
    pub fn at(&self, now: Instant) -> (f64, f64) {
        let t = now.saturating_duration_since(self.start).as_secs_f64();
        if t >= self.length {
            return (self.target, 0.0);
        }
        let (x, v) = self.spring.at(self.x0, self.v0, t);
        (self.target + x, v)
    }

    /// 지금부터 멈출 때까지의 초.
    pub fn left(&self, now: Instant) -> f64 {
        (self.start + std::time::Duration::from_secs_f64(self.length))
            .saturating_duration_since(now)
            .as_secs_f64()
    }

    /// 지금 어디에 있든 spring으로 to를 향합니다. tolerance는 값의 단위입니다.
    pub fn go(&mut self, to: f64, spring: Spring, tolerance: f64, now: Instant) -> Go {
        let (value, velocity) = self.at(now);
        let x0 = value - to;
        if x0.abs() <= tolerance && velocity.abs() <= tolerance * 10.0 {
            *self = Motion::rest(to);
            return Go::Jump(to as f32);
        }
        let length = spring.settle(x0, velocity, tolerance);
        let n = ((length * FRAMES_PER_SECOND).ceil() as usize).clamp(2, 120);
        let frames = (0..=n)
            .map(|i| {
                let p = i as f64 / n as f64;
                let v = if i == n { to } else { to + spring.at(x0, velocity, length * p).0 };
                (p as f32, v as f32)
            })
            .collect();
        *self = Motion {
            target: to,
            x0,
            v0: velocity,
            start: now,
            spring,
            length,
        };
        Go::Frames(frames, length)
    }

    /// 곧바로 value에 둡니다.
    pub fn set(&mut self, value: f64) {
        *self = Motion::rest(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn open_overshoots_a_little_and_settles() {
        let (frames, secs) = Spring::OPEN.unit_frames();
        let peak = frames.iter().map(|f| f.1).fold(0.0f32, f32::max);
        assert!(peak > 1.005 && peak < 1.05, "peak {peak}");
        assert!(secs > 0.3 && secs < 1.2, "{secs}");
        assert_eq!(frames.last().map(|f| f.1), Some(1.0));
    }

    #[test]
    fn close_never_overshoots() {
        let (frames, _) = Spring::CLOSE.unit_frames();
        assert!(frames.iter().all(|f| f.1 <= 1.0 + 1e-4));
    }

    #[test]
    fn a_new_target_starts_from_the_current_value_and_speed() {
        let t0 = Instant::now();
        let mut m = Motion::rest(126.0);
        let Go::Frames(..) = m.go(408.0, Spring::OPEN, 0.25, t0) else { panic!() };
        let mid = t0 + Duration::from_millis(120);
        let (value, velocity) = m.at(mid);
        assert!(value > 126.0 && value < 408.0 && velocity > 0.0);
        let Go::Frames(frames, _) = m.go(230.0, Spring::CLOSE, 0.25, mid) else {
            panic!()
        };
        assert!((frames[0].1 as f64 - value).abs() < 0.01);
        // 아직 바깥으로 가는 중: 다음 프레임은 돌아서기 전에 더 바깥입니다.
        assert!(frames[1].1 as f64 > value);
    }
}
