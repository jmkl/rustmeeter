//! Toolbar: the two bus destinations and the "add source" combo.
//!
//! A bus is a *destination*, so each picker reads `[A] -> [device]`. The strip rack points the other
//! way: `[source] -> [A]`. Keeping both arrows on screen is what stops the two from being read as the
//! same kind of thing.

use rustmeeter_core::*;
use windows_reactor::*;

use super::MixerApp;
use crate::messages::Message;
use crate::theme::*;

impl MixerApp {
    pub(crate) fn toolbar(&self, view: &MixerView, context: &ViewContext<Self>) -> View {
        let devices = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Pixel(176.0),
                GridLength::STAR, // spacer: keeps the row left-aligned
            ])
            .column_spacing(6.0)
            .children((
                self.bus_pick(0, view, context),
                in_column(self.bus_pick(1, view, context), 1),
                section_label("ADD SOURCE").grid_column(2),
                in_column(self.add_combo(context), 3),
            ));

        card(devices).grid_row(0).grid_column_span(2).into()
    }

    /// `[A] -> [device]`: everything routed to this bus is played into that device.
    fn bus_pick(&self, bus: usize, view: &MixerView, context: &ViewContext<Self>) -> View {
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(5.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                badge(bus_name(bus), bus_color(bus)),
                arrow(),
                self.bus_combo(bus, view, context),
            ))
            .into()
    }

    fn bus_combo(&self, bus: usize, view: &MixerView, context: &ViewContext<Self>) -> View {
        let items: Vec<String> = std::iter::once("None".to_string())
            .chain(self.outputs.iter().cloned())
            .collect();
        let (_, hint) = self.bus_role(view, bus);
        ComboBox::new()
            .width(196.0)
            .items_source(items)
            .selected_index(self.bus_selection(view, bus))
            .on_selection_changed(
                context.callback(move |index: Option<usize>| Message::SetBus(bus, index)),
            )
            .tooltip(hint)
            .into()
    }

    fn add_combo(&self, context: &ViewContext<Self>) -> View {
        ComboBox::new()
            .width(176.0)
            .placeholder_text("add a source…")
            .items_source(self.inputs.iter().map(|s| s.label()))
            .selected_index(self.add_index)
            .on_selection_changed(context.callback(|index: Option<usize>| Message::AddInput(index)))
            .tooltip(
                "A microphone, or an output device to tap: \"[Loopback] X\" is a copy of what X is \
                 playing, not a microphone.",
            )
            .into()
    }
}
