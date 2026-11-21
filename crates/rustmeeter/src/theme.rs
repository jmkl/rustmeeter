//! Palette and the small, reusable view builders shared by every panel.

use windows_reactor::*;

use crate::{component::button::{AcButton, AcButtonInput, Side}, util::{dim_color, shift_hue}};

// Palette. The window is dark acrylic, so cards are translucent white on purpose: they pick up
// the backdrop instead of painting over it.
pub(crate) const CARD: Color = Color::argb(0x14, 0xFF, 0xFF, 0xFF);
pub(crate) const CARD_EDGE: Color = Color::argb(0x22, 0xFF, 0xFF, 0xFF);
pub(crate) const WELL: Color = Color::argb(0x33, 0x00, 0x00, 0x00);
pub(crate) const FILL_OFF: Color = Color::argb(0x2A, 0xFF, 0xFF, 0xFF);
/// Very light scrim so text stays readable over a bright wallpaper.
pub(crate) const SCRIM: Color = Color::argb(0x40, 0x00, 0x00, 0x00);
/// Backdrop of the full-window EQ editor: near-opaque, so the mixer behind it is out of the way.
pub(crate) const MODAL: Color = Color::argb(0xF2, 0x0E, 0x12, 0x1A);
pub(crate) const DIM: Color = Color::argb(0xB4, 0xFF, 0xFF, 0xFF);
pub(crate) const TEXT: Color = Color::argb(0xE6, 0xFF, 0xFF, 0xFF);
pub(crate) const INK: Color = Color::rgb(0x0A, 0x12, 0x1A);
pub(crate) const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
pub(crate) const GREEN: Color = Color::rgb(0x34, 0xD3, 0x99);
pub(crate) const BLUE: Color = Color::rgb(0x60, 0xA5, 0xFA);
pub(crate) const AMBER: Color = Color::rgb(0xFB, 0xBF, 0x24);
pub(crate) const RED: Color = Color::rgb(0xF8, 0x71, 0x71);
pub(crate) const RED_STRONG: Color = Color::rgb(0xDC, 0x26, 0x26);
/// Channel fader: cyan level fill, the cap it drives and the groove it rides in.
pub(crate) const CYAN: Color = Color::rgb(0x3F, 0xC9, 0xE8);
pub(crate) const CAP: Color = Color::rgb(0xC9, 0xD2, 0xDC);
pub(crate) const CAP_EDGE: Color = Color::argb(0xE6, 0x0A, 0x12, 0x1A);
pub(crate) const GROOVE: Color = Color::rgb(0x10, 0x15, 0x1E);
pub(crate) const TRACK_BG: Color = Color::argb(0x22, 0x10, 0x10, 0x10);

/// Translucent, rounded panel used for every group.
pub(crate) fn card(child: impl Into<View>) -> Border {
    Border::new()
        .background(Brush::Solid(CARD))
        .border_brush(Brush::Solid(CARD_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(10.0)
        .padding(Thickness::uniform(8.0))
        .content(child)
}

/// Recessed area that holds the strip cards, so leftover space reads as part of the mixer.
pub(crate) fn rack(child: impl Into<View>) -> Border {
    Border::new()
        .background(Brush::Solid(WELL))
        .border_brush(Brush::Solid(CARD_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(10.0)
        .padding(Thickness::uniform(6.0))
        .content(child)
}

pub(crate) fn badge(text: &str, color: Color) -> Border {
    Border::new()
        .width(18.0)
        .height(18.0)
        .corner_radius(9.0)
        .background(Brush::Solid(color))
        .content(
            TextBlock::new()
                .text(text)
                .font_size(9.5)
                .font_weight(FontWeight::BOLD)
                .foreground(Brush::Solid(INK))
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center),
        )
}

pub(crate) fn label_text(text: &str, size: f64, color: Color) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(size)
        .foreground(Brush::Solid(color))
}

pub(crate) fn section_label(text: &str) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(9.5)
        .character_spacing(120)
        .font_weight(FontWeight::SEMI_BOLD)
        .foreground(Brush::Solid(DIM))
        .vertical_alignment(VerticalAlignment::Center)
}

/// Same voice as `section_label`, one step smaller: the one-line caption under a control.
pub(crate) fn caption(text: &str, color: Color) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(8.5)
        .character_spacing(60)
        .font_weight(FontWeight::SEMI_BOLD)
        .foreground(Brush::Solid(color))
        .text_trimming(TextTrimming::CharacterEllipsis)
        .vertical_alignment(VerticalAlignment::Center)
}

/// Small directional glyph for "this goes there" labels.
pub(crate) fn arrow() -> TextBlock {
    TextBlock::new()
        .text("\u{2192}")
        .font_size(11.0)
        .foreground(Brush::Solid(DIM))
        .vertical_alignment(VerticalAlignment::Center)
}

/// A and B are brand colours, not accents: the route chips, the badges and the faders all agree.
pub(crate) fn bus_color(bus: usize) -> Color {
    if bus == 0 { GREEN } else { BLUE }
}

/// Place an already-built `View` in a grid cell. Declaration types carry `grid_column` themselves;
/// a `View` (the result of `.tooltip()`, for instance) does not, so wrap it.
pub(crate) fn in_column(child: impl Into<View>, column: i32) -> Border {
    Border::new().grid_column(column).content(child)
}


pub(crate) fn action_button(
    text: &str,
    bg: Color,
    fg: Color,
    side:Side,
    click: impl IntoUnitCallback,
) -> View{
  View::component::<AcButton>(AcButtonInput{
    glyph: text.into(),
    hover_bg: dim_color(bg, 0.5),
    bg,
    pressed_bg: shift_hue(bg, 15.),
    fg,
    on_click: click.into_unit_callback(),
    side,
})

}



