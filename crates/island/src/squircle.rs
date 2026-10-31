//! Apple의 연속 곡률 모서리("스쿼클"), 그리고 섬의 방울: 섬이 화면 가장자리에서 나오거나 들어갈 때
//! 액체 목으로 가장자리에 이어진 모습.
//!
//! 연속 곡률 모서리는 곧은 변에서 시작하는 짧은 ease-in, 원호, ease-out으로 이루어지고 모서리에서
//! 반지름의 1.528665배 떨어진 곳에서 시작합니다. 두 변에 그만한 자리가 있으면 수치는 Apple의 것
//! 그대로입니다 (UIBezierPath가 내놓는 대로: Liam Rosenfeld, "Apple icon quest", 2021, Apple 아이콘
//! 마스크와 0 px 차이; PaintCode, "Code for iOS 7 rounded rectangles"). 자리가 모자라면 UIKit은
//! 다른 모양으로 건너뛰어 애니메이션에서 티가 나므로, 여기서는 모서리의 각 변이 자리가 되는 만큼의
//! ease를 받습니다 (Figma의 구성, figma-squircle, MIT, 부드러움을 변마다 따로): 알약은 긴 변을 따라
//! Apple의 ease를 지키고 끝에서 반원에 가깝게 돌아, iOS가 그리는 알약처럼 크기에 따라 매끄럽게
//! 바뀝니다. 두 구성이 만나는 곳의 차이는 반지름의 0.0065배입니다.
//!
//! 모든 윤곽의 구간 구성이 같아서 합성기가 하나를 다른 것으로 바꿀 수 있습니다.

/// UIKit의 CALayer.cornerCurveExpansionFactor(.continuous).
pub const EXPANSION: f32 = 1.528665;

/// 반지름 단위의 Apple 모서리, 모서리로부터 (들어오는 변을 따라, 나가는 변을 따라): (EXPANSION,
/// 0)까지의 직선 뒤 세 3차 곡선의 제어점과 끝점.
const APPLE: [[(f32, f32); 3]; 3] = [
    [(1.088_493, 0.0), (0.868_407, 0.0), (0.631_493_8, 0.074_911_39)],
    [(0.372_823_8, 0.169_059_6), (0.169_059_6, 0.372_823_8), (0.074_911_39, 0.631_493_8)],
    [(0.0, 0.868_407), (0.0, 1.088_493), (0.0, EXPANSION)],
];

/// 반지름 단위의 사분원 3차 곡선 손잡이.
const CIRCLE: f32 = 0.552_284_8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

const fn pt(x: f32, y: f32) -> Pt {
    Pt { x, y }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Line(Pt),
    Cubic(Pt, Pt, Pt),
}

impl Seg {
    pub fn end(&self) -> Pt {
        match self {
            Seg::Line(p) | Seg::Cubic(_, _, p) => *p,
        }
    }

    fn map(&self, f: impl Fn(Pt) -> Pt) -> Seg {
        match *self {
            Seg::Line(p) => Seg::Line(f(p)),
            Seg::Cubic(a, b, c) => Seg::Cubic(f(a), f(b), f(c)),
        }
    }
}

/// 자기 좌표계의 모서리 하나: 모서리로부터 (u, v), u는 들어오는 변, v는 나가는 변을 따라. 직선의
/// 끝, 그다음 3차 곡선 셋.
type Corner = [(f32, f32); 10];

fn apple(r: f32) -> Corner {
    let mut c = [(EXPANSION * r, 0.0); 10];
    for (i, cubic) in APPLE.iter().enumerate() {
        for (j, (u, v)) in cubic.iter().enumerate() {
            c[1 + i * 3 + j] = (u * r, v * r);
        }
    }
    c
}

