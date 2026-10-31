//! 합성기 쪽. 섬의 윤곽은 경로(Apple의 연속 곡률 모서리, squircle.rs 참고)이고, 합성기가 여기서
//! 뽑은 키 프레임을 따라 바꿉니다: 스프링이 크기, 모서리, 쉬는 자리에서의 거리를 움직입니다. 화면
//! 가장자리에 붙어 있으면 그 가장자리에서 액체처럼 나오며 빠져나오는 동안 가장자리에
//! 번지고(메니스커스), 돌아갈 때는 한 점으로 줄어듭니다. 다른 곳에서는 한 점에서 자라고 한 점으로
//! 줄어듭니다. 같은 경로가 함께 움직이는 내용을 잘라 냅니다. 보여 주는 카드마다 face가 하나
//! 있습니다: 그 그림과, 합성기가 스스로 움직이는 부분 (face의 P를 따르는 막대와 링, 계속 도는
//! 스피너).
use std::time::{Duration, Instant};

use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::{
    AnimationIterationBehavior, CompositionDrawingSurface, CompositionEasingFunction, CompositionObject, CompositionPath, CompositionPathGeometry,
    CompositionPropertySet, CompositionStretch, CompositionStrokeCap, Compositor, ContainerVisual, ShapeVisual, SpriteVisual,
};
use windows::core::{HSTRING, Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use crate::Position;
use crate::gfx::{Gfx, tint_hex};
use crate::layout::{self, CANVAS_H, CANVAS_W, EDGE, Face, Rect};
use crate::spring::{Go, Motion, Spring};
use crate::squircle::{self, Drop, Pt};

fn color(hex: u32, a: f32) -> Color {
    Color {
        A: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
        R: (hex >> 16) as u8,
        G: (hex >> 8) as u8,
        B: hex as u8,
    }
}

fn span(secs: f64) -> TimeSpan {
    TimeSpan {
        Duration: (secs * 10_000_000.0) as i64,
    }
}

/// 작아지는 스프링: 튀지 않고 열기보다 조금 느리게.
const SHRINK: Spring = Spring {
    response: 0.45,
    damping: 0.92,
};
/// 사라지기: 튀지 않고 한 점까지 줄어듭니다.
const AWAY: Spring = Spring { response: 0.4, damping: 1.0 };
/// 숫자(0.5초마다)를 따라가는 진행 막대나 링: 튀지 않고 조금 뒤처져 따라갑니다.
const FOLLOW: Spring = Spring { response: 0.8, damping: 1.0 };
/// 윤곽의 초당 키 프레임 (합성기는 그 사이를 직선으로 잇습니다): 화면 새로 고침 빈도(프레임마다
/// 하나), 이 범위 안에서.
const RATES: (f64, f64) = (60.0, 240.0);
/// 가장자리에서 나오기 시작하는 방울: 가장자리를 따라, 바깥으로, 그리고 모서리.
const DROP_ALONG: f32 = 64.0;
const DROP_OUT: f32 = 26.0;
const DROP_R: f32 = 13.0;
/// 화면 가운데에서 아무것도 없는 데서 나오는 섬, 그리고 사라질 때 남는 것: 한 점.
const POINT: f32 = 1.0;
/// 액체가 방울 옆을 지나 가장자리를 따라 번지는 최대 거리.
const MENISCUS: f32 = 16.0;
/// 키보드 포커스 링: 버튼과의 틈과 테 두께 (pt).
const FOCUS_GAP: f32 = 2.5;
const FOCUS_STROKE: f32 = 2.5;

/// 가장자리 좌표계에서의 방울. 가장자리를 빠져나오는 동안 그쪽 모서리는 각지고 옆면은 액체가 벽을
/// 적시듯 가장자리로 번집니다(메니스커스). 반쯤 나왔을 때 가장 많이, 다 빠져나오면 없습니다.
/// 가장자리를 벗어나면 섬만 남고 모서리는 다시 둥급니다.
fn liquid(center: f32, top: f32, along: f32, out: f32, r: f32) -> Drop {
    let base = Drop {
        center,
        top,
        w: along,
        h: out,
        r,
        near: r,
        waist_end: top,
        ..Drop::default()
    };
    if top > 0.0 {
        return base;
    }
    let shown = (top + out).max(0.0);
    let m = MENISCUS.min(shown * 0.6).min(-top);
    let side = along / 2.0;
    Drop {
        edge: true,
        near: 0.0,
        foot: (side + m, 0.0),
        waist: (side, m),
        waist_end: m,
        join: (side, m),
        ..base
    }
}

pub struct FaceVisual {
    pub root: ContainerVisual,
    art: SpriteVisual,
    surface: CompositionDrawingSurface,
    props: CompositionPropertySet,
    hovers: Vec<ShapeVisual>,
    /// 키보드로 고른 버튼을 두르는 링 (버튼마다 하나, 평소엔 투명).
    rings: Vec<ShapeVisual>,
    pub face: Face,
    /// 창 안에서 face가 놓이는 곳(pt, 섬이 다 나왔을 때).
    pub origin: Vector2,
    leaving: Option<Instant>,
    /// 스프링 위의 막대나 링 진행.
    progress: Motion,
    /// 입력칸의 캐럿 (Apple의 파란색, 깜빡임).
    caret: Option<SpriteVisual>,
}

pub struct Scene {
    comp: Compositor,
    root: ContainerVisual,
    /// A: 섬의 불투명도 (화면 가운데에서만 흐려집니다).
    island: CompositionPropertySet,
    outline: CompositionPathGeometry,
    clip: CompositionPathGeometry,
    /// Apple이 어두운 모드에서 섬 둘레에 그리는 테두리 선. 몸통에만 두르고, 섬이 가장자리를
    /// 벗어나야 보입니다.
    rim: ShapeVisual,
    rim_path: CompositionPathGeometry,
    rim_stroke: windows::UI::Composition::CompositionSpriteShape,
    /// 섬이 나오고 들어갈 때 함께 움직이는 face들.
    mover: ContainerVisual,
    linear: CompositionEasingFunction,
    ease_out: CompositionEasingFunction,
    ease_in: CompositionEasingFunction,
    w: Motion,
    h: Motion,
    r: Motion,
    /// 몸통이 쉬는 자리에서 가장자리 쪽으로 (그리고 그 너머로) 떨어진 거리.
    d: Motion,
    /// 섬이 붙어 있으면, 섬이 나오는 가장자리 (화면 밖으로 나가는 방향).
    out: Option<(f32, f32)>,
    /// 나오는 중: 다음 face는 섬이 다 나올 때까지 기다립니다.
    emerging: bool,
    shown: bool,
    faces: Vec<FaceVisual>,
    scale: f32,
    /// 윤곽의 초당 키 프레임.
    rate: f64,
    position: Position,
}

impl Scene {
    pub fn new(comp: &Compositor, root: &ContainerVisual) -> Result<Scene> {
        let island = comp.CreatePropertySet()?;
        island.InsertScalar(h!("A"), 0.0)?;
        let linear: CompositionEasingFunction = comp.CreateLinearEasingFunction()?.cast()?;
        // Apple의 ease out과 ease in (CAMediaTimingFunction의 것).
        let ease_out: CompositionEasingFunction = comp
            .CreateCubicBezierEasingFunction(Vector2::new(0.0, 0.0), Vector2::new(0.58, 1.0))?
            .cast()?;
        let ease_in: CompositionEasingFunction = comp
            .CreateCubicBezierEasingFunction(Vector2::new(0.42, 0.0), Vector2::new(1.0, 1.0))?
            .cast()?;
        let canvas = Vector2::new(CANVAS_W, CANVAS_H);

        let outline = comp.CreatePathGeometry()?;
        let fill = comp.CreateSpriteShapeWithGeometry(&outline)?;
        fill.SetFillBrush(&comp.CreateColorBrushWithColor(color(0, 1.0))?)?;
        let pill = comp.CreateShapeVisual()?;
        pill.SetSize(canvas)?;
        pill.Shapes()?.Append(&fill)?;

        let rim_path = comp.CreatePathGeometry()?;
        let rim_stroke = comp.CreateSpriteShapeWithGeometry(&rim_path)?;
        rim_stroke.SetStrokeBrush(&comp.CreateColorBrushWithColor(color(0xFFFFFF, 0.14))?)?;
        rim_stroke.SetStrokeThickness(1.0)?;
        let rim = comp.CreateShapeVisual()?;
        rim.SetSize(canvas)?;
        rim.Shapes()?.Append(&rim_stroke)?;
        rim.SetOpacity(0.0)?;

        let clip = comp.CreatePathGeometry()?;
        let clipper = comp.CreateContainerVisual()?;
        clipper.SetSize(canvas)?;
        clipper.SetClip(&comp.CreateGeometricClipWithGeometry(&clip)?)?;
        let mover = comp.CreateContainerVisual()?;
        mover.SetSize(canvas)?;
        clipper.Children()?.InsertAtTop(&mover)?;
        let body = comp.CreateContainerVisual()?;
        body.SetSize(canvas)?;
        body.Children()?.InsertAtTop(&pill)?;
        body.Children()?.InsertAtTop(&rim)?;
        root.Children()?.InsertAtTop(&body)?;
        root.Children()?.InsertAtTop(&clipper)?;

        let scene = Scene {
            comp: comp.clone(),
            root: root.clone(),
            island,
            outline,
            clip,
            rim,
            rim_path,
            rim_stroke,
            mover,
            linear,
            ease_out,
            ease_in,
            w: Motion::rest(DROP_ALONG as f64),
            h: Motion::rest(DROP_OUT as f64),
            r: Motion::rest(DROP_R as f64),
            d: Motion::rest(0.0),
            out: Position::Top.edge([true; 4]),
            emerging: false,
            shown: false,
            faces: Vec::new(),
            scale: 1.0,
            rate: RATES.0,
            position: Position::Top,
        };
        scene.follow(&body, "Opacity", "i.A")?;
        scene.follow(&clipper, "Opacity", "i.A")?;
        Ok(scene)
    }

    /// target.property가 섬의 수치(i)로 된 식을 따릅니다.
    fn follow<T: Interface>(&self, target: &T, property: &str, expression: &str) -> Result<()> {
        let a = self.comp.CreateExpressionAnimationWithExpression(&HSTRING::from(expression))?;
        a.SetReferenceParameter(h!("i"), &self.island)?;
        target.cast::<CompositionObject>()?.StartAnimation(&HSTRING::from(property), &a)
    }

    /// pt를 픽셀로: 창의 DPI.
    pub fn set_scale(&mut self, scale: f32) -> Result<()> {
        self.scale = scale;
        self.root.SetScale(Vector3::new(scale, scale, 1.0))?;
        // 1 픽셀.
        self.rim_stroke.SetStrokeThickness(1.0 / scale)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// 화면 새로 고침 빈도 (초당 프레임).
    pub fn set_rate(&mut self, hz: f64) {
        self.rate = hz.clamp(RATES.0, RATES.1);
    }

    /// 섬이 나오는 자리와 거기서 쓸 수 있는 화면 가장자리 (위, 아래, 왼쪽, 오른쪽: 화면 가장자리면
    /// true, 작업 표시줄이면 false). 숨어 있을 때만.
    pub fn set_position(&mut self, position: Position, bezels: [bool; 4]) {
        self.position = position;
        self.out = position.edge(bezels);
    }

    /// 화면 위 방울의 크기: 긴 변이 가장자리를 따라.
    fn drop_size(&self) -> (f32, f32) {
        match self.out {
            Some((dx, _)) if dx != 0.0 => (DROP_OUT, DROP_ALONG),
            Some(_) => (DROP_ALONG, DROP_OUT),
            None => (40.0, 40.0),
        }
    }

    /// 방울이 가장자리 안에 다 들어가 있을 때 바깥으로 나온 거리.
    fn out_of_sight(&self) -> f32 {
        if self.out.is_some() { EDGE + DROP_OUT + 2.0 } else { 0.0 }
    }

    fn rect_at(&self, w: f32, h: f32, d: f32) -> Rect {
        let base = layout::place(self.position, w, h);
        match self.out {
            Some((dx, dy)) => base.shift(dx * d, dy * d),
            None => base,
        }
    }

    fn slide(&self, d: f32) -> Vector3 {
        let (dx, dy) = self.out.unwrap_or((0.0, 0.0));
        Vector3::new(dx * d, dy * d, 0.0)
    }

    /// 지금 섬의 자리와 향하는 자리(pt).
    pub fn bounds(&self, now: Instant) -> (Rect, Rect) {
        let v = |m: &Motion| m.at(now).0 as f32;
        let t = |m: &Motion| m.target() as f32;
        (
            self.rect_at(v(&self.w), v(&self.h), v(&self.d)),
            self.rect_at(t(&self.w), t(&self.h), t(&self.d)),
        )
    }

    /// 크기, 모서리, 바깥 거리에 맞는 윤곽: 가장자리 좌표계에서, 그다음 화면 위에서. 그리고 테두리
    /// 선용 몸통만.
    fn paths(&self, gfx: &Gfx, w: f32, h: f32, r: f32, d: f32) -> Result<(CompositionPath, CompositionPath)> {
        let rect = self.rect_at(w, h, d);
        let (cw, ch) = (CANVAS_W, CANVAS_H);
        let outline = match self.out {
            None => squircle::drop(&Drop {
                center: rect.x + rect.w / 2.0,
                top: rect.y,
                w: rect.w,
                h: rect.h,
                r,
                near: r,
                ..Drop::default()
            }),
            Some((dx, dy)) => {
                let drop = |center: f32, top: f32, along: f32, out: f32| liquid(center, top, along, out, r);
                if dy < 0.0 {
                    squircle::drop(&drop(rect.x + rect.w / 2.0, rect.y, rect.w, rect.h))
                } else if dy > 0.0 {
                    squircle::map(squircle::drop(&drop(rect.x + rect.w / 2.0, ch - rect.bottom(), rect.w, rect.h)), |p| Pt {
                        x: p.x,
                        y: ch - p.y,
                    })
                } else if dx < 0.0 {
                    squircle::map(squircle::drop(&drop(rect.y + rect.h / 2.0, rect.x, rect.h, rect.w)), |p| Pt {
                        x: p.y,
                        y: p.x,
                    })
                } else {
                    squircle::map(squircle::drop(&drop(rect.y + rect.h / 2.0, cw - rect.right(), rect.h, rect.w)), |p| Pt {
                        x: cw - p.y,
                        y: p.x,
                    })
                }
            }
        };
        Ok((gfx.path(outline.0, &outline.1)?, gfx.squircle(rect, r)?))
    }

    /// spring으로 윤곽을 크기, 모서리, 바깥 거리로 보냅니다. 키 프레임은 합성기가 재생합니다. 멈출
    /// 때까지의 초를 돌려줍니다.
    #[allow(clippy::too_many_arguments)]
    fn shape(&mut self, gfx: &Gfx, w: f32, h: f32, r: f32, d: f32, spring: Spring, now: Instant) -> Result<f64> {
        for (m, to) in [(&mut self.w, w), (&mut self.h, h), (&mut self.r, r), (&mut self.d, d)] {
            m.go(to as f64, spring, 0.1, now);
        }
        let secs = [&self.w, &self.h, &self.r, &self.d].iter().map(|m| m.left(now)).fold(0.0, f64::max);
        let (path, offset) = (h!("Path"), h!("Offset"));
        if secs < 1.0 / self.rate {
            let (drop, body) = self.paths(gfx, w, h, r, d)?;
            for g in [&self.outline, &self.clip] {
                g.StopAnimation(path)?;
                g.SetPath(&drop)?;
            }
            self.rim_path.StopAnimation(path)?;
            self.rim_path.SetPath(&body)?;
            self.mover.StopAnimation(offset)?;
            self.mover.SetOffset(self.slide(d))?;
            return Ok(0.0);
        }
        // 스프링은 2초 안에 멈춥니다.
        let n = ((secs * self.rate).ceil() as usize).clamp(2, (2.0 * self.rate) as usize);
        let drops = self.comp.CreatePathKeyFrameAnimation()?;
        let bodies = self.comp.CreatePathKeyFrameAnimation()?;
        let offsets = self.comp.CreateVector3KeyFrameAnimation()?;
        for i in 0..=n {
            let p = i as f64 / n as f64;
            let at = now + Duration::from_secs_f64(secs * p);
            let v = |m: &Motion| m.at(at).0 as f32;
            let (w, h, r, d) = (v(&self.w), v(&self.h), v(&self.r), v(&self.d));
            let (drop, body) = self.paths(gfx, w, h, r, d)?;
            drops.InsertKeyFrameWithEasingFunction(p as f32, &drop, &self.linear)?;
            bodies.InsertKeyFrameWithEasingFunction(p as f32, &body, &self.linear)?;
            offsets.InsertKeyFrameWithEasingFunction(p as f32, self.slide(d), &self.linear)?;
        }
        for a in [&drops, &bodies] {
            a.SetDuration(span(secs))?;
        }
        offsets.SetDuration(span(secs))?;
        self.outline.StartAnimation(path, &drops)?;
        self.clip.StartAnimation(path, &drops)?;
        self.rim_path.StartAnimation(path, &bodies)?;
        self.mover.StartAnimation(offset, &offsets)?;
        Ok(secs)
    }

    /// 섬이 나옵니다: 아무것도 없는 데서 (fresh: 창이 숨어 있었음) 가장자리를 밀고 나오는 방울로,
    /// 또는 그 자리의 한 점에서. 사라지던 중이면 돌아서서 돌아옵니다.
    pub fn appear(&mut self, gfx: &Gfx, fresh: bool, now: Instant) -> Result<()> {
        if self.shown {
            return Ok(());
        }
        let edge = self.out.is_some();
        if fresh {
            let (w0, h0, r0) = if edge {
                (self.drop_size().0, self.drop_size().1, DROP_R)
            } else {
                (POINT, POINT, POINT / 2.0)
            };
            let away = self.out_of_sight();
            self.w.set(w0 as f64);
            self.h.set(h0 as f64);
            self.r.set(r0 as f64);
            self.d.set(away as f64);
            self.shape(gfx, w0, h0, r0, away, Spring::SMOOTH, now)?;
            self.island.StopAnimation(h!("A"))?;
            self.island.InsertScalar(h!("A"), 0.0)?;
            self.rim.StopAnimation(h!("Opacity"))?;
            self.rim.SetOpacity(0.0)?;
        }
        self.emerging = true;
        self.ease(&self.island, "A", 1.0, if edge { 0.04 } else { 0.08 }, 0.0, &self.ease_out)?;
        self.ease(&self.rim, "Opacity", 1.0, 0.2, if edge { 0.3 } else { 0.1 }, &self.ease_out)?;
        self.shown = true;
        Ok(())
    }

    /// 쉬는 자리에서 섬을 크기와 모서리로 바꿉니다. 끝날 때까지의 초.
    pub fn morph(&mut self, gfx: &Gfx, w: f32, h: f32, r: f32, now: Instant) -> Result<f64> {
        let grow = (w * h) as f64 >= self.w.target() * self.h.target();
        self.shape(gfx, w, h, r, 0.0, if grow { Spring::OPEN } else { SHRINK }, now)
    }

    /// 사라지기: (가장자리에 붙어 있든 아니든) 쉬는 자리의 한 점까지 줄어듭니다. 사라질 때까지의
    /// 초.
    pub fn vanish(&mut self, gfx: &Gfx, now: Instant) -> Result<f64> {
        self.leave_all(now)?;
        self.emerging = false;
        self.ease(&self.rim, "Opacity", 0.0, 0.08, 0.0, &self.ease_in)?;
        let secs = self.shape(gfx, POINT, POINT, POINT / 2.0, 0.0, AWAY, now)?;
        // 스프링 꼬리의 마지막 0.2초는 몇 pt짜리 점으로 남아 눈에 걸리므로, 점에 다다르는 그 꼬리
        // 동안만 흐리게 지웁니다.
        self.ease(&self.island, "A", 0.0, 0.2, (secs - 0.2).max(0.0), &self.ease_in)?;
        self.shown = false;
        Ok(secs.max(0.3))
    }

    /// target.property가 지금 값에서 value로 부드럽게 갑니다.
    fn ease<T: Interface>(&self, target: &T, property: &str, value: f32, secs: f64, delay: f64, curve: &CompositionEasingFunction) -> Result<()> {
        let a = self.comp.CreateScalarKeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, value, curve)?;
        a.SetDuration(span(secs))?;
        if delay > 0.0 {
            a.SetDelayTime(span(delay))?;
        }
        target.cast::<CompositionObject>()?.StartAnimation(&HSTRING::from(property), &a)
    }

    fn scale_to(&self, v: &ContainerVisual, to: f32, secs: f64) -> Result<()> {
        let a = self.comp.CreateVector3KeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, Vector3::new(to, to, 1.0), &self.ease_in)?;
        a.SetDuration(span(secs))?;
        v.StartAnimation(h!("Scale"), &a)
    }

    fn leave_all(&mut self, now: Instant) -> Result<()> {
        for i in 0..self.faces.len() {
            if self.faces[i].leaving.is_none() {
                let root = self.faces[i].root.clone();
                self.ease(&root, "Opacity", 0.0, 0.12, 0.0, &self.ease_in)?;
                self.scale_to(&root, 0.94, 0.15)?;
                self.faces[i].leaving = Some(now + Duration::from_millis(220));
            }
        }
        Ok(())
    }

    /// 떠나기를 마친 face들을 치웁니다. 다음 것이 끝날 때를 돌려줍니다.
    pub fn prune(&mut self, now: Instant) -> Option<Instant> {
        let children = self.mover.Children().ok();
        self.faces.retain(|f| match f.leaving {
            Some(t) if t <= now => {
                if let Some(c) = &children {
                    let _ = c.Remove(&f.root);
                }
                false
            }
            _ => true,
        });
        self.faces.iter().filter_map(|f| f.leaving).min()
    }

    /// 떠 있는 face (떠나는 중이 아닌 것).
    pub fn top(&self) -> Option<&FaceVisual> {
        self.faces.last().filter(|f| f.leaving.is_none())
    }

    /// 섬의 모양이 바뀌는 동안 새 face가 그 위로 들어오고 (나오는 중이면 방울이 다 나온 뒤에), 떠
    /// 있던 것은 떠납니다.
    pub fn enter(&mut self, gfx: &Gfx, face: Face, now: Instant) -> Result<()> {
        let fv = self.make(gfx, face)?;
        self.leave_all(now)?;
        let delay = if std::mem::take(&mut self.emerging) { 0.24 } else { 0.07 };
        fv.root.SetOpacity(0.0)?;
        fv.root.SetScale(Vector3::new(0.9, 0.9, 1.0))?;
        self.mover.Children()?.InsertAtTop(&fv.root)?;
        self.ease(&fv.root, "Opacity", 1.0, 0.24, delay, &self.ease_out)?;
        let (frames, secs) = Spring::OPEN.unit_frames();
        let a = self.comp.CreateVector3KeyFrameAnimation()?;
        for (p, v) in frames {
            let s = 0.9 + 0.1 * v;
            a.InsertKeyFrameWithEasingFunction(p, Vector3::new(s, s, 1.0), &self.linear)?;
        }
        a.SetDuration(span(secs))?;
        a.SetDelayTime(span(delay - 0.05))?;
        fv.root.StartAnimation(h!("Scale"), &a)?;
        self.faces.push(fv);
        Ok(())
    }

    /// 떠 있는 face가 같은 모양의 새 내용을 받습니다: 그 자리에서 다시 그리고, 막대와 링은 새
    /// 진행으로 미끄러집니다.
    pub fn update(&mut self, gfx: &Gfx, face: Face, now: Instant) -> Result<()> {
        let Some(fv) = self.faces.last_mut().filter(|f| f.leaving.is_none()) else {
            return Ok(());
        };
        gfx.draw(&fv.surface, &face, self.scale)?;
        if (face.fraction - fv.face.fraction).abs() > 1e-4 {
            // 새 숫자마다 막대가 있는 자리에서 지금 속도로 이어받습니다: 멈췄다 다시 가지 않고 그
            // 사이를 매끄럽게 움직입니다.
            match fv.progress.go(f64::from(face.fraction), FOLLOW, 0.0005, now) {
                Go::Frames(frames, secs) => {
                    let a = self.comp.CreateScalarKeyFrameAnimation()?;
                    for (p, v) in frames {
                        a.InsertKeyFrameWithEasingFunction(p, v, &self.linear)?;
                    }
                    a.SetDuration(span(secs))?;
                    fv.props.StartAnimation(h!("P"), &a)?;
                }
                Go::Jump(v) => {
                    fv.props.StopAnimation(h!("P"))?;
                    fv.props.InsertScalar(h!("P"), v)?;
                }
            }
        }
        let caret = fv.caret.clone().map(|c| (c, face.caret));
        fv.face = face;
        match caret {
            Some((c, at)) => self.caret_to(&c, at),
            None => Ok(()),
        }
    }

    /// 캐럿을 한 자리에 두고 켜진 상태부터 깜빡이게 하거나 (입력하는 동안처럼), 숨깁니다.
    fn caret_to(&self, caret: &SpriteVisual, at: Option<Rect>) -> Result<()> {
        let Some(r) = at else { return caret.SetIsVisible(false) };
        caret.SetOffset(Vector3::new(r.x, r.y, 0.0))?;
        caret.SetSize(Vector2::new(r.w, r.h))?;
        caret.SetIsVisible(true)?;
        // 반은 켜고 반은 끕니다 (macOS의 1.06초).
        let a = self.comp.CreateScalarKeyFrameAnimation()?;
        for (p, o) in [(0.0, 1.0), (0.5, 1.0), (0.501, 0.0), (0.999, 0.0), (1.0, 1.0)] {
            a.InsertKeyFrameWithEasingFunction(p, o, &self.linear)?;
        }
        a.SetDuration(span(1.06))?;
        a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        caret.StartAnimation(h!("Opacity"), &a)
    }

    /// 떠 있는 face를 그 자리에서 다시 그립니다 (미리보기가 왔습니다).
    pub fn repaint(&self, gfx: &Gfx) -> Result<()> {
        for fv in self.faces.iter().filter(|f| f.leaving.is_none()) {
            gfx.draw(&fv.surface, &fv.face, self.scale)?;
        }
        Ok(())
    }

    /// 모든 face를 다시 그립니다 (새 GPU 장치, 새 DPI).
    pub fn redraw(&mut self, gfx: &Gfx) -> Result<()> {
        for fv in &mut self.faces {
            if fv.leaving.is_some() {
                continue;
            }
            let surface = gfx.surface(fv.face.w, fv.face.h, self.scale)?;
            gfx.draw(&surface, &fv.face, self.scale)?;
            let brush = self.comp.CreateSurfaceBrushWithSurface(&surface)?;
            brush.SetStretch(CompositionStretch::Fill)?;
            fv.art.SetBrush(&brush)?;
            fv.art.SetSize(Vector2::new(
                (fv.face.w * self.scale).ceil() / self.scale,
                (fv.face.h * self.scale).ceil() / self.scale,
            ))?;
            fv.surface = surface;
        }
        Ok(())
    }

    /// 마우스 아래의 버튼을 밝히고, 누르면 더 밝힙니다.
    pub fn hover(&self, button: Option<usize>, pressed: bool) -> Result<()> {
        let Some(fv) = self.top() else { return Ok(()) };
        for (i, v) in fv.hovers.iter().enumerate() {
            let to = match (Some(i) == button, pressed) {
                (true, true) => 0.22,
                (true, false) => 0.1,
                _ => 0.0,
            };
            self.ease(v, "Opacity", to, 0.12, 0.0, &self.ease_out)?;
        }
        Ok(())
    }

    /// 키보드로 고른 버튼에 포커스 링을 두르고, 나머지 링은 걷는다.
    pub fn focus(&self, button: Option<usize>) -> Result<()> {
        let Some(fv) = self.top() else { return Ok(()) };
        for (i, v) in fv.rings.iter().enumerate() {
            let to = if Some(i) == button { 1.0 } else { 0.0 };
            self.ease(v, "Opacity", to, 0.1, 0.0, &self.ease_out)?;
        }
        Ok(())
    }

    fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    fn make(&self, gfx: &Gfx, face: Face) -> Result<FaceVisual> {
        let comp = &self.comp;
        let root = comp.CreateContainerVisual()?;
        let at = layout::place(self.position, face.w, face.h);
        let origin = Vector2::new(self.snap(at.x), self.snap(at.y));
        let (fx, fy) = self.position.anchor();
        root.SetOffset(Vector3::new(origin.X, origin.Y, 0.0))?;
        root.SetSize(Vector2::new(face.w, face.h))?;
        // 섬이 붙어 있는 쪽에서 자라 나옵니다.
        root.SetCenterPoint(Vector3::new(face.w * fx, face.h * fy, 0.0))?;

        let surface = gfx.surface(face.w, face.h, self.scale)?;
        gfx.draw(&surface, &face, self.scale)?;
        let brush = comp.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        let art = comp.CreateSpriteVisual()?;
        art.SetSize(Vector2::new(
            (face.w * self.scale).ceil() / self.scale,
            (face.h * self.scale).ceil() / self.scale,
        ))?;
        art.SetBrush(&brush)?;

        let props = comp.CreatePropertySet()?;
        props.InsertScalar(h!("P"), face.fraction)?;
        let tint = color(tint_hex(face.tint), 1.0);
        let live = comp.CreateShapeVisual()?;
        live.SetSize(Vector2::new(face.w, face.h))?;
        let shapes = live.Shapes()?;
        if let Some(bar) = face.bar {
            let r = bar.h / 2.0;
            let track = comp.CreateRoundedRectangleGeometry()?;
            track.SetOffset(Vector2::new(bar.x, bar.y))?;
            track.SetSize(Vector2::new(bar.w, bar.h))?;
            track.SetCornerRadius(Vector2::new(r, r))?;
            let s = comp.CreateSpriteShapeWithGeometry(&track)?;
            s.SetFillBrush(&comp.CreateColorBrushWithColor(color(0xFFFFFF, 0.2))?)?;
            shapes.Append(&s)?;
            let fill = comp.CreateRoundedRectangleGeometry()?;
            fill.SetOffset(Vector2::new(bar.x, bar.y))?;
            fill.SetCornerRadius(Vector2::new(r, r))?;
            let e = comp.CreateExpressionAnimationWithExpression(h!("Vector2(Max(f.P * bw, bh), bh)"))?;
            e.SetReferenceParameter(h!("f"), &props)?;
            e.SetScalarParameter(h!("bw"), bar.w)?;
            e.SetScalarParameter(h!("bh"), bar.h)?;
            fill.StartAnimation(h!("Size"), &e)?;
            let s = comp.CreateSpriteShapeWithGeometry(&fill)?;
            s.SetFillBrush(&comp.CreateColorBrushWithColor(tint)?)?;
            shapes.Append(&s)?;
        }
        if let Some(ring) = face.ring {
            let center = Vector2::new(ring.x + ring.w / 2.0, ring.y + ring.h / 2.0);
            // 열렸을 때 전송의 멈춤 버튼이 들어 있는 링: 두껍고, 강조색의 트랙 위에 (iPhone의
            // AirDrop처럼). 작을 때는 밝은 트랙 위에 가늘게.
            let big = ring.w > 30.0;
            let stroke = if big { 4.0 } else { 3.0 };
            let radius = ring.w / 2.0 - stroke / 2.0 - if big { 0.5 } else { 1.0 };
            let track = if big { color(tint_hex(face.tint), 0.28) } else { color(0xFFFFFF, 0.22) };
            for live_part in [false, true] {
                let g = comp.CreateEllipseGeometry()?;
                g.SetCenter(center)?;
                g.SetRadius(Vector2::new(radius, radius))?;
                let s = comp.CreateSpriteShapeWithGeometry(&g)?;
                let c = if live_part { tint } else { track };
                s.SetStrokeBrush(&comp.CreateColorBrushWithColor(c)?)?;
                s.SetStrokeThickness(stroke)?;
                if live_part {
                    s.SetStrokeStartCap(CompositionStrokeCap::Round)?;
                    s.SetStrokeEndCap(CompositionStrokeCap::Round)?;
                    s.SetCenterPoint(center)?;
                    s.SetRotationAngleInDegrees(-90.0)?;
                    let e = comp.CreateExpressionAnimationWithExpression(h!("Max(f.P, 0.001)"))?;
                    e.SetReferenceParameter(h!("f"), &props)?;
                    g.StartAnimation(h!("TrimEnd"), &e)?;
                }
                shapes.Append(&s)?;
            }
        }
        if let Some(sp) = face.spinner {
            let center = Vector2::new(sp.x + sp.w / 2.0, sp.y + sp.h / 2.0);
            // 열렸을 때 (아직 보일 것이 없어) 도는 전송의 링, 그 트랙 위에.
            let big = sp.w > 30.0;
            let stroke = if big { 4.0 } else { 2.5 };
            let radius = sp.w / 2.0 - if big { stroke / 2.0 + 0.5 } else { 2.0 };
            if big {
                let g = comp.CreateEllipseGeometry()?;
                g.SetCenter(center)?;
                g.SetRadius(Vector2::new(radius, radius))?;
                let s = comp.CreateSpriteShapeWithGeometry(&g)?;
                s.SetStrokeBrush(&comp.CreateColorBrushWithColor(color(tint_hex(face.tint), 0.28))?)?;
                s.SetStrokeThickness(stroke)?;
                shapes.Append(&s)?;
            }
            let g = comp.CreateEllipseGeometry()?;
            g.SetCenter(center)?;
            g.SetRadius(Vector2::new(radius, radius))?;
            g.SetTrimEnd(0.3)?;
            let s = comp.CreateSpriteShapeWithGeometry(&g)?;
            s.SetStrokeBrush(&comp.CreateColorBrushWithColor(if big { tint } else { color(0xFFFFFF, 0.85) })?)?;
            s.SetStrokeThickness(stroke)?;
            s.SetStrokeStartCap(CompositionStrokeCap::Round)?;
            s.SetStrokeEndCap(CompositionStrokeCap::Round)?;
            s.SetCenterPoint(center)?;
            let a = comp.CreateScalarKeyFrameAnimation()?;
            a.InsertKeyFrameWithEasingFunction(0.0, 0.0, &self.linear)?;
            a.InsertKeyFrameWithEasingFunction(1.0, 360.0, &self.linear)?;
            a.SetDuration(span(0.9))?;
            a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            s.StartAnimation(h!("RotationAngleInDegrees"), &a)?;
            shapes.Append(&s)?;
        }
        let mut hovers = Vec::new();
        let mut rings = Vec::new();
        for (rect, _) in &face.buttons {
            // 아래에 그린 버튼과 같은 연속 곡률 캡슐.
            let g = comp.CreatePathGeometryWithPath(&gfx.squircle(*rect, rect.h / 2.0)?)?;
            let s = comp.CreateSpriteShapeWithGeometry(&g)?;
            s.SetFillBrush(&comp.CreateColorBrushWithColor(color(0xFFFFFF, 1.0))?)?;
            let v = comp.CreateShapeVisual()?;
            v.SetSize(Vector2::new(face.w, face.h))?;
            v.Shapes()?.Append(&s)?;
            v.SetOpacity(0.0)?;
            hovers.push(v);
            // 키보드 포커스 링: 버튼에서 2.5pt 떨어진 같은 모양의 파란 테 (macOS의 초점 링).
            let around = rect.inflate(FOCUS_GAP + FOCUS_STROKE / 2.0);
            let g = comp.CreatePathGeometryWithPath(&gfx.squircle(around, around.h / 2.0)?)?;
            let s = comp.CreateSpriteShapeWithGeometry(&g)?;
            s.SetStrokeBrush(&comp.CreateColorBrushWithColor(color(tint_hex(crate::Tint::Blue), 1.0))?)?;
            s.SetStrokeThickness(FOCUS_STROKE)?;
            let v = comp.CreateShapeVisual()?;
            v.SetSize(Vector2::new(face.w, face.h))?;
            v.Shapes()?.Append(&s)?;
            v.SetOpacity(0.0)?;
            rings.push(v);
        }
        let children = root.Children()?;
        children.InsertAtTop(&art)?;
        children.InsertAtTop(&live)?;
        for v in hovers.iter().chain(&rings) {
            children.InsertAtTop(v)?;
        }
        let caret = match face.field {
            Some(_) => {
                let c = comp.CreateSpriteVisual()?;
                c.SetBrush(&comp.CreateColorBrushWithColor(color(tint_hex(crate::Tint::Blue), 1.0))?)?;
                children.InsertAtTop(&c)?;
                self.caret_to(&c, face.caret)?;
                Some(c)
            }
            None => None,
        };
        let progress = Motion::rest(f64::from(face.fraction));
        Ok(FaceVisual {
            root,
            art,
            surface,
            props,
            hovers,
            rings,
            face,
            origin,
            leaving: None,
            progress,
            caret,
        })
    }

    /// 모든 face를 한꺼번에 치웁니다 (섬을 숨겼습니다).
    pub fn clear(&mut self) {
        if let Ok(c) = self.mover.Children() {
            let _ = c.RemoveAll();
        }
        self.faces.clear();
    }
}
