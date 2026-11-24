use iced::widget::{Column, Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::edits::{EditMessage, EditState, EditedFile};
use crate::i18n::Strings;
use crate::theme;

// Constants

const LIST_HEIGHT: f32 = 150.0;
const NAME_WIDTH: f32 = 260.0;

// Panel

pub fn view(app: &Hoplix) -> Option<Element<'_, Message>> {
    let files = &app.edits.files;
    if files.is_empty() {
        return None;
    }
    let strings = &app.strings;
    let title = text(strings.format("edit.title", &[("count", &files.len().to_string())])).size(14);
    let rows = files.iter().map(|file| file_row(strings, file));
    let list = container(
        scrollable(
            Column::with_children(rows)
                .spacing(2)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Shrink),
    )
    .max_height(LIST_HEIGHT);
    Some(
        container(column![title, list].spacing(4))
            .padding([4, 12])
            .width(Length::Fill)
            .style(theme::bar)
            .into(),
    )
}

fn file_row<'a>(strings: &'a Strings, file: &'a EditedFile) -> Element<'a, Message> {
    let id = file.id;
    let status = match file.state {
        EditState::Downloading => strings.get("edit.status.downloading").to_string(),
        EditState::Watching => strings.get("edit.status.watching").to_string(),
        EditState::Uploading => strings.get("edit.status.uploading").to_string(),
        EditState::Changed => strings.format("edit.changed", &[("name", &file.name)]),
    };
    let mut line = row![
        container(text(&file.name).size(13).wrapping(text::Wrapping::None))
            .width(NAME_WIDTH)
            .clip(true),
        text(status).size(13),
        Space::new().width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if file.state == EditState::Changed {
        line = line
            .push(
                button(text(strings.get("edit.upload")).size(13))
                    .style(button::primary)
                    .on_press(Message::Edit(EditMessage::Upload(id))),
            )
            .push(
                button(text(strings.get("edit.ignore")).size(13))
                    .style(theme::secondary_button)
                    .on_press(Message::Edit(EditMessage::Ignore(id))),
            );
    }
    line = line.push(
        button(text(strings.get("edit.stop")).size(13))
            .style(button::text)
            .on_press(Message::Edit(EditMessage::Stop(id))),
    );
    let content = container(line).padding([2, 6]).width(Length::Fill);
    if file.state == EditState::Changed {
        content.style(theme::warning).into()
    } else {
        content.into()
    }
}
