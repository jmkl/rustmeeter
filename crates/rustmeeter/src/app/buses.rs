//! Bus panel: the two output buses with their faders and meters.

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use super::strips::mute_chip;
use crate::hslider::hslider;
use crate::theme::*;
use crate::util::*;

/// The side column is a fixed 300 px; this leaves a small margin inside the card.
const BUS_SLIDER_W: f64 = 260.0;

impl MixerApp {
    pub(crate) fn side(&self, view: &MixerView) -> View {
        Grid::new()
            .grid_row(3)
            .grid_column(1)
            .rows([GridLength::Auto, GridLength::STAR])
            .row_spacing(6.0)
            .children((
                section_label("BUSES"),
                card(
                    Grid::new()
                        .rows([GridLength::Auto, GridLength::Auto])
                        .row_spacing(8.0)
                        .children((self.bus_row(0, view), self.bus_row(1, view).grid_row(1))),
                )
                .grid_row(1),
            ))
            .into()
    }

    fn bus_row(&self, bus: usize, view: &MixerView) -> Grid {
        let p = &view.buses[bus].params;
        let gain = p.clone();
        let muted = p.muted();
        let has_device = view.buses[bus].device.is_some();
        let device = view.buses[bus]
            .device
            .clone()
            .unwrap_or_else(|| "no device".to_string());
        let level = ((p.level_db() + 60.0) / 60.0).clamp(0.0, 1.0) as f64;

        Grid::new()
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Auto])
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
                GridLength::Auto,
            ])
            .column_spacing(6.0)
            .row_spacing(3.0)
            .children((
                badge(bus_name(bus), if bus == 0 { GREEN } else { BLUE }),
                TextBlock::new()
                    .grid_column(1)
                    .text(&device)
                    .font_size(10.5)
                    .foreground(Brush::Solid(if has_device {
                        Color::argb(0xE6, 0xFF, 0xFF, 0xFF)
                    } else {
                        DIM
                    }))
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
                mute_chip(p).grid_column(2).height(20.0),
                TextBlock::new()
                    .grid_column(3)
                    .text(trim_db(p.gain_db()))
                    .font_size(10.0)
                    .foreground(Brush::Solid(if muted { RED } else { DIM }))
                    .text_alignment(TextAlignment::Right)
                    .width(32.0)
                    .vertical_alignment(VerticalAlignment::Center),
                hslider(
                    BUS_SLIDER_W,
                    f64::from(p.gain_db()),
                    f64::from(MIN_DB),
                    f64::from(MAX_DB),
                    0.5,
                    true,
                    move |v| gain.set_gain_db(v as f32),
                )
                .grid_row(1)
                .grid_column_span(4),
                ProgressBar::new()
                    .grid_row(2)
                    .grid_column_span(4)
                    .maximum(1.0)
                    .value(level)
                    .height(3.0),
            ))
            .into()
    }
}
