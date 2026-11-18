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
                self.strip_card(index, strip, view, context),
            ));
        }
        let content: View = if cards.is_empty() {
            card(
                TextBlock::new()
                    .text("No strips yet — add a source above. A strip is one device you capture.")
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
            .columns([GridLength::Auto, GridLength::STAR])
            .column_spacing(8.0)
            .children((
                section_label(&format!("STRIPS  ·  {}", view.strips.len())),
                TextBlock::new()
                    .grid_column(1)
                    .text("each strip is one source · route it to A or B")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .text_alignment(TextAlignment::Right)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        Grid::new()
            .grid_row(2)
            .rows([GridLength::Auto, GridLength::STAR])
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

    fn strip_card(
        &self,
        index: usize,
        s: &StripView,
        view: &MixerView,
        context: &ViewContext<Self>,
    ) -> View {
        let id = s.id;
        let params = &s.params;
        let eq = &s.eq;

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
                    .vertical_alignment(VerticalAlignment::Center)
                    .tooltip(&s.label),
                x_button(context.callback(move |()| Message::RemoveStrip(id))).grid_column(2),
            ));

        let (what, why) = source_caption(s);

        let routes = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
            .column_spacing(4.0)
            .children((
                route_chip(params, 0, view, s),
                in_column(route_chip(params, 1, view, s), 1),
                mute_chip(params).grid_column(3),
            ));

        let mut body: Vec<View> = vec![
            header.into(),
            caption(what, why).tooltip(caption_hint(s)),
            routes.into(),
        ];

        let live = params.clone();
        let bypass = eq.bypass.clone();
        let invalidator = self.eq_invalidator.clone();
        body.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((
                    eq_button(
                        &s.eq,
                        self.eq_open == Some(id),
                        context.callback(move |()| Message::ToggleEq(id)),
                    )
                    .grid_column(0),
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .background(if s.eq.bypass {
                            Brush::Solid(RED)
                        } else {
                            Brush::Solid(CARD)
                        })
                        .height(24.0)
                        .min_width(24.0)
                        .content(
                            FontIcon::new()
                                .glyph("\u{ef90}")
                                .font_size(11.5)
                                .foreground(Brush::Solid(if s.eq.bypass { INK } else { TEXT })),
                        )
                        .on_click(move || {
                            live.update_eq(|e| e.bypass = !bypass);
                            invalidator.invalidate();
                        })
                        .grid_column(1),
                ))
                .into(),
        );

        let gain = params.clone();
        body.push(fader(
            150.,
            params.gain_db(),
            params.level_db(),
            move |db| gain.set_gain_db(db),
        ));

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
fn route_chip(params: &Arc<Params>, bus: usize, view: &MixerView, strip: &StripView) -> View {
    let on = params.routed(bus);
    let target = view.buses[bus].device.as_deref();
    // The engine drops routes that would feed a device back into the same strip. Showing that on the
    // chip is the only way the user finds out: the fader would otherwise just look quiet.
    let feedback = on
        && target.is_some_and(|d| {
            strip
                .fed_by
                .as_deref()
                .is_some_and(|fed| same_device(fed, d))
        });
    // Routed, but the bus points at nothing: amber, because the sound is going nowhere.
    let stranded = on && !feedback && target.is_none();
    let (bg, fg) = if feedback {
        (RED, INK)
    } else if stranded {
        (AMBER, INK)
    } else if on {
        (bus_color(bus), INK)
    } else {
        (FILL_OFF, TEXT)
    };
    let hint = match (on, feedback, target) {
        (_, true, Some(d)) => format!(
            "Loops back into itself: bus {} plays into \"{d}\", which is what this strip listens to. \
             This route is dropped.",
            bus_name(bus)
        ),
        (true, _, None) => format!(
            "Sent to bus {}, but that bus has no device: nothing comes out. Pick one up top.",
            bus_name(bus)
        ),
        (true, _, Some(d)) => format!("Sent to bus {} -> \"{d}\".", bus_name(bus)),
        (false, _, Some(d)) => format!("Click to send to bus {} -> \"{d}\".", bus_name(bus)),
        (false, _, None) => format!(
            "Click to send to bus {}, which has no device.",
            bus_name(bus)
        ),
    };
    let live = params.clone();
    chip(bus_name(bus), bg, fg)
        .width(32.0)
        .on_click(move || live.set_routed(bus, !on))
        .tooltip(hint)
        .into()
}

/// What this strip is listening to, in the fewest words that stay true.
fn source_caption(s: &StripView) -> (&'static str, Color) {
    if s.loopback {
        ("copy of that output", DIM)
    } else if s.fed_by.is_some() {
        ("the cable's far end", AMBER)
    } else {
        ("microphone", DIM)
    }
}

/// The long form of `source_caption`, for a tooltip.
fn caption_hint(s: &StripView) -> String {
    if s.loopback {
        format!(
            "A copy of everything \"{}\" is playing. It is a source, not a microphone: no other app \
             can record it.",
            s.name
        )
    } else if let Some(fed) = &s.fed_by {
        format!(
            "This is the far end of a virtual cable, not a microphone. It carries whatever some bus \
             sends into \"{fed}\", and any app that records this device hears that bus."
        )
    } else {
        "Records this microphone directly.".to_string()
    }
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
