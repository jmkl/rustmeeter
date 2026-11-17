//! Live caption transcript panel.

use windows_reactor::*;

use super::MixerApp;
use crate::theme::*;

impl MixerApp {
    pub(crate) fn transcript(&self) -> View {
        let lines: Vec<KeyedView> = self
            .transcript
            .iter()
            .map(|(key, line)| {
                keyed(
                    *key,
                    TextBlock::new()
                        .text(line)
                        .font_size(11.5)
                        .text_wrapping(TextWrapping::NoWrap)
                        .is_text_selection_enabled(true),
                )
            })
            .collect();

        let content: View = if lines.is_empty() {
            TextBlock::new()
                .text("Captions appear here once you start them.")
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .into()
        } else {
            StackPanel::new().spacing(1.0).keyed_children(lines).into()
        };

        Grid::new()
            .grid_row(3)
            .rows([GridLength::Auto, GridLength::STAR])
            .row_spacing(6.0)
            .children((
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        section_label("TRANSCRIPT"),
                        TextBlock::new()
                            .grid_column(1)
                            .text(self.caps.log_path().display().to_string())
                            .font_size(9.5)
                            .foreground(Brush::Solid(DIM)),
                    )),
                card(
                    ScrollViewer::new()
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(content),
                )
                .grid_row(1),
            ))
            .into()
    }
}
