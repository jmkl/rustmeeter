use windows_reactor::*;

use crate::component::button::Side::None;

#[derive(Clone, Copy)]
pub enum AcButtonMessage {
    Enter,
    Leave,
    Press,
    Release,
}
#[derive(Clone, Copy)]
pub enum Side {
    Left,
    Right,
    Center,
    None,
}

#[derive(Clone)]
pub struct AcButtonInput {
    pub glyph: String,
    pub side: Side,
    pub bg: Color,
    pub hover_bg: Color,
    pub pressed_bg: Color,
    pub fg: Color,
    pub on_click: Callback<()>,
    pub size: (f64, f64),
    pub font_size: f64,
}

// Callback doesn't implement PartialEq, so compare visual properties only.
impl PartialEq for AcButtonInput {
    fn eq(&self, other: &Self) -> bool {
        self.glyph == other.glyph
            && self.bg == other.bg
            && self.hover_bg == other.hover_bg
            && self.pressed_bg == other.pressed_bg
            && self.fg == other.fg
    }
}

pub struct AcButton {
    hovered: bool,
    pressed: bool,
    on_click: Callback<()>,
}

impl Component for AcButton {
    type Input = AcButtonInput;
    type Message = AcButtonMessage;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            hovered: false,
            pressed: false,
            on_click: input.on_click.clone(),
        }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        self.on_click = input.on_click.clone();
    }

    fn update(&mut self, message: Self::Message, _context: &ComponentContext<Self>) {
        match message {
            AcButtonMessage::Enter => self.hovered = true,
            AcButtonMessage::Leave => {
                self.hovered = false;
                self.pressed = false;
            }
            AcButtonMessage::Press => self.pressed = true,
            AcButtonMessage::Release => {
                if self.pressed && self.hovered {
                    self.on_click.call(());
                }
                self.pressed = false;
            }
        }
    }

    fn view(&self, input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        let bg = if self.pressed {
            input.pressed_bg
        } else if self.hovered {
            input.hover_bg
        } else {
            input.bg
        };
        let on_enter = context.message(AcButtonMessage::Enter);
        let on_leave = context.message(AcButtonMessage::Leave);
        let on_press = context.message(AcButtonMessage::Press);
        let on_release = context.message(AcButtonMessage::Release);
        let radius = match input.side {
            Side::Left => CornerRadius::new(3., 0., 0., 3.),
            Side::Right => CornerRadius::new(0., 3., 3., 0.),
            Side::Center => CornerRadius::new(0., 0., 0., 0.),
            None => CornerRadius::uniform(3.),
        };

        Border::new()
            .width(input.size.0)
            .height(input.size.1)
            .corner_radius(radius)
            .background(Brush::Solid(bg))
            .on_pointer_entered(move |_| on_enter.call(()))
            .on_pointer_exited(move |_| on_leave.call(()))
            .on_pointer_pressed(move |_| on_press.call(()))
            .on_pointer_released(move |_| on_release.call(()))
            .content(
                FontIcon::new()
                    .glyph(&input.glyph)
                    .font_size(input.font_size)
                    .foreground(Brush::Solid(input.fg))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .into()
    }
}
