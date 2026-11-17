//! The EQ band table: header row plus one row per band.

use std::sync::Arc;

use rustmeeter_core::*;
use windows_reactor::*;

use super::BAND_COLORS;
use crate::theme::*;
use crate::util::*;

/// Table layout: on, curve, gain, Q, frequency, remove (about 560 px, same as the plot).
pub(super) fn eq_columns() -> [GridLength; 6] {
    [
        GridLength::Pixel(26.0),
        GridLength::Pixel(100.0),
        GridLength::Pixel(122.0),
        GridLength::Pixel(100.0),
        GridLength::Pixel(150.0),
        GridLength::Pixel(22.0),
    ]
}

pub(super) fn eq_head(text: &str, column: i32) -> TextBlock {
    TextBlock::new()
        .grid_column(column)
        .text(text)
        .font_size(9.0)
        .foreground(Brush::Solid(DIM))
}

/// Slider + value readout in one table cell.
fn eq_cell(column: i32, slider: Slider, readout: String) -> View {
    Grid::new()
        .grid_column(column)
        .columns([GridLength::STAR, GridLength::Pixel(44.0)])
        .column_spacing(4.0)
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
pub(super) fn eq_row(params: &Arc<Params>, i: usize, b: &EqBand) -> Grid {
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
        .columns(eq_columns())
        .column_spacing(4.0)
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
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(f64::from(EQ_GAIN_MIN))
                    .maximum(f64::from(EQ_GAIN_MAX))
                    .step_frequency(0.1)
                    .value(f64::from(b.gain_db))
                    .is_enabled(uses_gain)
                    .on_value_changed(move |v: f64| {
                        p_gain.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.gain_db = v as f32;
                            }
                        })
                    }),
                if uses_gain {
                    format!("{:+.1} dB", zeroed(b.gain_db))
                } else {
                    "-".to_string()
                },
            ),
            // Q and frequency sliders are logarithmic: the slider value is log10 of the parameter.
            eq_cell(
                3,
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(f64::from(EQ_Q_MIN).log10())
                    .maximum(f64::from(EQ_Q_MAX).log10())
                    .step_frequency(0.01)
                    .value(f64::from(b.q).log10())
                    .on_value_changed(move |v: f64| {
                        p_q.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.q = 10f64.powf(v) as f32;
                            }
                        })
                    }),
                format!("{:.2}", b.q),
            ),
            eq_cell(
                4,
                Slider::new()
                    .height(22.0)
                    .min_height(22.0)
                    .minimum(lo_hz)
                    .maximum(hi_hz)
                    .step_frequency(0.002)
                    .value(f64::from(b.freq_hz).log10())
                    .on_value_changed(move |v: f64| {
                        p_freq.update_eq(|s| {
                            if let Some(band) = s.bands.get_mut(i) {
                                band.freq_hz = 10f64.powf(v) as f32;
                            }
                        })
                    }),
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
