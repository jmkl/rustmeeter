//! Compact horizontal slider used by the bus faders and the EQ band table.
//!
//! The stock WinUI `Slider` clips its thumb inside short rows, so this one draws its own 4 px
//! groove and a 16 px cap that always fits. The track is a single hit target: pressing it jumps
//! the cap to the pointer, and the track captures the pointer so a drag keeps tracking outside
//! its bounds. Values snap to `step`, measured from `min` like the WinUI slider does.

use windows_reactor::*;

use crate::theme::{CAP, CAP_EDGE, CYAN, GROOVE};

/// Row height: the cap exactly fits, so nothing is clipped.
const SLIDER_H: f64 = 16.0;

const CAP_W: f64 = 12.0;
const TRACK_H: f64 = 4.0;

/// Horizontal slider of a fixed `width` (the caller owns the layout).
///
/// `enabled: false` dims the control and detaches the pointer handlers, for parameters a band
/// kind does not use (e.g. gain on a shelf-less filter).
pub(crate) fn hslider(
    width: f64,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    enabled: bool,
    on_change: impl Fn(f64) + Clone + 'static,
) -> Border {
    let t = if max > min {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let usable = (width - CAP_W).max(0.0);
    let cap_x = CAP_W / 2.0 + t * usable;
    let groove_y = (SLIDER_H - TRACK_H) / 2.0;

    let groove = Border::new()
        .width(width)
        .height(TRACK_H)
        .corner_radius(TRACK_H / 2.0)
        .background(Brush::Solid(GROOVE))
        .canvas_left(0.0)
        .canvas_top(groove_y);

    let fill = Border::new()
        .width(cap_x)
        .height(TRACK_H)
        .corner_radius(TRACK_H / 2.0)
        .background(Brush::Solid(CYAN))
        .canvas_left(0.0)
        .canvas_top(groove_y);

    let cap = Border::new()
        .width(CAP_W)
        .height(SLIDER_H)
        .corner_radius(3.0)
        .background(Brush::Solid(CAP))
        .border_brush(Brush::Solid(CAP_EDGE))
        .border_thickness(Thickness::uniform(1.0))
        .canvas_left(cap_x - CAP_W / 2.0)
        .canvas_top(0.0)
        .content(
            Border::new()
                .width(3.0)
                .corner_radius(1.5)
                .background(Brush::Solid(CYAN))
                .horizontal_alignment(HorizontalAlignment::Center),
        );

    let track = Border::new()
        .width(width)
        .height(SLIDER_H)
        // Transparent rather than absent: the whole groove must hit-test.
        .background(Brush::Solid(Color::argb(0x00, 0x00, 0x00, 0x00)))
        .content(
            Canvas::new()
                .width(width)
                .height(SLIDER_H)
                .children((groove, fill, cap)),
        );

    if !enabled {
        return track.opacity(0.45);
    }

    let set_press = on_change.clone();
    let set_move = on_change;
    track
        .capture_pointer_on_press(true)
        .on_pointer_pressed(move |p: PointerEventInfo| {
            if p.is_left_button_pressed {
                set_press(value_at(p.x, width, min, max, step));
            }
        })
        .on_pointer_moved(move |p: PointerEventInfo| {
            if p.is_left_button_pressed {
                set_move(value_at(p.x, width, min, max, step));
            }
        })
}

/// Pointer x (relative to the track) to a snapped value.
fn value_at(x: f64, width: f64, min: f64, max: f64, step: f64) -> f64 {
    let usable = (width - CAP_W).max(1.0);
    let t = ((x - CAP_W / 2.0) / usable).clamp(0.0, 1.0);
    snap(min + t * (max - min), min, step)
}

/// Snaps to `step` from `min`, the same anchoring the stock slider uses.
fn snap(value: f64, min: f64, step: f64) -> f64 {
    if step <= 0.0 {
        value
    } else {
        min + ((value - min) / step).round() * step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_at_both_ends_hits_the_limits() {
        let (width, min, max) = (100.0, -15.0, 15.0);
        assert_eq!(value_at(0.0, width, min, max, 0.1), min);
        assert_eq!(value_at(width, width, min, max, 0.1), max);
    }

    #[test]
    fn values_snap_to_the_step() {
        // Middle of the track, snapped onto the 0.5 dB grid anchored at -15.
        assert!((value_at(53.0, 100.0, -15.0, 15.0, 0.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn snap_anchors_on_min() {
        let (min, step) = (1.301_03, 0.002);
        for k in 0..10 {
            let value = min + f64::from(k) * step;
            assert!((snap(value, min, step) - value).abs() < 1e-12);
        }
    }
}
