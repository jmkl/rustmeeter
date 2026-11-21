//! Parametric EQ editor: the full-window modal, its pointer interactions and the shared graph
//! geometry.
//!
//! `draw` renders the graph with Direct2D; `geometry` maps frequency/gain to pixels; `table` owns
//! the per-band rows. This module holds the editor view and the state changes behind it.

mod draw;
mod geometry;
mod table;

use rustmeeter_core::*;
use windows_canvas::{ColorF, canvas_invalidated};
use windows_reactor::*;

use self::draw::draw_eq_curve;
use self::geometry::{freq_to_x, gain_to_y, x_to_freq, y_to_gain};
use self::table::{eq_columns, eq_head, eq_row};
use super::MixerApp;
use crate::messages::Message;
use crate::theme::*;

// Parametric EQ editor geometry. The plot spans 20 Hz..20 kHz (3 decades, log) and +-15 dB.
// `EQ_W` must fit the smallest window (760 px) once the content padding is subtracted.
const EQ_W: f64 = 660.0;
const EQ_H: f64 = 240.0;
/// Padding of the modal card; `app` uses it so the table can subtract the same value.
pub(super) const MODAL_PAD: f64 = 14.0;
/// Room reserved on the right of the band table for its vertical scrollbar.
const SCROLLBAR_W: f64 = 18.0;
/// Response curve resolution: `EQ_COLS + 1` samples across `EQ_W` (lower = cheaper to redraw).
const EQ_COLS: usize = 160;
/// Sample rate used only to draw the curve.
const EQ_FS: f32 = 48_000.0;
/// Solid background of the EQ graph. The canvas surface is opaque, so it paints the well itself.
const EQ_WELL: ColorF = ColorF::from_rgba8(0x0A, 0x0D, 0x14, 0xFF);
const DOT: f64 = 16.0;
/// A press this close (px) to an existing dot never adds a new band.
const DOT_HIT: f64 = 14.0;
const BAND_COLORS: [Color; 10] = [
    Color::rgb(0xF8, 0x71, 0x71),
    Color::rgb(0xFB, 0x92, 0x3C),
    Color::rgb(0xFB, 0xBF, 0x24),
    Color::rgb(0xA3, 0xE6, 0x35),
    Color::rgb(0x34, 0xD3, 0x99),
    Color::rgb(0x22, 0xD3, 0xEE),
    Color::rgb(0x60, 0xA5, 0xFA),
    Color::rgb(0x81, 0x8C, 0xF8),
    Color::rgb(0xC0, 0x84, 0xFC),
    Color::rgb(0xF4, 0x72, 0xB6),
];

/// A dot being dragged: where the pointer was (window DIPs) and the band's values at that moment.
#[derive(Clone, Copy)]
pub(crate) struct EqDrag {
    strip: StripId,
    band: usize,
    wx: f64,
    wy: f64,
    freq: f32,
    gain: f32,
}

impl MixerApp {
    /// Dot pressed: right button removes the band, left button starts a drag.
    pub(crate) fn eq_press(&mut self, strip: StripId, band: usize, wx: f64, wy: f64, right: bool) {
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let Some(b) = params.eq().bands.get(band).copied() else {
            return;
        };
        if right {
            params.update_eq(|s| {
                if band < s.bands.len() {
                    s.bands.remove(band);
                }
            });
            self.eq_drag = None;
        } else {
            self.eq_drag = Some(EqDrag {
                strip,
                band,
                wx,
                wy,
                freq: b.freq_hz,
                gain: b.gain_db,
            });
        }
    }

    /// Pointer moved while a dot is captured. Uses window coordinates (the dot itself moves
    /// under the pointer, so element-local coordinates would drift).
    pub(crate) fn eq_move(&mut self, wx: f64, wy: f64, left: bool) {
        let Some(d) = self.eq_drag else {
            return;
        };
        if !left {
            self.eq_drag = None;
            return;
        }
        let Some(params) = self.strip_params(d.strip) else {
            return;
        };
        let freq = (f64::from(d.freq) * 10f64.powf((wx - d.wx) / EQ_W * 3.0)) as f32;
        let gain = d.gain - ((wy - d.wy) / (EQ_H / 2.0) * f64::from(EQ_GAIN_MAX)) as f32;
        params.update_eq(|s| {
            if let Some(b) = s.bands.get_mut(d.band) {
                b.freq_hz = freq;
                if b.kind.uses_gain() {
                    b.gain_db = gain;
                }
            }
        });
    }

