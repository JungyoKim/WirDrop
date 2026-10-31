//! Direct2D와 DirectWrite: 카드가 바뀔 때 합성 표면에 그리는 섬의 글, 아이콘, 그림. 글꼴은
//! 메모리에서 씁니다: Pretendard (한글 서브셋, WOFF2)와 섬이 쓰는 Framework7 Icons.
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::{IGeometrySource2D, IGeometrySource2D_Impl};
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, CompositionPath, Compositor};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, E_FAIL, E_NOTIMPL, HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Device, ID2D1DeviceContext, ID2D1Factory, ID2D1Factory1, ID2D1Geometry, ID2D1PathGeometry,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_CONTAINER_TYPE_WOFF2, DWRITE_FACTORY_TYPE_ISOLATED, DWRITE_FONT_FACE_TYPE_TRUETYPE, DWRITE_FONT_FEATURE,
    DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_METRICS, DWRITE_FONT_SIMULATIONS_NONE, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_GLYPH_METRICS, DWRITE_GLYPH_RUN,
    DWRITE_HIT_TEST_METRICS, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection1, IDWriteFontFace,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, IDXGIDevice};
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapIgnoreAlpha,
    WICBitmapPaletteTypeMedianCut,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositionGraphicsDeviceInterop, ICompositorInterop};
