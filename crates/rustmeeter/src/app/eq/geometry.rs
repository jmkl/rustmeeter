//! Log-frequency / linear-gain mapping for the EQ graph.

use rustmeeter_core::*;

use super::{EQ_H, EQ_W};

pub(super) fn freq_to_x(freq: f32) -> f64 {
    (f64::from(freq.clamp(EQ_FREQ_MIN, EQ_FREQ_MAX)) / f64::from(EQ_FREQ_MIN)).log10() / 3.0 * EQ_W
}

pub(super) fn x_to_freq(x: f64) -> f32 {
    (f64::from(EQ_FREQ_MIN) * 10f64.powf(x.clamp(0.0, EQ_W) / EQ_W * 3.0)) as f32
}

pub(super) fn gain_to_y(gain: f32) -> f64 {
    EQ_H / 2.0
        - f64::from(gain.clamp(EQ_GAIN_MIN, EQ_GAIN_MAX)) / f64::from(EQ_GAIN_MAX) * EQ_H / 2.0
}

pub(super) fn y_to_gain(y: f64) -> f32 {
    ((EQ_H / 2.0 - y) / (EQ_H / 2.0) * f64::from(EQ_GAIN_MAX)) as f32
}
