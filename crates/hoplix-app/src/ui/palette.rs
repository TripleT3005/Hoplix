use iced::widget::{
    Column, Space, button, column, container, mouse_area, opaque, row, text, text_input,
};
use iced::{Alignment, Background, Color, Element, Length};

use crate::app::{Hoplix, Message};
use crate::commands::{PALETTE_INPUT, Palette};

// Constants

const PALETTE_WIDTH: f32 = 560.0;
const TOP_OFFSET: f32 = 72.0;

// Palette

pub fn view<'a>(app: &'a Hoplix, palette: &'a Palette) -> Element<'a, Message> {
    let strings = &app.strings;
    let items = app.palette_items();
    let input = text_input(strings.get("command.placeholder"), &palette.query)
        .id(PALETTE_INPUT)
        .on_input(Message::PaletteQuery)
        .on_submit(Message::PaletteSubmitted)
        .padding(10)
        .size(15);
    let list: Element<'a, Message> = if items.is_empty() {
        container(text(strings.get("command.no_match")).size(13))
            .padding([8, 12])
            .into()
    } else {
        Column::with_children(items.into_iter().enumerate().map(|(index, item)| {
            let selected = index == palette.selected;
            let shortcut = item.shortcut.unwrap_or_default();
            button(
                row![
                    text(item.label).size(14).width(Length::Fill),
                    text(shortcut).size(12),
                ]
                .align_y(Alignment::Center)
                .spacing(12),
            )
            .width(Length::Fill)
            .padding([6, 12])
            .style(move |theme: &iced::Theme, status| {
                let palette = theme.extended_palette();
                let highlighted =
                    selected || matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: highlighted
                        .then_some(Background::Color(palette.primary.weak.color)),
                    text_color: if highlighted {
                        palette.primary.weak.text
                    } else {
                        palette.background.base.text
                    },
                    border: iced::Border {
                        radius: 6.0.into(),
                        ..iced::Border::default()
                    },
                    ..button::Style::default()
                }
            })
            .on_press(Message::RunAction(item.action))
            .into()
        }))
        .spacing(2)
        .into()
    };
    let card = container(column![input, list].spacing(8))
        .padding(10)
        .width(PALETTE_WIDTH)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(Background::Color(palette.background.base.color)),
                border: iced::Border {
                    color: palette.background.strong.color,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                shadow: iced::Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                    offset: iced::Vector::new(0.0, 8.0),
                    blur_radius: 24.0,
                },
                ..container::Style::default()
            }
        });
    let layout = column![Space::new().height(TOP_OFFSET), opaque(card)]
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center);
    opaque(
        mouse_area(container(layout).style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.3))),
            ..container::Style::default()
        }))
        .on_press(Message::PaletteClosed),
    )
}