    /// Left press on empty graph space adds a Bell band there. A press on a dot also bubbles up
    /// to the graph, so anything near an existing dot is ignored.
    pub(crate) fn eq_add_at(&mut self, strip: StripId, x: f64, y: f64, left: bool) {
        if !left || x < 0.0 || y < 0.0 || x > EQ_W || y > EQ_H {
            return;
        }
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let near_dot = params.eq().bands.iter().any(|b| {
            let g = if b.kind.uses_gain() { b.gain_db } else { 0.0 };
            let (dx, dy) = (freq_to_x(b.freq_hz) - x, gain_to_y(g) - y);
            dx * dx + dy * dy < DOT_HIT * DOT_HIT
        });
        if near_dot {
            return;
        }
        let band = EqBand::new(EqKind::Bell, x_to_freq(x), y_to_gain(y));
        params.update_eq(|s| {
            s.add(band);
        });
    }

    /// "+ Add band" button: a flat Bell at the first standard frequency that is still free.
    pub(crate) fn eq_add_default(&mut self, strip: StripId) {
        let Some(params) = self.strip_params(strip) else {
            return;
        };
        let used: Vec<f32> = params.eq().bands.iter().map(|b| b.freq_hz).collect();
        let freq = [
            1000.0_f32, 250.0, 4000.0, 100.0, 8000.0, 500.0, 2000.0, 60.0, 12000.0, 16000.0,
        ]
        .into_iter()
        .find(|f| used.iter().all(|u| (u / f).log10().abs() > 0.1))
        .unwrap_or(1000.0);
        params.update_eq(|s| {
            s.add(EqBand::new(EqKind::Bell, freq, 0.0));
        });
    }

    /// Width of the band table: the modal fills the window, minus its padding and the scrollbar.
    fn eq_table_width(&self) -> f64 {
        (self.window_w - 2.0 * MODAL_PAD - SCROLLBAR_W).max(480.0)
    }

    /// Parametric EQ editor for one strip: response graph with draggable dots, plus a band table.
    pub(crate) fn eq_card(&self, index: usize, s: &StripView, context: &ViewContext<Self>) -> View {
        let id = s.id;
        let params = &s.params;
        let eq = &s.eq;
        let bypass = eq.bypass;
        let table_w = self.eq_table_width();

        // ---- header: title, bypass, close
        let header = {
            let live = params.clone();
            Grid::new()
                .columns([
                    GridLength::Auto,
                    GridLength::STAR,
                    GridLength::Auto,
                    GridLength::Auto,
                ])
                .column_spacing(6.0)
                .children((
                    badge(&(index + 1).to_string(), GREEN),
                    TextBlock::new()
                        .grid_column(1)
                        .text(format!("EQ · {}", s.label))
                        .font_size(10.5)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .vertical_alignment(VerticalAlignment::Center),
                    chip(
                        "Bypass",
                        if bypass { AMBER } else { FILL_OFF },
                        if bypass { INK } else { TEXT },
                    )
                    .grid_column(2)
                    .width(54.0)
                    .on_click({
                        let invalidator = self.eq_invalidator.clone();
                        move || {
                            live.update_eq(|e| e.bypass = !bypass);
                            invalidator.invalidate();
                        }
                    }),
                    x_button(context.callback(move |()| Message::ToggleEq(id))).grid_column(3),
                ))
        };

        // ---- plot: one Direct2D canvas (well, grid, 0 dB line, axis labels and the response
        // curve), with the bands' dots as XAML on top. The canvas surface is opaque, so it paints
        // the background and grid itself; XAML siblings underneath a swap-chain panel do not show.
        // A Canvas reconciles its children positionally, so the dots are appended after the canvas
        // and keep a stable order while bands are added or removed.
        let mut plot: Vec<View> = Vec::new();

        // The response is drawn with Direct2D rather than a column of `Border`s: a polyline is
        // anti-aliased, where axis-aligned rectangles can only approximate a slope as a staircase.
        // It sits below the dots, so the dots stay on top and keep their handlers.
        {
            let curve = eq.clone();
            let invalidator = self.eq_invalidator.clone();
            plot.push(
                Border::new()
                    .width(EQ_W)
                    .height(EQ_H)
                    .canvas_left(0.0)
                    .canvas_top(0.0)
                    .content(canvas_invalidated(&invalidator, move |ctx| {
                        draw_eq_curve(ctx, &curve)
                    }))
                    .into(),
            );
        }

        for (i, b) in eq.bands.iter().enumerate() {
            let color = BAND_COLORS[i % BAND_COLORS.len()];
            let live = b.enabled && !bypass;
            let fill = if live {
                color
            } else {
                Color::argb(0x66, color.r, color.g, color.b)
            };
            let dragging = self.eq_drag.is_some_and(|d| d.strip == id && d.band == i);
            let x = freq_to_x(b.freq_hz);
            let y = gain_to_y(if b.kind.uses_gain() { b.gain_db } else { 0.0 });
            plot.push(
                Border::new()
                    .width(DOT)
                    .height(DOT)
                    .corner_radius(DOT / 2.0)
                    .background(Brush::Solid(fill))
                    .border_brush(Brush::Solid(if dragging {
                        WHITE
                    } else {
                        Color::argb(0x99, 0x0A, 0x12, 0x1A)
                    }))
                    .border_thickness(Thickness::uniform(if dragging { 2.0 } else { 1.0 }))
                    .canvas_left(x - DOT / 2.0)
                    .canvas_top(y - DOT / 2.0)
                    .capture_pointer_on_press(true)
                    .on_pointer_pressed(context.callback(move |p: PointerEventInfo| {
                        Message::EqPress {
                            strip: id,
                            band: i,
                            wx: p.window_x,
                            wy: p.window_y,
                            right: p.is_right_button_pressed,
                        }
                    }))
                    .on_pointer_moved(context.callback(|p: PointerEventInfo| Message::EqMove {
                        wx: p.window_x,
                        wy: p.window_y,
                        left: p.is_left_button_pressed,
                    }))
                    .on_pointer_released(context.callback(|_: PointerEventInfo| Message::EqRelease))
                    .on_pointer_capture_lost(context.callback(|()| Message::EqRelease))
                    .content(
                        TextBlock::new()
                            .text((i + 1).to_string())
                            .font_size(8.5)
                            .font_weight(FontWeight::BOLD)
                            .foreground(Brush::Solid(INK))
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .vertical_alignment(VerticalAlignment::Center),
                    )
                    .into(),
            );
        }

        let graph = Border::new()
            .width(EQ_W)
            .height(EQ_H)
            .corner_radius(6.0)
            .background(Brush::Solid(WELL))
            .on_pointer_pressed(
                context.callback(move |p: PointerEventInfo| Message::EqAddAt {
                    strip: id,
                    x: p.x,
                    y: p.y,
                    left: p.is_left_button_pressed,
                }),
            )
            .content(Canvas::new().width(EQ_W).height(EQ_H).children(plot));

        // ---- table: one row per band
        let table_header = Grid::new()
            .columns(eq_columns(table_w))
            .column_spacing(4.0)
            .children((
                eq_head("#", 0),
                eq_head("Curve", 1),
                eq_head("Gain", 2),
                eq_head("Q", 3),
                eq_head("Frequency", 4),
            ));
        let rows: Vec<KeyedView> = eq
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| keyed(i as u64, eq_row(params, i, b, table_w)))
            .collect();
        let table_body: View = if rows.is_empty() {
            TextBlock::new()
                .text("No bands yet: click the graph or press + Add band.")
                .font_size(10.0)
                .foreground(Brush::Solid(DIM))
                .into()
        } else {
            StackPanel::new().spacing(3.0).keyed_children(rows).into()
        };
        let table = ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(table_body);

