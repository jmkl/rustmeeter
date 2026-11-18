//! Bus panel: the two output buses with their faders and meters.

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use super::strips::mute_chip;
use crate::fader::fader;
use crate::theme::*;
use crate::util::*;

impl MixerApp {
    pub(crate) fn side(&self, view: &MixerView) -> View {
        Grid::new()
            .grid_row(2)
            .grid_column(1)
            .rows([GridLength::Auto, GridLength::STAR])
            .row_spacing(6.0)
            .children((
                section_label("OUTPUT BUSES"),
                card(
                    Grid::new()
                        .rows([GridLength::STAR])
                        .columns([GridLength::Auto, GridLength::Auto])
                        .column_spacing(6.0)
                        .children((self.bus_card(0, view), self.bus_card(1, view))),
                )
                .grid_row(1),
            ))
            .into()
    }

    fn bus_card(&self, bus: usize, view: &MixerView) -> View {
        let param = &view.buses[bus].params;
        let gain = param.clone();
        let muted = param.muted();
        let has_device = view.buses[bus].device.is_some();
        let device = view.buses[bus]
            .device
            .clone()
            .unwrap_or_else(|| "no device".to_string());
        // Who hears this bus, and why: the answer to "so what does A actually do?"
        let (role, hint) = self.bus_role(view, bus);
        let peer = self.bus_capture_peer(view, bus);

        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                badge(bus_name(bus), bus_color(bus)),
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
                    .vertical_alignment(VerticalAlignment::Center)
                    .tooltip(&device),
                mute_chip(param).grid_column(2).height(20.0),
            ));

        // The device name, then what that device means. For a cable, name the endpoint apps actually
        // pick as their microphone: the one piece of information no other part of the UI can give.
        let mut title: Vec<View> = vec![
            header.into(),
            caption(role, self.bus_role_color(view, bus))
                .tooltip(&hint)
                .into(),
        ];
        if let Some(peer) = &peer {
            title.push(
                caption(&format!("\"{peer}\""), AMBER)
                    .tooltip(format!(
                        "Apps that record \"{peer}\" as their microphone hear everything routed here."
                    ))
                    .into(),
            );
        }

        let vfader = fader(200., param.gain_db(), param.level_db(), move |db| {
            gain.set_gain_db(db)
        });

        let gain_db = zeroed(param.gain_db());

        let gain_txt: View = TextBlock::new()
            .text(if gain_db == 0.0 {
                "0.0 dB".to_string()
            } else {
                format!("{gain_db:+.1} dB")
            })
            .font_size(10.0)
            .foreground(Brush::Solid(DIM))
            .horizontal_alignment(HorizontalAlignment::Center)
            .into();

        Border::new()
            .width(138.0)
            .padding(Thickness::uniform(8.0))
            .corner_radius(10.0)
            .vertical_alignment(VerticalAlignment::Stretch)
            .background(Brush::Solid(CARD))
            .border_brush(Brush::Solid(if muted {
                Color::argb(0x7F, RED.r, RED.g, RED.b)
            } else {
                CARD_EDGE
            }))
            .border_thickness(Thickness::uniform(1.0))
            .content(
                Grid::new()
                    .rows([GridLength::STAR, GridLength::Auto, GridLength::Auto])
                    .columns([GridLength::STAR])
                    .children((
                        Border::new()
                            .grid_row(0)
                            .content(StackPanel::new().spacing(1.0).children(title))
                            .vertical_alignment(VerticalAlignment::Top),
                        Border::new().grid_row(1).content(vfader),
                        Border::new().grid_row(2).content(gain_txt),
                    )),
            )
            .grid_column(bus as i32)
            .into()
    }

    /// Amber for the one role that surprises people: a "speaker" that other apps record.
    fn bus_role_color(&self, view: &MixerView, bus: usize) -> Color {
        if view.buses[bus].device.is_none() {
            DIM
        } else if self.bus_capture_peer(view, bus).is_some() {
            AMBER
        } else {
            DIM
        }
    }
}
