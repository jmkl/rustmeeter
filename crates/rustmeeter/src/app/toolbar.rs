//! Toolbar: transport, caption controls, bus device pickers and the "add input" combo.

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use crate::messages::Message;
use crate::theme::*;

impl MixerApp {
    pub(crate) fn toolbar(&self, view: &MixerView, context: &ViewContext<Self>) -> View {
        let transport = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children((
                if view.running {
                    action_button(
                        "Stop",
                        RED_STRONG,
                        WHITE,
                        context.callback(|()| Message::ToggleRun),
                    )
                } else {
                    action_button(
                        "Start",
                        GREEN,
                        INK,
                        context.callback(|()| Message::ToggleRun),
                    )
                },
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .height(30.0)
                    .content(label_text("Refresh", 11.0, DIM))
                    .on_click(context.callback(|()| Message::Refresh)),
            ));

        let mut captions: Vec<View> = vec![section_label("CAPTIONS").into()];
        match self.caps.active_strip() {
            Some(n) => captions.push(
                action_button(
                    &format!("Stop · strip {n}"),
                    RED,
                    INK,
                    context.callback(|()| Message::CaptionsOff),
                )
                .into(),
            ),
            None => {
                let mut items: Vec<String> = vec!["first mic".to_string()];
                for i in 1..=view.strips.len() {
                    items.push(format!("strip {i}"));
                }
                captions.push(
                    ComboBox::new()
                        .width(104.0)
                        .items_source(items)
                        .selected_index(Some(self.caption_strip.unwrap_or(0)))
                        .on_selection_changed(
                            context.callback(|index: Option<usize>| Message::CaptionStrip(index)),
                        )
                        .into(),
                );
                captions.push(
                    action_button(
                        "Start captions",
                        BLUE,
                        INK,
                        context.callback(|()| Message::CaptionsOn),
                    )
                    .into(),
                );
            }
        }

        let top = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                transport,
                StackPanel::new()
                    .grid_column(1)
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children(captions),
            ));

        let devices = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Pixel(196.0),
                GridLength::Auto,
                GridLength::Pixel(196.0),
                GridLength::Auto,
                GridLength::Pixel(176.0),
                GridLength::STAR,
            ])
            .column_spacing(6.0)
            .children((
                badge("A", GREEN),
                self.bus_combo(0, view, context).grid_column(1),
                badge("B", BLUE).grid_column(2),
                self.bus_combo(1, view, context).grid_column(3),
                section_label("ADD").grid_column(4),
                self.add_combo(context).grid_column(5),
                TextBlock::new()
                    .grid_column(6)
                    .text("A: what you hear · B: recorded by apps")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .text_alignment(TextAlignment::Right)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        card(StackPanel::new().spacing(6.0).children((top, devices)))
            .grid_row(0)
            .grid_column_span(2)
            .into()
    }

    fn bus_combo(&self, bus: usize, view: &MixerView, context: &ViewContext<Self>) -> ComboBox {
        let items: Vec<String> = std::iter::once("None".to_string())
            .chain(self.outputs.iter().cloned())
            .collect();
        ComboBox::new()
            .width(196.0)
            .items_source(items)
            .selected_index(self.bus_selection(view, bus))
            .on_selection_changed(
                context.callback(move |index: Option<usize>| Message::SetBus(bus, index)),
            )
    }

    fn add_combo(&self, context: &ViewContext<Self>) -> ComboBox {
        ComboBox::new()
            .width(176.0)
            .placeholder_text("add input…")
            .items_source(self.inputs.iter().map(|s| s.label()))
            .selected_index(self.add_index)
            .on_selection_changed(context.callback(|index: Option<usize>| Message::AddInput(index)))
    }
}
