//! Strip rack: the horizontally scrolling row of channel cards.

use std::sync::Arc;

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use super::eq::eq_button;
use crate::fader::fader;
use crate::messages::Message;
use crate::theme::*;
use crate::util::*;

impl MixerApp {
    pub(crate) fn strips(&self, view: &MixerView, context: &ViewContext<Self>) -> View {
        let mut cards: Vec<KeyedView> = Vec::new();
        for (index, strip) in view.strips.iter().enumerate() {
            cards.push(keyed(
                u64::from(strip.id),
                self.strip_card(index, strip, context),
            ));
        }
        let content: View = if cards.is_empty() {
            card(
                TextBlock::new()
                    .text("No strips yet — pick a device under ADD.")
                    .font_size(10.5)
                    .foreground(Brush::Solid(DIM)),
            )
            .into()
        } else {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .keyed_children(cards)
                .into()
        };
        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                section_label(&format!("STRIPS  ·  {}", view.strips.len())),
                TextBlock::new()
                    .grid_column(2)
                    .text("mic → B · loopback → A · captions read the raw strip")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        Grid::new()
            .grid_row(2)
            .grid_column_span(2)
            .rows([GridLength::Auto, GridLength::Auto])
            .row_spacing(6.0)
            .children((
                header,
                rack(
                    ScrollViewer::new()
                        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(content),
                )
                .grid_row(1),
            ))
            .into()
    }

    fn strip_card(&self, index: usize, s: &StripView, context: &ViewContext<Self>) -> View {
        let id = s.id;
        let params = &s.params;

        let header = Grid::new()
            .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                badge(&(index + 1).to_string(), GREEN),
                TextBlock::new()
                    .grid_column(1)
                    .text(&s.label)
                    .font_size(10.5)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
                x_button(context.callback(move |()| Message::RemoveStrip(id))).grid_column(2),
            ));

        let routes = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
            .column_spacing(4.0)
            .children((
                route_chip(params, 0),
                route_chip(params, 1).grid_column(1),
                mute_chip(params).grid_column(3),
            ));

        let mut body: Vec<View> = vec![header.into(), routes.into()];

        body.push(
            eq_button(
                &s.eq,
                self.eq_open == Some(id),
                context.callback(move |()| Message::ToggleEq(id)),
            )
            .into(),
        );

        let gain = params.clone();
        body.push(fader(params.gain_db(), params.level_db(), move |db| {
            gain.set_gain_db(db)
        }));

        let gain_db = zeroed(params.gain_db());
        body.push(
            TextBlock::new()
                .text(if gain_db == 0.0 {
                    "0.0 dB".to_string()
                } else {
                    format!("{gain_db:+.1} dB")
                })
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
        );

        let muted = params.muted();
        Border::new()
            .width(168.0)
            .padding(Thickness::uniform(8.0))
            .corner_radius(10.0)
            .vertical_alignment(VerticalAlignment::Top)
            .background(Brush::Solid(CARD))
            .border_brush(Brush::Solid(if muted {
                Color::argb(0x7F, RED.r, RED.g, RED.b)
            } else {
                CARD_EDGE
            }))
            .border_thickness(Thickness::uniform(1.0))
            .content(StackPanel::new().spacing(5.0).children(body))
            .into()
    }
}

/// Colored A/B routing toggle, read from the live atomics and written straight back.
fn route_chip(params: &Arc<Params>, bus: usize) -> Button {
    let on = params.routed(bus);
    let (bg, fg) = if on {
        (if bus == 0 { GREEN } else { BLUE }, INK)
    } else {
        (FILL_OFF, TEXT)
    };
    let live = params.clone();
    chip(bus_name(bus), bg, fg)
        .width(32.0)
        .on_click(move || live.set_routed(bus, !on))
}

pub(crate) fn mute_chip(params: &Arc<Params>) -> Button {
    let muted = params.muted();
    let live = params.clone();
    chip(
        "M",
        if muted { RED } else { FILL_OFF },
        if muted { INK } else { TEXT },
    )
    .width(24.0)
    .on_click(move || live.set_muted(!muted))
}