/// 들어오는 변을 따라 부드러움 xa, 나가는 변을 따라 xb인 모서리 (0이면 거기는 그냥 원, EXPANSION -
/// 1이 Apple의 거리): 각 ease가 사분원 회전의 45° × 부드러움을, 나머지는 (r, r)을 중심으로 한
/// 반지름 r의 원호가 맡습니다.
fn smooth(r: f32, xa: f32, xb: f32) -> Corner {
    let deg = std::f32::consts::PI / 180.0;
    // 변 하나의 ease: 변에서 시작하는 곳, 거기의 손잡이, 원호와 만나는 곳.
    let ease = |xi: f32| {
        let theta = 45.0 * xi * deg;
        let p = (1.0 + xi) * r;
        let c = r * (theta / 2.0).tan() * theta.cos();
        let m = (p - c - r * (1.0 - theta.sin())).max(0.0);
        let (a, b) = (2.0 * m / 3.0, m / 3.0);
        (theta, p, a, b)
    };
    let (ta, pa, aa, ba) = ease(xa);
    let (tb, pb, ab, bb) = ease(xb);
    let sa = (r - r * ta.sin(), r - r * ta.cos());
    let sb = (r - r * tb.cos(), r - r * tb.sin());
    let k = 4.0 / 3.0 * ((std::f32::consts::FRAC_PI_2 - ta - tb) / 4.0).tan() * r;
    [
        (pa, 0.0),
        (pa - aa, 0.0),
        (pa - aa - ba, 0.0),
        sa,
        (sa.0 - k * ta.cos(), sa.1 + k * ta.sin()),
        (sb.0 + k * tb.sin(), sb.1 - k * tb.cos()),
        sb,
        (0.0, pb - ab - bb),
        (0.0, pb - ab),
        (0.0, pb),
    ]
}

/// 변을 따라 room_a와 room_b까지 (각각의 절반) 뻗을 수 있는 반지름 r의 모서리.
fn corner(r: f32, room_a: f32, room_b: f32) -> Corner {
    if r <= 0.0 {
        return [(0.0, 0.0); 10];
    }
    if room_a >= EXPANSION * r && room_b >= EXPANSION * r {
        return apple(r);
    }
    let xi = |room: f32| (room / r - 1.0).clamp(0.0, EXPANSION - 1.0);
    smooth(r, xi(room_a), xi(room_b))
}

/// 반지름 r의 연속 곡률 모서리를 가진 사각형의 윤곽. 위 변에 있는 왼쪽 위 모서리의 끝에서 시계
/// 방향으로: 시작점과 구간 16개 (모서리마다 직선 하나와 3차 곡선 셋).
pub fn outline(x: f32, y: f32, w: f32, h: f32, r: f32) -> (Pt, [Seg; 16]) {
    outline4(x, y, w, h, [r; 4])
}

/// 모서리마다 반지름이 다른 [outline]: 오른쪽 위, 오른쪽 아래, 왼쪽 아래, 왼쪽 위.
pub fn outline4(x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) -> (Pt, [Seg; 16]) {
    let (w, h) = (w.max(0.0), h.max(0.0));
    let radii = radii.map(|r| r.clamp(0.0, w.min(h) / 2.0));
    // 모서리마다: 위치, 들어오는 변과 나가는 변의 방향, 그리고 그 자리.
    let corners = [
        ((x + w, y), (-1.0, 0.0), (0.0, 1.0), w / 2.0, h / 2.0),
        ((x + w, y + h), (0.0, -1.0), (-1.0, 0.0), h / 2.0, w / 2.0),
        ((x, y + h), (1.0, 0.0), (0.0, -1.0), w / 2.0, h / 2.0),
        ((x, y), (0.0, 1.0), (1.0, 0.0), h / 2.0, w / 2.0),
    ];
    let at = |(vx, vy): (f32, f32), da: (f32, f32), db: (f32, f32), (u, v): (f32, f32)| pt(vx + u * da.0 + v * db.0, vy + u * da.1 + v * db.1);
    let mut segs = [Seg::Line(pt(x, y)); 16];
    let mut start = pt(x, y);
    for (i, (vertex, da, db, room_a, room_b)) in corners.into_iter().enumerate() {
        let c = corner(radii[i], room_a, room_b);
        let p = |j: usize| at(vertex, da, db, c[j]);
        segs[i * 4] = Seg::Line(p(0));
        for k in 0..3 {
            segs[i * 4 + 1 + k] = Seg::Cubic(p(1 + k * 3), p(2 + k * 3), p(3 + k * 3));
        }
        if i == 3 {
            start = p(9);
        }
    }
    (start, segs)
}

