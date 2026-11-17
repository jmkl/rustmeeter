//! The EQ band table: header row plus one row per band.

use std::sync::Arc;

use rustmeeter_core::*;
use windows_reactor::*;

use super::BAND_COLORS;
use crate::hslider::hslider;
use crate::theme::*;
use crate::util::*;

// Column widths. The three slider cells also hold a value readout, so their sliders are
// `cell - READOUT_W - CELL_GAP` wide. Only the fixed columns are constants: the slider columns
// share whatever the modal leaves, so the table fits the panel instead of a fixed 560 px.
const COL_ON: f64 = 26.0;
const COL_CURVE: f64 = 100.0;
const COL_REMOVE: f64 = 22.0;
const READOUT_W: f64 = 44.0;
const CELL_GAP: f64 = 4.0;
const FIXED_W: f64 = COL_ON + COL_CURVE + COL_REMOVE + 5.0 * CELL_GAP;

/// Slider column weights, preserving the old 122 : 100 : 150 proportions.
const GAIN_WEIGHT: f64 = 1.22;
const Q_WEIGHT: f64 = 1.0;
const FREQ_WEIGHT: f64 = 1.5;
const WEIGHT_SUM: f64 = GAIN_WEIGHT + Q_WEIGHT + FREQ_WEIGHT;

/// Table layout: on, curve, gain, Q, frequency, remove.
pub(super) fn eq_columns(table_w: f64) -> [GridLength; 6] {
    [
        GridLength::Pixel(COL_ON),
        GridLength::Pixel(COL_CURVE),
        GridLength::Pixel(cell_w(table_w, GAIN_WEIGHT)),
        GridLength::Pixel(cell_w(table_w, Q_WEIGHT)),
        GridLength::Pixel(cell_w(table_w, FREQ_WEIGHT)),
        GridLength::Pixel(COL_REMOVE),
    ]
}

fn cell_w(table_w: f64, weight: f64) -> f64 {
    (table_w - FIXED_W).max(0.0) * weight / WEIGHT_SUM
}

fn slider_w(table_w: f64, weight: f64) -> f64 {
    (cell_w(table_w, weight) - READOUT_W - CELL_GAP).max(24.0)
}

pub(super) fn eq_head(text: &str, column: i32) -> TextBlock {
    TextBlock::new()
        .grid_column(column)
        .text(text)
        .font_size(9.0)
        .foreground(Brush::Solid(DIM))
}

/// Slider + value readout in one table cell.
fn eq_cell(column: i32, slider: impl Into<View>, readout: String) -> View {
    Grid::new()
        .grid_column(column)
        .columns([GridLength::STAR, GridLength::Pixel(READOUT_W)])
        .column_spacing(CELL_GAP)
        .children((
            slider,
            TextBlock::new()
                .grid_column(1)
                .text(readout)
                .font_size(9.5)
                .foreground(Brush::Solid(DIM))
                .vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

/// One band in the table. Every widget writes straight to the strip's `Params` (live, no restart).
pub(super) fn eq_row(params: &Arc<Params>, i: usize, b: &EqBand, table_w: f64) -> Grid {
    let color = BAND_COLORS[i % BAND_COLORS.len()];
    let on = b.enabled;
    let uses_gain = b.kind.uses_gain();
    let (p_on, p_kind, p_gain, p_q, p_freq, p_del) = (
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
        params.clone(),
    );
    let kinds: Vec<String> = EqKind::ALL.iter().map(|k| k.label().to_string()).collect();
    let (lo_hz, hi_hz) = (
        f64::from(EQ_FREQ_MIN).log10(),
        f64::from(EQ_FREQ_MAX).log10(),
    );

    Grid::new()
        .columns(eq_columns(table_w))
        .column_spacing(CELL_GAP)
        .children((
            chip(
                &(i + 1).to_string(),
                if on { color } else { FILL_OFF },
                if on { INK } else { TEXT },
            )
            .width(26.0)
            .on_click(move || {
                p_on.update_eq(|s| {
                    if let Some(band) = s.bands.get_mut(i) {
                        band.enabled = !on;
                    }
                })
            }),
            ComboBox::new()
                .grid_column(1)
                .width(100.0)
                .items_source(kinds)
                .selected_index(Some(b.kind.index()))
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some(kind) = index.and_then(|n| EqKind::ALL.get(n)).copied() {
                        p_kind.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.kind = kind;
                            }
                        });
                    }
                }),
            eq_cell(
                2,
                hslider(
                    slider_w(table_w, GAIN_WEIGHT),
                    f64::from(b.gain_db),
                    f64::from(EQ_GAIN_MIN),
                    f64::from(EQ_GAIN_MAX),
                    0.1,
                    uses_gain,
                    move |v| {
                        p_gain.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.gain_db = v as f32;
                            }
                        })
                    },
                ),
                if uses_gain {
                    format!("{:+.1} dB", zeroed(b.gain_db))
                } else {
                    "-".to_string()
                },
            ),
            // Q and frequency sliders are logarithmic: the slider value is log10 of the parameter.
            eq_cell(
                3,
                hslider(
                    slider_w(table_w, Q_WEIGHT),
                    f64::from(b.q).log10(),
                    f64::from(EQ_Q_MIN).log10(),
                    f64::from(EQ_Q_MAX).log10(),
                    0.01,
                    true,
                    move |v| {
                        p_q.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.q = 10f64.powf(v) as f32;
                            }
                        })
                    },
                ),
                format!("{:.2}", b.q),
            ),
            eq_cell(
                4,
                hslider(
                    slider_w(table_w, FREQ_WEIGHT),
                    f64::from(b.freq_hz).log10(),
                    lo_hz,
                    hi_hz,
                    0.002,
                    true,
                    move |v| {
                        p_freq.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.freq_hz = 10f64.powf(v) as f32;
                            }
                        })
                    },
                ),
                fmt_hz(b.freq_hz),
            ),
            x_button(move || {
                p_del.update_eq(|s| {
                    if i < s.bands.len() {
                        s.bands.remove(i);
                    }
                })
            })
            .grid_column(5),
        ))
}

fn fmt_hz(freq: f32) -> String {
    if freq >= 1000.0 {
        format!("{:.2} kHz", freq / 1000.0)
    } else {
        format!("{freq:.0} Hz")
    }
}
