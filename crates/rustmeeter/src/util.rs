//! Small display and domain helpers shared by the panels.

use windows_reactor::Color;

/// Keeps a hair below zero from rendering as `-0`.
pub(crate) fn zeroed(db: f32) -> f32 {
    if db.abs() < 0.05 { 0.0 } else { db }
}

pub(crate) fn dim_color(color: Color, factor: f32) -> Color {
    let factor = factor.clamp(0.0, 1.0);

    Color::rgb(
        (color.r as f32 * factor).round() as u8,
        (color.g as f32 * factor).round() as u8,
        (color.b as f32 * factor).round() as u8,
    )
}
pub(crate) fn shift_hue(color: Color, degrees: f32) -> Color {
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let h = (h + degrees).rem_euclid(360.0);

    let s = if max == 0.0 { 0.0 } else { delta / max };
    let v = max;

    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    Color::rgb(
        ((r + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}