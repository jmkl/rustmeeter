//! Live caption panel: the one line being spoken right now.

use windows_reactor::*;

use super::MixerApp;
use crate::theme::*;

impl MixerApp {
    pub(crate) fn transcript(&self) -> View {
        let caption_text = self.shared.borrow().latest_caption.text.clone();
        let latest = caption_text.trim();
        let empty = latest.is_empty();

        // One caption, as large as the panel allows. Past lines are not shown here: they are in
        // `captions.log`, and a wall of scrolling text next to the mixer only made the current line
        // harder to find.
        let caption = TextBlock::new()
            .text(if empty {
                "Captions appear here once you start them."
            } else {
                latest
            })
            .font_size(if empty { 12.0 } else { 24.0 })
            .font_weight(if empty {
                FontWeight::NORMAL
            } else {
                FontWeight::BOLD
            })
            .text_alignment(TextAlignment::Center)
            .text_wrapping(TextWrapping::Wrap)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .foreground(Brush::Solid(if empty { DIM } else { TEXT }));

        Grid::new()
            .grid_row(3)
            .grid_column_span(2)
            .rows([
                GridLength::Auto, // title
                GridLength::STAR, // caption card, as tall as the panel allows
            ])
            .row_spacing(6.0)
            .children((section_label("TRANSCRIPT"), card(caption).grid_row(1)))
            .into()
    }
}