/// 섬이 나오는 가장자리의 좌표계에서 본 방울: u는 가장자리를 따라, v는 가장자리에서 멀어지는 쪽
/// (가장자리는 v = 0). 몸통은 가장자리 방향으로 w, 깊이 h, 가까운 쪽이 top만큼 떨어져 있고 모서리는
/// r (가장자리 쪽 near). 액체 다리가 몸통을 가장자리에 잇고 가운데를 기준으로 양쪽이 같습니다:
/// 가장자리 위의 발에서 허리로 들어가 waist_end까지 내려가고 (다리가 끊긴 뒤 늘어난 허리: 그 사이는
/// 비어 있음) 몸통과 만나는 곳으로 나가며, 곡선마다 오목한 사분원입니다 (점은 (가운데로부터의 거리,
/// v)). edge가 false면 가장자리도 다리도 없습니다.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drop {
    pub center: f32,
    pub top: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub near: f32,
    pub edge: bool,
    pub foot: (f32, f32),
    pub waist: (f32, f32),
    pub waist_end: f32,
    pub join: (f32, f32),
}

/// 윤곽이 가장자리 너머로 나가는 거리: 창에 잘려 그려지지 않게.
const PAST: f32 = 2.0;

/// 방울의 윤곽: 시작점과 구간 26개 (모양과 상관없이).
pub fn drop(d: &Drop) -> (Pt, [Seg; 26]) {
    let x = d.center - d.w / 2.0;
    let near = if d.edge { d.near } else { d.r };
    let (_, body) = outline4(x, d.top, d.w, d.h, [near, d.r, d.r, near]);
    let c = d.center;
    let (foot, waist, waist_end, join, past) = if d.edge {
        (d.foot, d.waist, d.waist_end, d.join, -PAST)
    } else {
        ((0.0, d.top), (0.0, d.top), d.top, (0.0, d.top), d.top)
    };
    let k = CIRCLE;
    // 한쪽 다리를 내려가며 (side 1.0 오른쪽, -1.0 왼쪽).
    let side = |sign: f32| {
        let at = |(u, v): (f32, f32)| pt(c + sign * u, v);
        let f = at(foot);
        let w1 = at(waist);
        let w2 = at((waist.0, waist_end));
        let j = at(join);
        let into_waist = Seg::Cubic(pt(f.x + (w1.x - f.x) * k, f.y), pt(w1.x, w1.y - (w1.y - f.y) * k), w1);
        let out_to_join = Seg::Cubic(pt(w2.x, w2.y + (j.y - w2.y) * k), pt(j.x - (j.x - w2.x) * k, j.y), j);
        (f, into_waist, w2, out_to_join)
    };
    let (rf, r1, rw2, r2) = side(1.0);
    let (lf, l1, lw2, l2) = side(-1.0);
    let reverse = |s: Seg, from: Pt| match s {
        Seg::Cubic(a, b, _) => Seg::Cubic(b, a, from),
        line => line,
    };
    let start = pt(lf.x, past);
    let mut segs = [Seg::Line(start); 26];
    segs[0] = Seg::Line(pt(rf.x, past));
    segs[1] = Seg::Line(rf);
    segs[2] = r1;
    segs[3] = Seg::Line(rw2);
    segs[4] = r2;
    segs[5..21].copy_from_slice(&body);
    segs[21] = Seg::Line(l2.end());
    // 왼쪽을 올라가며: 오른쪽 곡선을 뒤집어 거꾸로 돌립니다.
    segs[22] = reverse(l2, lw2);
    segs[23] = Seg::Line(l1.end());
    segs[24] = reverse(l1, lf);
    segs[25] = Seg::Line(start);
    (start, segs)
}

