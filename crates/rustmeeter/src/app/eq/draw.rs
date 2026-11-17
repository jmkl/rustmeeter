//! Direct2D rendering of the EQ graph.
//!
//! The canvas surface is opaque, so this draws the well, grid, 0 dB line and axis labels too;
//! XAML siblings painted beneath a swap-chain panel do not show through. The band dots stay XAML.

use rustmeeter_core::*;
use windows_canvas::{ColorF, DrawContext, PathBuilder, Rect, Vector2};
use windows_reactor::Color;
use windows_text::TextFormat;

use super::geometry::{freq_to_x, gain_to_y, x_to_freq};
use super::{EQ_COLS, EQ_FS, EQ_H, EQ_W, EQ_WELL};
use crate::theme::DIM;

const CURVE: Color = Color::rgb(0x60, 0xA5, 0xFA);
const CURVE_FILL: Color = Color::argb(0x40, 0x60, 0xA5, 0xFA);
const GRID_STRONG: Color = Color::argb(0x40, 0xFF, 0xFF, 0xFF);
const GRID_WEAK: Color = Color::argb(0x18, 0xFF, 0xFF, 0xFF);

/// Converts a Reactor palette color to a Direct2D color.
fn color_f(color: Color) -> ColorF {
    ColorF::from_rgba8(color.r, color.g, color.b, color.a)
}

/// Draws the whole EQ graph with Direct2D: the well, the grid, the 0 dB line, the axis labels,
/// then the response area and polyline. Direct2D anti-aliases the polyline, so the curve is smooth
/// instead of the 5 px staircase a column of `Border`s produces.
pub(super) fn draw_eq_curve(ctx: &DrawContext<'_>, eq: &EqSettings) -> windows_canvas::Result<()> {
    ctx.clear(EQ_WELL);

    let width = EQ_W as f32;
    let height = EQ_H as f32;
    let baseline = height / 2.0;

    // Grid: horizontal gain lines (0 dB strongest), then vertical frequency lines.
    for (db, strong) in [
        (-12.0_f32, false),
        (-6.0, false),
        (0.0, true),
        (6.0, false),
        (12.0, false),
    ] {
        let y = (gain_to_y(db) as f32).floor() + 0.5;
        let color = if strong { GRID_STRONG } else { GRID_WEAK };
        let brush = ctx.create_solid_brush(color_f(color))?;
        ctx.draw_line(Vector2::new(0.0, y), Vector2::new(width, y), &brush, 1.0);
    }
    for (hz, strong) in [
        (50.0_f32, false),
        (100.0, true),
        (200.0, false),
        (500.0, false),
        (1000.0, true),
        (2000.0, false),
        (5000.0, false),
        (10000.0, true),
    ] {
        let x = (freq_to_x(hz) as f32).floor() + 0.5;
        let color = if strong { GRID_STRONG } else { GRID_WEAK };
        let brush = ctx.create_solid_brush(color_f(color))?;
        ctx.draw_line(Vector2::new(x, 0.0), Vector2::new(x, height), &brush, 1.0);
    }

    // Axis labels: frequency along the bottom, gain down the left edge.
    let label_format = TextFormat::new("Segoe UI", 8.5);
    let label_brush = ctx.create_solid_brush(color_f(DIM))?;
    for (hz, text) in [(100.0_f32, "100"), (1000.0, "1k"), (10000.0, "10k")] {
        let left = freq_to_x(hz) as f32 + 3.0;
        let rect = Rect::new(left, height - 13.0, left + 44.0, height - 1.0);
        ctx.draw_text(text, &label_format, &rect, &label_brush);
    }
    for (db, text) in [(12.0_f32, "+12"), (0.0, "0"), (-12.0, "-12")] {
        let top = gain_to_y(db) as f32 + 1.0;
        let rect = Rect::new(3.0, top, 33.0, top + 12.0);
        ctx.draw_text(text, &label_format, &rect, &label_brush);
    }

    if eq.bypass || eq.bands.is_empty() {
        return Ok(());
    }

    let step = (EQ_W / EQ_COLS as f64) as f32;
    let response: Vec<Vector2> = (0..=EQ_COLS)
        .map(|i| {
            let x = i as f32 * step;
            let db = eq
                .response_db(x_to_freq(f64::from(x)), EQ_FS)
                .clamp(EQ_GAIN_MIN, EQ_GAIN_MAX);
            Vector2::new(x, gain_to_y(db) as f32)
        })
        .collect();

    // Filled area between the response and the 0 dB line (closed along the baseline).
    let area = std::iter::once(Vector2::new(0.0, baseline))
        .chain(response.iter().copied())
        .chain(std::iter::once(Vector2::new(width, baseline)));
    let area = PathBuilder::new(ctx.device())?.polygon(area)?;
    ctx.fill_path(&area, &ctx.create_solid_brush(color_f(CURVE_FILL))?);

    // The response line itself.
    let mut points = response.into_iter();
    let first = points.next().expect("EQ_COLS + 1 samples");
    let mut figure = PathBuilder::new(ctx.device())?.begin_hollow(first);
    for point in points {
        figure = figure.line_to(point);
    }
    let line = figure.end_open().build()?;
    ctx.draw_path(&line, &ctx.create_solid_brush(color_f(CURVE))?, 2.0);
    Ok(())
}
