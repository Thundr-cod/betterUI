// Direct2D + DirectWrite drawing helpers. The render target runs at 96 DPI so
// one drawing unit = one physical pixel; callers scale sizes themselves.

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_ELLIPSE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

pub const ICON_FONT: &str = "Segoe Fluent Icons";
pub const TEXT_FONT: &str = "Segoe UI Variable Text";

#[derive(Clone, Copy)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    fn d2d(self) -> D2D1_COLOR_F {
        // Premultiplied alpha, as the render target expects.
        D2D1_COLOR_F {
            r: self.0 * self.3,
            g: self.1 * self.3,
            b: self.2 * self.3,
            a: self.3,
        }
    }
}

pub struct Text {
    layout: IDWriteTextLayout,
    pub width: f32,
}

pub struct Gfx {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self { d2d, dwrite, target: None, brush: None })
        }
    }

    pub fn text_format(&self, family: &str, size: f32, weight: i32) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(
                &HSTRING::from(family),
                None,
                DWRITE_FONT_WEIGHT(weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("en-us"),
            )?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        }
    }

    /// Lay out a single line of text, vertically centred in `height`.
    pub fn text(&self, s: &str, format: &IDWriteTextFormat, height: f32) -> Result<Text> {
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide, format, 100_000.0, height)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok(Text { layout, width: m.widthIncludingTrailingWhitespace })
        }
    }

    /// Like `text`, but cut off with "…" if wider than `max_width`.
    pub fn text_fit(
        &self,
        s: &str,
        format: &IDWriteTextFormat,
        max_width: f32,
        height: f32,
    ) -> Result<Text> {
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide, format, max_width.max(1.0), height)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(format)?;
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            layout.SetTrimming(&trimming, &sign)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok(Text { layout, width: m.widthIncludingTrailingWhitespace.min(max_width) })
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(t) = &self.target {
            unsafe {
                let _ = t.Resize(&D2D_SIZE_U { width: w, height: h });
            }
        }
    }

    /// Start a frame. Creates the render target on first use (or after the
    /// GPU device was lost) and clears to fully transparent.
    pub fn begin(&mut self, hwnd: HWND, w: u32, h: u32) -> Result<Frame> {
        if self.target.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: w, height: h },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            unsafe {
                let target = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
                let brush = target.CreateSolidColorBrush(&Color(1.0, 1.0, 1.0, 1.0).d2d(), None)?;
                self.target = Some(target);
                self.brush = Some(brush);
            }
        }
        let target = self.target.clone().unwrap();
        let brush = self.brush.clone().unwrap();
        unsafe {
            target.BeginDraw();
            target.Clear(Some(&Color(0.0, 0.0, 0.0, 0.0).d2d()));
        }
        Ok(Frame { target, brush })
    }

    /// Finish a frame. If the GPU device was lost, the target is dropped and
    /// recreated on the next frame.
    pub fn end(&mut self, frame: Frame) -> Result<()> {
        let r = unsafe { frame.target.EndDraw(None, None) };
        if r.is_err() {
            self.target = None;
            self.brush = None;
        }
        r
    }
}

pub struct Frame {
    target: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
}

impl Frame {
    pub fn round_rect(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.target.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    pub fn circle(&self, cx: f32, cy: f32, radius: f32, color: Color) {
        let e = D2D1_ELLIPSE {
            point: D2D_POINT_2F { x: cx, y: cy },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.target.FillEllipse(&e, &self.brush);
        }
    }

    pub fn text(&self, t: &Text, x: f32, y: f32, color: Color) {
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.target.DrawTextLayout(
                D2D_POINT_2F { x, y },
                &t.layout,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}