/// 윤곽(시작점과 구간)을 f로 옮깁니다.
pub fn map<const N: usize>((start, segs): (Pt, [Seg; N]), f: impl Fn(Pt) -> Pt) -> (Pt, [Seg; N]) {
    (f(start), segs.map(|s| s.map(&f)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roomy_corners_are_apples() {
        let (start, segs) = outline(0.0, 0.0, 408.0, 146.0, 44.0);
        assert!((start.x - EXPANSION * 44.0).abs() < 1e-3 && start.y == 0.0);
        let e = segs[3].end();
        assert!((e.x - 408.0).abs() < 1e-3 && (e.y - EXPANSION * 44.0).abs() < 1e-3);
        let q = segs[2].end();
        assert!((408.0 - q.x - 0.074_911_39 * 44.0).abs() < 1e-3);
    }

    #[test]
    fn a_pill_eases_in_along_its_long_side_and_rounds_its_ends() {
        let h = 36.67;
        let r = h / 2.0;
        let (_, segs) = outline(0.0, 0.0, 230.0, h, r);
        // 곡선은 긴 변을 따라 끝에서 Apple의 1.528665 r만큼에서 시작해...
        let along = segs[0].end();
        assert!((230.0 - along.x - EXPANSION * r).abs() < 1e-3, "{along:?}");
        // ...평평한 데 없이 똑바로 서서 끝의 가운데에 닿습니다.
        let tip = segs[3].end();
        assert!((tip.x - 230.0).abs() < 1e-3 && (tip.y - r).abs() < 1e-3);
        if let Seg::Cubic(_, c2, _) = segs[3] {
            assert!((c2.x - 230.0).abs() < 1e-3);
        }
        // 그 사이의 원호는 끝의 원 위에 있습니다.
        let c = (230.0 - r, r);
        for s in &segs[2..3] {
            let p = s.end();
            let d = ((p.x - c.0).powi(2) + (p.y - c.1).powi(2)).sqrt();
            assert!((d - r).abs() < 0.01, "{p:?} is {d} from the center");
        }
    }

    #[test]
    fn closed_and_inside() {
        for (w, h, r) in [(408.0, 120.0, 44.0), (230.0, 36.67, 18.33), (300.0, 100.0, 44.0), (44.0, 44.0, 22.0)] {
            let (start, segs) = outline(10.0, 5.0, w, h, r);
            let last = segs[15].end();
            assert!(
                (last.x - start.x).abs() < 1e-3 && (last.y - start.y).abs() < 1e-3,
                "{w}x{h}: {last:?} vs {start:?}"
            );
            for s in &segs {
                let p = s.end();
                assert!(p.x >= 10.0 - 1e-3 && p.x <= 10.0 + w + 1e-3 && p.y >= 5.0 - 1e-3 && p.y <= 5.0 + h + 1e-3);
            }
        }
    }

    #[test]
    fn a_drop_is_closed_and_its_neck_joins_the_edge() {
        let d = Drop {
            center: 228.0,
            top: 11.0,
            w: 230.0,
            h: 36.67,
            r: 18.33,
            near: 18.33,
            edge: true,
            foot: (30.0, 0.0),
            waist: (14.0, 5.5),
            waist_end: 5.5,
            join: (24.0, 11.0),
        };
        let (start, segs) = drop(&d);
        assert_eq!(segs[25].end(), start);
        // 발은 가장자리에, 허리는 틈에, 이음은 몸통에. 양쪽이 같습니다.
        assert!(segs[1].end().y == 0.0 && (segs[4].end().y - 11.0).abs() < 1e-4);
        assert!((segs[24].end().x - (228.0 - 30.0)).abs() < 1e-4 && (segs[23].end().x - (228.0 - 14.0)).abs() < 1e-4);
        // 가장자리를 벗어나면 몸통뿐입니다.
        let free = drop(&Drop { edge: false, ..d });
        assert_eq!(free.1.len(), segs.len());
        assert!(free.1.iter().all(|s| s.end().y >= 11.0 - 1e-3));
    }
}
