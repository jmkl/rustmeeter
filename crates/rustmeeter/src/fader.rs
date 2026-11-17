//! Custom vertical channel fader: printed dB scale, dark groove, cyan level fill and a wide cap.
//!
//! WinUI's stock `Slider` cannot be reshaped from Reactor, so the fader is composed from
//! primitives. The whole well is a single hit target: pressing it jumps the cap to the pointer,
//! and the well captures the pointer so a drag keeps tracking outside its bounds. Values snap to
//! 0.5 dB, matching the old slider's step.

use rustmeeter_core::{MAX_DB, MIN_DB};
use windows_reactor::*;

use crate::theme::{CAP, CAP_EDGE, CYAN, DIM, GROOVE, vmeter};

/// Height shared by the groove, the scale and the meter.
pub(crate) const FADER_H: f64 = 150.0;

const CAP_W: f64 = 35.0;
const CAP_H: f64 = 16.0;
const WELL_W: f64 = 52.0;
const TRACK_W: f64 = 5.0;
const LABEL_W: f64 = 30.0;
/// Width of the level meter next to the groove.
const METER_W: f64 = 12.0;

/// Printed scale, top to bottom. The tick spacing is deliberately even while the dB steps are
/// not, so the range around 0 dB gets most of the travel (a console-style fader taper).
const TICKS: [f32; 8] = [MAX_DB, 6.0, 0.0, -6.0, -12.0, -24.0, -40.0, MIN_DB];

/// Gain fader plus level meter, matching the strip card's height.
pub(crate) fn fader(
    value_db: f32,
    level_db: f32,
    on_change: impl Fn(f32) + Clone + 'static,
) -> View {
    let press = on_change.clone();
    let drag = on_change;

    let usable = FADER_H - CAP_H;
    let cap_y = CAP_H / 2.0 + t_at(value_db) * usable;
    let fill_h = (FADER_H - cap_y).max(0.0);

    let labels: Vec<View> = TICKS
        .iter()
        .enumerate()
        .map(|(i, db)| {
            let y = CAP_H / 2.0 + i as f64 / (TICKS.len() - 1) as f64 * usable;
            TextBlock::new()
                .text(if *db <= MIN_DB {
                    "-inf".to_string()
                } else {
                    format!("{db:.0}")
                })
                .font_size(9.0)
                .foreground(Brush::Solid(DIM))
                .text_alignment(TextAlignment::Right)
                .width(LABEL_W)
                .height(14.0)
                .canvas_left(0.0)
                .canvas_top(y - 7.0)
                .into()
        })
        .collect();

    let scale = Canvas::new()
        .width(LABEL_W)
        .height(FADER_H)
        .children(labels);

    let groove = Border::new()
        .width(TRACK_W)
        .height(FADER_H)
        .corner_radius(TRACK_W / 2.0)
        .background(Brush::Solid(GROOVE))
        .canvas_left((WELL_W - TRACK_W) / 2.0)
        .canvas_top(0.0);

    let fill = Border::new()
        .width(TRACK_W)
        .height(fill_h)
        .corner_radius(TRACK_W / 2.0)
        .background(Brush::Solid(CYAN))
        .canvas_left((WELL_W - TRACK_W) / 2.0)
        .canvas_top(FADER_H - fill_h);

    let cap = Border::new()
        .width(CAP_W)
        .height(CAP_H)
        .corner_radius(3.0)
        .background(Brush::Solid(CAP))
        .border_brush(Brush::Solid(CAP_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .canvas_left((WELL_W - CAP_W) / 2.0)
        .canvas_top(cap_y - CAP_H / 2.0)
        .content(
            Border::new()
                .height(3.0)
                .corner_radius(1.5)
                .background(Brush::Solid(CYAN))
                .vertical_alignment(VerticalAlignment::Center),
        );

    let well = Border::new()
        .width(WELL_W)
        .height(FADER_H)
        // Transparent rather than absent: the gaps between the children must still hit-test.
        .background(Brush::Solid(Color::argb(0x00, 0x00, 0x00, 0x00)))
        .capture_pointer_on_press(true)
        .on_pointer_pressed(move |p: PointerEventInfo| {
            if p.is_left_button_pressed {
                press(snap(db_at(y_to_t(p.y))));
            }
        })
        .on_pointer_moved(move |p: PointerEventInfo| {
            if p.is_left_button_pressed {
                drag(snap(db_at(y_to_t(p.y))));
            }
        })
        .content(
            Canvas::new()
                .width(WELL_W)
                .height(FADER_H)
                .children((groove, fill, cap)),
        );

    Grid::new()
        .columns([
            GridLength::Pixel(LABEL_W),
            GridLength::Pixel(WELL_W),
            GridLength::Auto,
        ])
        .column_spacing(6.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .children((
            scale,
            well.grid_column(1),
            vmeter(level_db, METER_W, FADER_H).grid_column(2),
        ))
        .into()
}

/// Pointer y (relative to the well) to travel, 0 at the top tick and 1 at the bottom tick.
fn y_to_t(y: f64) -> f64 {
    ((y - CAP_H / 2.0) / (FADER_H - CAP_H)).clamp(0.0, 1.0)
}

/// Travel to dB: piecewise linear between the printed ticks.
fn db_at(t: f64) -> f32 {
    let x = t.clamp(0.0, 1.0) as f32 * (TICKS.len() - 1) as f32;
    let i = (x.floor() as usize).min(TICKS.len() - 2);
    let f = x - i as f32;
    TICKS[i] + (TICKS[i + 1] - TICKS[i]) * f
}

/// dB to travel: inverse of [`db_at`].
fn t_at(db: f32) -> f64 {
    let db = db.clamp(MIN_DB, MAX_DB);
    for i in 0..TICKS.len() - 1 {
        let (hi, lo) = (TICKS[i], TICKS[i + 1]);
        if db >= lo {
            let f = f64::from((hi - db) / (hi - lo));
            return (i as f64 + f) / (TICKS.len() - 1) as f64;
        }
    }
    1.0
}

/// Drags land on half-dB steps, so the readout stays legible.
fn snap(db: f32) -> f32 {
    (db * 2.0).round() / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The printed ticks must sit exactly where the mapping says they do.
    #[test]
    fn ticks_round_trip() {
        for (i, db) in TICKS.iter().enumerate() {
            let t = i as f64 / (TICKS.len() - 1) as f64;
            assert!((db_at(t) - *db).abs() < 0.001, "{db} dB at {t}");
            assert!((t_at(*db) - t).abs() < 1e-9, "{db} dB at {t}");
        }
    }

    #[test]
    fn travel_is_monotonic() {
        let mut previous = f32::INFINITY;
        for step in 0..=100 {
            let db = db_at(f64::from(step) / 100.0);
            assert!(db <= previous + 1e-4);
            previous = db;
        }
    }
}