/// Small square toggle. The button's own padding would clip a single glyph, so the label is
/// pulled outward with a negative margin (WinUI buttons do not clip their content).
pub(crate) fn chip(text: &str, bg: Color, fg: Color) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .background(Brush::Solid(bg))
        .height(22.0)
        .content(
            TextBlock::new()
                .text(text)
                .font_size(10.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(fg))
                .text_alignment(TextAlignment::Center)
                .margin(Thickness::new(-10.0, -5.0, -10.0, -5.0)),
        )
}

/// The little `×` used by strip headers and EQ rows.
pub(crate) fn x_button(click: impl IntoUnitCallback) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .width(18.0)
        .height(18.0)
        .background(Brush::Solid(FILL_OFF))
        .content(label_text("×", 11.0, RED).margin(Thickness::new(-6.0, -6.0, -6.0, -6.0)))
        .on_click(click)
}

/// A small filled dot, for "state at a glance" rows.
pub(crate) fn dot(color: Color) -> Border {
    Border::new()
        .width(8.0)
        .height(8.0)
        .corner_radius(4.0)
        .background(Brush::Solid(color))
}

/// A tiny level history: the "is it hearing anything?" graph.
///
/// Deliberately unlabelled and 16 px tall: it answers one question, and the answer is the shape.
/// Two details make it readable at this size. Levels are converted to dB and mapped over
/// −60…0 dB, because linear RMS puts normal speech within 2 px of the floor. And when there are
/// more samples than bars, each bar keeps the loudest sample in its bucket, so a short word stays
/// visible instead of being drawn 1 px wide and skipped by the display.
pub(crate) fn spark(values: &[f32], width: f64, height: f64) -> Border {
    const GAP: f64 = 1.0;
    const MIN_BAR: f64 = 3.0;
    let count = ((width + GAP) / (MIN_BAR + GAP)).floor().max(1.0) as usize;
    let count = count.min(values.len().max(1));
    let bar_w = ((width - (count - 1) as f64 * GAP) / count as f64).max(1.0);

    // One bar per bucket; `max` so a transient is not averaged away.
    let levels: Vec<f32> = (0..count)
        .map(|i| {
            let start = i * values.len() / count;
            let end = ((i + 1) * values.len() / count)
                .max(start + 1)
                .min(values.len());
            values[start..end].iter().copied().fold(0.0, f32::max)
        })
        .collect();

    let mut bars: Vec<View> = vec![
        // Baseline: "flat" reads as a line at the bottom rather than as an empty box.
        Border::new()
            .width(width)
            .height(1.0)
            .background(Brush::Solid(Color::argb(0x66, 0x3F, 0xC9, 0xE8)))
            .canvas_left(0.0)
            .canvas_top(height - 1.0)
            .into(),
    ];
    bars.extend(levels.iter().enumerate().map(|(i, &v)| {
        let db = 20.0 * v.max(1e-6).log10(); // 0..1 linear -> dBFS
        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let h = (f64::from(norm) * height).max(1.0);
        Border::new()
            .width(bar_w)
            .height(h)
            .corner_radius(0.5)
            .background(Brush::Solid(CYAN))
            .canvas_left(i as f64 * (bar_w + GAP))
            .canvas_top(height - h)
            .into()
    }));

    Border::new()
        .width(width)
        .height(height)
        .corner_radius(2.0)
        .background(Brush::Solid(GROOVE))
        .content(Canvas::new().width(width).height(height).children(bars))
}

pub(crate) fn run_state(running: bool) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(6.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            dot(if running { GREEN } else { RED }),
            TextBlock::new()
                .text(if running { "Running" } else { "Stopped" })
                .font_size(10.5)
                .foreground(Brush::Solid(DIM)),
        ))
        .into()
}

/// Vertical segmented peak meter, in the style of a console LED ladder.
///
/// Segments are lit from the bottom up to `db`, and coloured by where they sit: green below
/// -12 dB, amber above it. The scale runs from `METER_MIN_DB` (bottom) to 0 dB (top).
pub(crate) fn vmeter(db: f32, width: f64, height: f64) -> Border {
    const SEG_H: f64 = 4.0;
    const SEG_GAP: f64 = 2.0;
    let count = (((height + SEG_GAP) / (SEG_H + SEG_GAP)).floor() as usize).max(1);
    let seg_h = (height - (count - 1) as f64 * SEG_GAP) / count as f64;

    let segments: Vec<View> = (0..count)
        .map(|i| {
            let i = i as f64;
            let centre = (i * (seg_h + SEG_GAP) + seg_h / 2.0) / height;
            let seg_db = METER_MIN_DB + (centre * f64::from(METER_MAX_DB - METER_MIN_DB)) as f32;
            let lit = db >= seg_db;
            Border::new()
                .width(width)
                .height(seg_h)
                .corner_radius(1.0)
                .background(Brush::Solid(if lit {
                    segment_color(seg_db)
                } else {
                    TRACK_BG
                }))
                .canvas_left(0.0)
                .canvas_top(height - (i + 1.0) * seg_h - i * SEG_GAP)
                .into()
        })
        .collect();

    Border::new()
        .width(width)
        .height(height)
        .corner_radius((width / 2.0).min(3.0))
        .background(Brush::Solid(TRACK_BG))
        .content(Canvas::new().width(width).height(height).children(segments))
}

/// Bottom of the meter scale; `level_db()` is floored at -90, so anything quieter reads empty.
const METER_MIN_DB: f32 = -60.0;
const METER_MAX_DB: f32 = 0.0;

fn segment_color(db: f32) -> Color {
    if db >= -12.0 { AMBER } else { GREEN }
}
