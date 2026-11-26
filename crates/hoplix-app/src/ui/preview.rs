use iced::widget::{
    Column, Space, button, column, container, mouse_area, row, rule, scrollable, text,
};
use iced::{Alignment, Element, Font, Length, mouse};

use crate::app::{Hoplix, Message, Tab};
use crate::preview::{
    Content, Encoding, Fit, PREVIEW_BYTES, PreviewMessage, TextPreview, limit_bytes,
};
use crate::{format, local_time, theme, ui};

// Constants

pub const DIVIDER_WIDTH: f32 = 6.0;
const TEXT_SIZE: u32 = 12;
const INFO_SIZE: u32 = 12;
pub const TEXT_SCROLL_ID: &str = "preview-text";

// Panel

pub fn fit(app: &Hoplix, tab: &Tab, available: f32, list_min: f32) -> Option<Fit> {
    let preview = &app.preview;
    if !preview.open || app.focused_tab().map(|focused| focused.id) != Some(tab.id) {
        return None;
    }
    Some(Fit::new(preview.width, available - DIVIDER_WIDTH, list_min))
}

pub fn panel(app: &Hoplix, fit: Fit) -> Element<'_, Message> {
    let divider = mouse_area(
        container(rule::vertical(1))
            .width(DIVIDER_WIDTH)
            .height(Length::Fill)
            .center_x(DIVIDER_WIDTH),
    )
    .interaction(mouse::Interaction::ResizingHorizontally)
    .on_press(Message::Preview(PreviewMessage::ResizeStarted(fit)));
    let body = column![header(app), body(app)].spacing(8);
    row![
        divider,
        container(body)
            .padding([4, 6])
            .width(fit.width)
            .height(Length::Fill)
    ]
    .height(Length::Fill)
    .into()
}

fn header(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let mut header = Column::new().spacing(2);
    header = header.push(
        row![
            text(strings.get("preview.title")).size(15),
            Space::new().width(Length::Fill),
            button(text("×").size(13))
                .padding([0, 6])
                .style(button::text)
                .on_press(Message::Preview(PreviewMessage::Toggle)),
        ]
        .align_y(Alignment::Center),
    );
    let Some(subject) = &app.preview.subject else {
        return header.into();
    };
    let entry = &subject.entry;
    header = header.push(
        container(
            text(entry.name.as_str())
                .size(14)
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .width(Length::Fill),
    );
    let separator = strings.get("number.decimal_separator");
    let mut details = vec![ui::browser::kind_label(strings, entry)];
    if let Some(size) = entry.size.filter(|_| !entry.is_directory()) {
        details.push(format::size(size, separator));
    }
    if let Some(modified) = entry.modified.and_then(local_time::local) {
        details.push(local_time::format(&modified, strings.get("date.format")));
    }
    header
        .push(text(details.join(" · ")).size(INFO_SIZE))
        .push(rule::horizontal(1))
        .into()
}

fn body(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let message = |key: &str| -> Element<'_, Message> {
        container(text(strings.get(key).to_string()).size(13))
            .center_x(Length::Fill)
            .padding([16, 0])
            .into()
    };
    match &app.preview.content {
        Content::Nothing => message("preview.nothing"),
        Content::Several(count) => container(
            text(strings.format("preview.several", &[("count", &count.to_string())])).size(13),
        )
        .center_x(Length::Fill)
        .padding([16, 0])
        .into(),
        Content::Folder => message("preview.folder"),
        Content::Waiting | Content::Loading => message("preview.loading"),
        Content::NoPreview => message("preview.none"),
        Content::Failed(error) => ui::notice(ui::error_text(strings, error)),
        Content::Confirm => {
            let limit = format::size(
                limit_bytes(app.settings.preview_limit),
                strings.get("number.decimal_separator"),
            );
            column![
                text(strings.format("preview.large", &[("limit", &limit)])).size(13),
                button(text(strings.get("preview.load")).size(13))
                    .style(theme::secondary_button)
                    .on_press(Message::Preview(PreviewMessage::LoadRequested)),
            ]
            .spacing(10)
            .padding([16, 0])
            .align_x(Alignment::Center)
            .width(Length::Fill)
            .into()
        }
        Content::Text(preview) => text_view(app, preview),
    }
}

fn text_view<'a>(app: &'a Hoplix, preview: &'a TextPreview) -> Element<'a, Message> {
    let strings = &app.strings;
    let separator = strings.get("number.decimal_separator");
    let lines = if preview.lines == 1 {
        strings.get("preview.lines.one").to_string()
    } else {
        strings.format(
            "preview.lines.other",
            &[("count", &preview.lines.to_string())],
        )
    };
    let encoding = match preview.encoding {
        Encoding::Utf8 => "UTF-8",
        Encoding::Utf16Le => "UTF-16 LE",
        Encoding::Utf16Be => "UTF-16 BE",
        Encoding::Windows1252 => "Windows-1252",
    };
    let mut info = column![text(format!("{lines} · {encoding}")).size(INFO_SIZE)].spacing(2);
    if preview.truncated {
        info = info.push(
            text(strings.format(
                "preview.truncated",
                &[("size", &format::size(PREVIEW_BYTES as u64, separator))],
            ))
            .size(INFO_SIZE)
            .style(text::warning),
        );
    }
    let content = text(preview.text.as_str())
        .font(Font::MONOSPACE)
        .size(TEXT_SIZE)
        .wrapping(text::Wrapping::None);
    column![
        info,
        scrollable(container(content).padding(iced::Padding::ZERO.right(12).bottom(12)))
            .id(TEXT_SCROLL_ID)
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(6)
    .into()
}