use windows::Win32::System::WinRT::Graphics::Direct2D::{IGeometrySource2DInterop, IGeometrySource2DInterop_Impl};
use windows::core::{Error, Interface, Ref, Result, implement, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::layout::{Face, Ink, Item, Measure, Rect, Style};
use crate::squircle::{self, Pt, Seg};
use crate::{Glyph, Tint};

const PRETENDARD: [&[u8]; 3] = [
    include_bytes!("../assets/Pretendard-Regular.subset.woff2"),
    include_bytes!("../assets/Pretendard-Medium.subset.woff2"),
    include_bytes!("../assets/Pretendard-SemiBold.subset.woff2"),
];
const ICONS: &[u8] = include_bytes!("../assets/F7Icons.ttf");

pub fn rgba(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

/// Apple 시스템 색, 어두운 모드.
pub fn tint_hex(t: Tint) -> u32 {
    match t {
        Tint::Blue => 0x0A84FF,
        Tint::Green => 0x30D158,
        Tint::Red => 0xFF453A,
        Tint::Orange => 0xFF9F0A,
        Tint::Gray => 0x8E8E93,
    }
}

fn ink(i: Ink) -> D2D1_COLOR_F {
    match i {
        Ink::White => rgba(0xFFFFFF, 1.0),
        // Apple의 보조·삼차 레이블 색(어두운 모드): EBEBF5의 60 %와 30 % (데스크톱의 더 밝고
        // 어지러운 배경에 맞춰 조금 더 진하게).
        Ink::Secondary => rgba(0xEBEBF5, 0.62),
        Ink::Tertiary => rgba(0xEBEBF5, 0.4),
        Ink::Tint(t) => rgba(tint_hex(t), 1.0),
    }
}

/// GPU와 함께 잃었습니다 (드라이버 업데이트, 절전): 장치를 다시 만들고 다시 그립니다.
pub fn device_lost(e: &Error) -> bool {
    let c = e.code();
    c == DXGI_ERROR_DEVICE_REMOVED || c == DXGI_ERROR_DEVICE_RESET || c == D2DERR_RECREATE_TARGET
}

fn d3d(kind: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            kind,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| Error::from(E_FAIL))
}

fn d2d_device(factory: &ID2D1Factory1) -> Result<ID2D1Device> {
    let device = d3d(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| d3d(D3D_DRIVER_TYPE_WARP))?;
    let dxgi: IDXGIDevice = device.cast()?;
    unsafe { factory.CreateDevice(&dxgi) }
}

pub struct Gfx {
    factory: ID2D1Factory1,
    pub graphics: CompositionGraphicsDevice,
    dwrite: IDWriteFactory5,
    formats: HashMap<Style, IDWriteTextFormat>,
    tnum: IDWriteTypography,
    icons: IDWriteFontFace,
    wic: IWICImagingFactory,
    pictures: RefCell<HashMap<PathBuf, Picture>>,
    /// (이 스레드 밖에서 일꾼이) 만들 미리보기: 파일과 크기.
    asks: RefCell<Vec<(PathBuf, u32)>>,
    lines: RefCell<HashMap<Style, f32>>,
}

/// 파일의 미리보기: 있는 것 (그 크기; 파일에 없으면 none)과 요청받은 가장 큰 크기.
#[derive(Default)]
struct Picture {
    ready: Option<(u32, Option<IWICBitmapSource>)>,
    asked: u32,
}

impl Gfx {
    pub fn new(compositor: &Compositor) -> Result<Gfx> {
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let device = d2d_device(&factory)?;
        let interop: ICompositorInterop = compositor.cast()?;
        let graphics = unsafe { interop.CreateGraphicsDevice(&device)? };
        let dwrite: IDWriteFactory5 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)? };
        let (fonts, icons) = unsafe { load_fonts(&dwrite)? };
        let mut formats = HashMap::new();
        for (style, weight, size) in [
            (Style::Title, DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0),
            (Style::Body, DWRITE_FONT_WEIGHT_MEDIUM, 13.0),
            (Style::Small, DWRITE_FONT_WEIGHT_MEDIUM, 12.0),
            (Style::Number, DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0),
            (Style::Button, DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0),
            (Style::Brief, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0),
            (Style::Field, DWRITE_FONT_WEIGHT_NORMAL, 15.0),
        ] {
            formats.insert(style, unsafe { format(&dwrite, &fonts, weight, size)? });
        }
        let tnum = unsafe {
            let t = dwrite.CreateTypography()?;
            t.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            t
        };
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Gfx {
            factory,
            graphics,
            dwrite,
            formats,
            tnum,
            icons,
            wic,
            pictures: RefCell::default(),
            asks: RefCell::default(),
            lines: RefCell::default(),
        })
    }

    /// 같은 합성 그래픽 장치 뒤에 새 GPU 장치를 둡니다. 그 뒤 표면을 다시 그려야 합니다.
    pub fn renew(&self) -> Result<()> {
        let device = d2d_device(&self.factory)?;
        let interop: ICompositionGraphicsDeviceInterop = self.graphics.cast()?;
        unsafe { interop.SetRenderingDevice(&device) }
    }

    /// 사각형과 모서리 반지름에 맞는 섬의 윤곽 (Apple의 연속 곡률 모서리), 합성기용.
    pub fn squircle(&self, rect: Rect, r: f32) -> Result<CompositionPath> {
        let (start, segs) = squircle::outline(rect.x, rect.y, rect.w, rect.h, r);
        self.path(start, &segs)
    }

    /// 합성기용 윤곽.
    pub fn path(&self, start: Pt, segs: &[Seg]) -> Result<CompositionPath> {
        let source: IGeometrySource2D = Outline(self.geometry(start, segs)?.cast()?).into();
        CompositionPath::Create(&source)
    }

    /// Direct2D 도형으로 만든 윤곽.
    fn geometry(&self, start: Pt, segs: &[Seg]) -> Result<ID2D1PathGeometry> {
        let v = |p: Pt| Vector2 { X: p.x, Y: p.y };
        let geometry = unsafe { self.factory.CreatePathGeometry()? };
        unsafe {
            let sink = geometry.Open()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            sink.BeginFigure(v(start), D2D1_FIGURE_BEGIN_FILLED);
            for s in segs {
                match *s {
                    Seg::Line(p) => sink.AddLine(v(p)),
                    Seg::Cubic(a, b, c) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: v(a),
                        point2: v(b),
                        point3: v(c),
                    }),
                }
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
        }
        geometry.cast()
    }

    /// Direct2D로 그리는 연속 곡률 모서리 사각형 (r이 높이의 절반이면 캡슐).
    fn rounded(&self, rect: Rect, r: f32) -> Result<ID2D1PathGeometry> {
        let (start, segs) = squircle::outline(rect.x, rect.y, rect.w, rect.h, r);
        self.geometry(start, &segs)
    }

    pub fn surface(&self, w: f32, h: f32, scale: f32) -> Result<CompositionDrawingSurface> {
        self.graphics.CreateDrawingSurface(
            Size {
                Width: (w * scale).ceil().max(1.0),
                Height: (h * scale).ceil().max(1.0),
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    fn text_layout(&self, text: &str, style: Style, w: f32, h: f32) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe { self.dwrite.CreateTextLayout(&wide, &self.formats[&style], w.max(1.0), h.max(1.0))? };
        // 고정폭 숫자: 퍼센트가 바뀌어도 자리가 흔들리지 않습니다.
        if matches!(style, Style::Small | Style::Number | Style::Brief) {
            unsafe {
                layout.SetTypography(
                    &self.tnum,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: wide.len() as u32,
                    },
                )?
            };
        }
        Ok(layout)
    }

    fn metrics(&self, layout: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
        let mut m = DWRITE_TEXT_METRICS::default();
        let _ = unsafe { layout.GetMetrics(&mut m) };
        m
    }

    /// 글이 시작하는 곳을 아래쪽 정수 픽셀에 둡니다 (그리기 변환이 pt를 픽셀로 바꿉니다):
    /// Direct2D가 글을 픽셀 격자에 맞출 때 고를 것이 없게.
    fn on_pixel(dc: &ID2D1DeviceContext, x: f32, y: f32) -> Vector2 {
        let mut m = Matrix3x2::default();
        unsafe { dc.GetTransform(&mut m) };
        let k = if m.M22 > 0.0 { m.M22 } else { 1.0 };
        Vector2 {
            X: x,
            Y: ((y * k + m.M32).round() - m.M32) / k,
        }
    }

    /// face를 그 표면 전체에 그립니다.
    pub fn draw(&self, surface: &CompositionDrawingSurface, face: &Face, scale: f32) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let painted = self.paint(&dc, face, scale, offset);
        let ended = unsafe { interop.EndDraw() };
        painted.and(ended)
    }

    fn paint(&self, dc: &ID2D1DeviceContext, face: &Face, scale: f32, offset: POINT) -> Result<()> {
        unsafe {
            // pt를 픽셀로, 그다음 아틀라스 안 표면의 자리를 정수 픽셀로. (자리를 pt로 두고 화면
            // DPI로 그렸더니 아틀라스가 번갈아 바뀌면서 자리가 한 번은 1/3pt, 다음엔 2/3pt
            // 어긋났고, Direct2D가 pt 격자에 맞추는 글은 갱신마다 반 픽셀씩 흔들렸습니다.)
            dc.SetDpi(96.0, 96.0);
            dc.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: offset.x as f32,
                M32: offset.y as f32,
            });
            dc.Clear(Some(&D2D1_COLOR_F::default()));
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        for item in &face.items {
            match item {
                Item::Text {
                    rect,
                    text,
                    style,
                    ink: i,
                    right,
                    lines,
                } => self.text(dc, *rect, text, *style, ink(*i), *right, *lines)?,
                Item::Icon { rect, glyph, ink: i } => self.icon(dc, *glyph, *rect, ink(*i))?,
                Item::Disc { rect, alpha } => {
                    let color = if *alpha < 0.0 { rgba(0, 1.0) } else { rgba(0xFFFFFF, *alpha) };
                    self.disc(dc, *rect, color)?
                }
                Item::Picture {
                    rect,
                    radius,
                    path,
                    glyph,
                    tint,
                } => {
                    if !self.picture(dc, path, *rect, *radius, scale)? {
                        if glyph.round() {
                            self.icon(dc, *glyph, *rect, ink(Ink::Tint(*tint)))?;
                        } else {
                            self.disc(dc, *rect, rgba(0xFFFFFF, 0.16))?;
                            let g = rect.w * 0.55;
                            self.icon(
                                dc,
                                *glyph,
                                Rect::new(rect.x + (rect.w - g) / 2.0, rect.y + (rect.h - g) / 2.0, g, g),
                                ink(Ink::White),
                            )?;
                        }
                    }
                }
                Item::Capsule { rect, primary, tint, label } => {
                    let fill = if *primary { rgba(tint_hex(*tint), 1.0) } else { rgba(0xFFFFFF, 0.16) };
                    unsafe {
                        let brush = dc.CreateSolidColorBrush(&fill, None)?;
                        dc.FillGeometry(&self.rounded(*rect, rect.h / 2.0)?, &brush, None);
                        let layout = self.text_layout(label, Style::Button, rect.w - 16.0, rect.h)?;
                        layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                        layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        let white = dc.CreateSolidColorBrush(&ink(Ink::White), None)?;
                        dc.DrawTextLayout(
                            Self::on_pixel(dc, rect.x + 8.0, rect.y),
                            &layout,
                            &white,
                            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                        );
                    }
                }
                Item::Field {
                    rect,
                    inner,
                    text,
                    placeholder,
                    scroll,
                    selection,
                } => self.field(dc, *rect, *inner, text, placeholder, *scroll, *selection)?,
                Item::Stop { rect, tint } => unsafe {
                    let brush = dc.CreateSolidColorBrush(&rgba(tint_hex(*tint), 1.0), None)?;
                    dc.FillGeometry(&self.rounded(*rect, rect.w * 0.24)?, &brush, None);
                },
            }
        }
        Ok(())
    }

    /// 입력칸: 상자(Apple의 삼차 채움)와 그 안에 입력한 것을 스크롤해 상자에 맞춰 자르고 선택을
    /// 뒤에 깝니다. 비었으면 안내 문구.
    #[allow(clippy::too_many_arguments)]
    fn field(
        &self,
        dc: &ID2D1DeviceContext,
        rect: Rect,
        inner: Rect,
        text: &str,
        placeholder: &str,
        scroll: f32,
        selection: Option<(f32, f32)>,
    ) -> Result<()> {
        unsafe {
            let fill = dc.CreateSolidColorBrush(&rgba(0x767680, 0.24), None)?;
            dc.FillGeometry(&self.rounded(rect, rect.h / 2.0)?, &fill, None);
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: inner.x - 2.0,
                    top: rect.y,
                    right: inner.right() + 2.0,
                    bottom: rect.bottom(),
                },
                D2D1_ANTIALIAS_MODE_ALIASED,
            );
            let drawn = (|| -> Result<()> {
                if text.is_empty() {
                    return self.text(dc, inner, placeholder, Style::Field, ink(Ink::Tertiary), false, 1);
                }
                let layout = self.text_layout(text, Style::Field, 4000.0, inner.h)?;
                layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let x = inner.x - scroll;
                if let Some((from, to)) = selection {
                    let selection = dc.CreateSolidColorBrush(&rgba(tint_hex(Tint::Blue), 0.45), None)?;
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: x + from,
                            top: inner.y,
                            right: x + to,
                            bottom: inner.bottom(),
                        },
                        &selection,
                    );
                }
                let white = dc.CreateSolidColorBrush(&ink(Ink::White), None)?;
                dc.DrawTextLayout(Self::on_pixel(dc, x, inner.y), &layout, &white, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                Ok(())
            })();
            dc.PopAxisAlignedClip();
            drawn
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(&self, dc: &ID2D1DeviceContext, rect: Rect, text: &str, style: Style, color: D2D1_COLOR_F, right: bool, lines: u32) -> Result<()> {
        let layout = self.text_layout(text, style, rect.w, rect.h)?;
        unsafe {
            if lines > 1 {
                layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            }
            if right {
                layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            }
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            dc.DrawTextLayout(
                Self::on_pixel(dc, rect.x, rect.y),
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
        Ok(())
    }

    fn disc(&self, dc: &ID2D1DeviceContext, rect: Rect, color: D2D1_COLOR_F) -> Result<()> {
        unsafe {
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            let ellipse = D2D1_ELLIPSE {
                point: Vector2 {
                    X: rect.x + rect.w / 2.0,
                    Y: rect.y + rect.h / 2.0,
                },
                radiusX: rect.w / 2.0,
                radiusY: rect.h / 2.0,
            };
            dc.FillEllipse(&ellipse, &brush);
        }
        Ok(())
    }

    /// 아이콘 하나. em 상자가 아닌 잉크를 rect 가운데에 들어가는 만큼 크게.
    fn icon(&self, dc: &ID2D1DeviceContext, glyph: Glyph, rect: Rect, color: D2D1_COLOR_F) -> Result<()> {
        unsafe {
            let code = glyph.code() as u32;
            let mut index = 0u16;
            self.icons.GetGlyphIndices(&code, 1, &mut index)?;
            let mut gm = DWRITE_GLYPH_METRICS::default();
            self.icons.GetDesignGlyphMetrics(&index, 1, &mut gm, false)?;
            let mut fm = DWRITE_FONT_METRICS::default();
            self.icons.GetMetrics(&mut fm);
            let upem = fm.designUnitsPerEm as f32;
            // 디자인 단위의 잉크 상자, 기준선에서 위로 y.
            let left = gm.leftSideBearing as f32;
            let right = gm.advanceWidth as f32 - gm.rightSideBearing as f32;
            let top = gm.verticalOriginY as f32 - gm.topSideBearing as f32;
            let bottom = gm.verticalOriginY as f32 - gm.advanceHeight as f32 + gm.bottomSideBearing as f32;
            let (iw, ih) = ((right - left).max(1.0), (top - bottom).max(1.0));
            let k = (rect.w / iw).min(rect.h / ih);
            let x = rect.x + rect.w / 2.0 - (left + iw / 2.0) * k;
            let y = rect.y + rect.h / 2.0 + (top + bottom) / 2.0 * k;
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            let run = DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some(self.icons.clone())),
                fontEmSize: upem * k,
                glyphCount: 1,
                glyphIndices: &index,
                glyphAdvances: std::ptr::null(),
                glyphOffsets: std::ptr::null(),
                isSideways: false.into(),
                bidiLevel: 0,
            };
            dc.DrawGlyphRun(Vector2 { X: x, Y: y }, &run, None, &brush, DWRITE_MEASURING_MODE_NATURAL);
            drop(ManuallyDrop::into_inner(run.fontFace));
        }
        Ok(())
    }

    /// 모서리를 둥글린 파일 미리보기로 rect를 채웁니다. 없으면 false.
    fn picture(&self, dc: &ID2D1DeviceContext, path: &Path, rect: Rect, radius: f32, scale: f32) -> Result<bool> {
        let px = (rect.w.max(rect.h) * scale).ceil() as u32;
        let Some(source) = self.thumbnail(path, px) else { return Ok(false) };
        unsafe {
            let bitmap = dc.CreateBitmapFromWicBitmap(&source, None)?;
            let size = bitmap.GetSize();
            let k = (rect.w / size.width.max(1.0)).max(rect.h / size.height.max(1.0));
            let brush = dc.CreateBitmapBrush(&bitmap, None, None)?;
            brush.SetInterpolationMode1(D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC);
            brush.SetTransform(&Matrix3x2 {
                M11: k,
                M12: 0.0,
                M21: 0.0,
                M22: k,
                M31: rect.x + (rect.w - size.width * k) / 2.0,
                M32: rect.y + (rect.h - size.height * k) / 2.0,
            });
            dc.FillGeometry(&self.rounded(rect, radius)?, &brush, None);
        }
        Ok(true)
    }

    /// 셸이 만드는 파일 미리보기 (사진, 영상, 미리보기 처리기가 있는 모든 것). 파일마다 하나, px의
    /// 두 배 이상. 아직 없으면 (또는 더 작은 것만 있으면) 요청하고, 그동안 있는 것을 씁니다 (없으면
    /// 아이콘이 대신합니다).
    fn thumbnail(&self, path: &Path, px: u32) -> Option<IWICBitmapSource> {
        let side = (px * 2).max(64);
        let mut pictures = self.pictures.borrow_mut();
        if pictures.len() > 32 && !pictures.contains_key(path) {
            pictures.retain(|_, p| p.ready.is_none());
        }
        let p = pictures.entry(path.to_path_buf()).or_default();
        let (have, source) = match &p.ready {
            Some((s, Some(source))) => (*s, Some(source.clone())),
            Some((_, None)) => (u32::MAX, None),
            None => (0, None),
        };
        if have < side && p.asked < side {
            p.asked = side;
            self.asks.borrow_mut().push((path.to_path_buf(), side));
        }
        source
    }

    /// 지난번 뒤로 요청한 미리보기들.
    pub fn asks(&self) -> Vec<(PathBuf, u32)> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// 만든 미리보기 (bitmap: 여기서 지우는 HBITMAP). 이제부터 그것을 그립니다.
    pub fn made(&self, path: PathBuf, side: u32, bitmap: Option<isize>) {
        let source = bitmap.and_then(|raw| unsafe {
            let hbitmap = HBITMAP(raw as _);
            let source = (|| -> Result<IWICBitmapSource> {
                let bitmap = self.wic.CreateBitmapFromHBITMAP(hbitmap, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
                let converter = self.wic.CreateFormatConverter()?;
                converter.Initialize(
                    &bitmap,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeMedianCut,
                )?;
                converter.cast()
            })();
            let _ = DeleteObject(hbitmap.into());
            source.map_err(|e| tracing::debug!("thumbnail for {}: {e}", path.display())).ok()
        });
        let mut pictures = self.pictures.borrow_mut();
        let p = pictures.entry(path).or_default();
        // 더 큰 것이 먼저 왔을 수 있습니다.
        if p.ready.as_ref().is_none_or(|(s, have)| have.is_none() || *s <= side) {
            p.ready = Some((side, source));
        }
    }
}