        // ---- footer
        let reset = params.clone();
        let footer = Grid::new()
            .columns([GridLength::Auto, GridLength::Auto, GridLength::STAR])
            .column_spacing(6.0)
            .children((
                action_button(
                    "\u{e710}",
                    BLUE,
                    INK,
                    crate::component::button::Side::None,
                    context.callback(move |()| Message::EqAdd(id)),
                ),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .grid_column(1)
                    .height(30.0)
                    .content(label_text("Reset", 11.0, DIM))
                    .on_click(move || reset.set_eq(EqSettings::default())),
                TextBlock::new()
                    .grid_column(2)
                    .text("click graph: add · drag dot: freq / gain · right-click dot: remove")
                    .font_size(9.0)
                    .foreground(Brush::Solid(DIM))
                    .text_alignment(TextAlignment::Right)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        // Fills the modal: header, graph, the band table taking the remaining height, then footer.
        Grid::new()
            .rows([
                GridLength::Auto, // header
                GridLength::Auto, // graph
                GridLength::STAR, // band table
                GridLength::Auto, // footer
            ])
            .row_spacing(10.0)
            .children((
                header,
                graph
                    .grid_row(1)
                    .horizontal_alignment(HorizontalAlignment::Center),
                Grid::new()
                    .grid_row(2)
                    .rows([GridLength::Auto, GridLength::STAR])
                    .row_spacing(4.0)
                    .children((table_header, table.grid_row(1))),
                footer.grid_row(3),
            ))
            .into()
    }
}

/// The strip card's EQ entry: one button that opens/closes the editor.
pub(crate) fn eq_button(eq: &EqSettings, open: bool, click: impl IntoUnitCallback) -> Button {
    let active = eq.active_bands();
    let label = if eq.bypass {
        "EQ · bypassed".to_string()
    } else if active == 0 {
        "EQ".to_string()
    } else {
        format!("EQ · {active} band{}", if active == 1 { "" } else { "s" })
    };
    let lit = !eq.bypass && active > 0;
    Button::new()
        .style(ButtonStyle::Subtle)
        .background(Brush::Solid(if open { BLUE } else { FILL_OFF }))
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .height(24.0)
        .content(
            TextBlock::new()
                .text(format!("{} {label}", if open { "▾" } else { "▸" }))
                .font_size(10.5)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(Brush::Solid(if open {
                    INK
                } else if lit {
                    BLUE
                } else {
                    TEXT
                })),
        )
        .on_click(click)
}
