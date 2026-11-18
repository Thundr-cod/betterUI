// Pieces the flyouts lay out: shapes and text collected into a list (so the
// same layout both measures the window and paints it), and the volume-style
// slider row.

use windows::core::Result;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use crate::ui::render::{Color, Frame, Gfx, Text};
use crate::ui::{R, DIM, FAINT, FG};

pub enum Item {
    Rect(R, f32, Color),
    Circle(f32, f32, f32, Color),
    Text(Text, f32, f32, Color),
}

pub fn draw(frame: &Frame, items: &[Item]) {
    for item in items {
        match item {
            Item::Rect(r, radius, c) => frame.round_rect(r.x, r.y, r.w, r.h, *radius, *c),
            Item::Circle(x, y, radius, c) => frame.circle(*x, *y, *radius, *c),
            Item::Text(t, x, y, c) => frame.text(t, *x, *y, *c),
        }
    }
}

/// Where a slider row's parts ended up, for hit-testing.
pub struct Slider {
    /// The icon button on the left (mute).
    pub button: R,
    /// The area that grabs the slider.
    pub track_hit: R,
    pub track_x: f32,
    pub track_w: f32,
}

pub struct SliderStyle<'a> {
    pub gfx: &'a Gfx,
    pub icon_font: &'a IDWriteTextFormat,
    pub text_font: &'a IDWriteTextFormat,
    pub accent: Color,
    /// DPI scale, left/right padding, and window width.
    pub scale: f32,
    pub pad: f32,
    pub width: f32,
}

/// [icon button] [=====o-----] [ 42 ], 32 px tall, at `y`.
pub fn slider(st: &SliderStyle, items: &mut Vec<Item>, y: f32, glyph: &str, value: f32, muted: bool) -> Result<Slider> {
    let s = st.scale;
    let row_h = 32.0 * s;
    let button = R::new(st.pad, y, row_h, row_h);
    items.push(Item::Rect(button, 6.0 * s, FAINT));
    let icon = st.gfx.text(glyph, st.icon_font, row_h)?;
    let ix = button.x + (button.w - icon.width) / 2.0;
    items.push(Item::Text(icon, ix, y, if muted { DIM } else { FG }));

    let pct = st.gfx.text(&format!("{}", (value * 100.0).round() as i32), st.text_font, row_h)?;
    let track_x = button.x + button.w + 14.0 * s;
    let track_w = st.width - st.pad - 34.0 * s - 10.0 * s - track_x;
    let cy = y + row_h / 2.0;
    let v = value.clamp(0.0, 1.0);
    let fill = if muted { DIM } else { st.accent };
    items.push(Item::Rect(R::new(track_x, cy - 2.0 * s, track_w, 4.0 * s), 2.0 * s, Color(1.0, 1.0, 1.0, 0.18)));
    if v > 0.0 {
        items.push(Item::Rect(R::new(track_x, cy - 2.0 * s, track_w * v, 4.0 * s), 2.0 * s, fill));
    }
    let knob_x = track_x + track_w * v;
    items.push(Item::Circle(knob_x, cy, 8.0 * s, Color(0.27, 0.27, 0.30, 1.0)));
    items.push(Item::Circle(knob_x, cy, 4.5 * s, fill));
    let px = st.width - st.pad - pct.width;
    items.push(Item::Text(pct, px, y, DIM));
    Ok(Slider { button, track_hit: R::new(track_x - 10.0 * s, y, track_w + 20.0 * s, row_h), track_x, track_w })
}

/// The 0..1 value at `x` on the slider `id`, from the tracks recorded as
/// (id, track x, track width) when laid out.
pub fn track_value<T: PartialEq>(tracks: &[(T, f32, f32)], id: T, x: f32) -> Option<f32> {
    tracks.iter().find(|(t, _, _)| *t == id).filter(|(_, _, w)| *w > 0.0).map(|&(_, tx, tw)| (x - tx) / tw)
}