/// 합성기에 넘기는 Direct2D 도형 (CompositionPath는 IGeometrySource2D를 받습니다).
#[implement(IGeometrySource2D, IGeometrySource2DInterop)]
struct Outline(ID2D1Geometry);

impl IGeometrySource2D_Impl for Outline_Impl {}

impl IGeometrySource2DInterop_Impl for Outline_Impl {
    fn GetGeometry(&self) -> Result<ID2D1Geometry> {
        Ok(self.0.clone())
    }

    fn TryGetGeometryUsingFactory(&self, _: Ref<ID2D1Factory>) -> Result<ID2D1Geometry> {
        Err(E_NOTIMPL.into())
    }
}

unsafe fn format(dwrite: &IDWriteFactory5, fonts: &IDWriteFontCollection1, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dwrite.CreateTextFormat(
            w!("Pretendard"),
            fonts,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("ko-kr"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let sign = dwrite.CreateEllipsisTrimmingSign(&f)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        f.SetTrimming(&trimming, &sign)?;
        Ok(f)
    }
}

/// Pretendard 세 굵기를 한 모음으로, 그리고 아이콘 글꼴의 face.
unsafe fn load_fonts(dwrite: &IDWriteFactory5) -> Result<(IDWriteFontCollection1, IDWriteFontFace)> {
    unsafe {
        let loader = dwrite.CreateInMemoryFontFileLoader()?;
        dwrite.RegisterFontFileLoader(&loader)?;
        let builder = dwrite.CreateFontSetBuilder()?;
        for woff2 in PRETENDARD {
            let stream = dwrite.UnpackFontFile(DWRITE_CONTAINER_TYPE_WOFF2, woff2.as_ptr().cast(), woff2.len() as u32)?;
            let size = stream.GetFileSize()?;
            let mut start = std::ptr::null_mut();
            let mut context = std::ptr::null_mut();
            stream.ReadFileFragment(&mut start, 0, size, &mut context)?;
            let bytes = std::slice::from_raw_parts(start as *const u8, size as usize).to_vec();
            stream.ReleaseFileFragment(context);
            // 주인 없음: 로더가 사본을 둡니다.
            let file = loader.CreateInMemoryFontFileReference(dwrite, bytes.as_ptr().cast(), bytes.len() as u32, None)?;
            builder.AddFontFile(&file)?;
        }
        let set = builder.CreateFontSet()?;
        let fonts = dwrite.CreateFontCollectionFromFontSet(&set)?;
        let file = loader.CreateInMemoryFontFileReference(dwrite, ICONS.as_ptr().cast(), ICONS.len() as u32, None)?;
        let icons = dwrite.CreateFontFace(DWRITE_FONT_FACE_TYPE_TRUETYPE, &[Some(file)], 0, DWRITE_FONT_SIMULATIONS_NONE)?;
        Ok((fonts, icons))
    }
}

impl Measure for Gfx {
    fn width(&self, text: &str, style: Style) -> f32 {
        self.text_layout(text, style, 4000.0, 200.0)
            .map(|l| self.metrics(&l).widthIncludingTrailingWhitespace)
            .unwrap_or(0.0)
    }

    fn line(&self, style: Style) -> f32 {
        if let Some(h) = self.lines.borrow().get(&style) {
            return *h;
        }
        let h = self
            .text_layout("가Ag", style, 4000.0, 200.0)
            .map(|l| self.metrics(&l).height)
            .unwrap_or(18.0);
        self.lines.borrow_mut().insert(style, h);
        h
    }

    fn lines(&self, text: &str, style: Style, width: f32, max: u32) -> u32 {
        let Ok(layout) = self.text_layout(text, style, width, 4000.0) else {
            return 1;
        };
        let _ = unsafe { layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP) };
        self.metrics(&layout).lineCount.clamp(1, max)
    }

    fn index_at(&self, text: &str, style: Style, x: f32) -> usize {
        if text.is_empty() {
            return 0;
        }
        let Ok(layout) = self.text_layout(text, style, 4000.0, 200.0) else {
            return 0;
        };
        let (mut trailing, mut inside) = (Default::default(), Default::default());
        let mut hit = DWRITE_HIT_TEST_METRICS::default();
        let y = self.line(style) / 2.0;
        if unsafe { layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut hit) }.is_err() {
            return 0;
        }
        // UTF-16 단위로: 맞힌 글자의 시작, 오른쪽 절반이면 끝.
        let units = hit.textPosition + if trailing.as_bool() { hit.length } else { 0 };
        let mut seen = 0;
        text.chars()
            .take_while(|c| {
                seen += c.len_utf16() as u32;
                seen <= units
            })
            .count()
    }
}
