use hoplix_core::{
    ConflictPolicy, HistoryEntry, HistoryResult, JobSnapshot, JobStatus, Location,
    TransferDirection,
};
use iced::widget::{
    Column, Space, button, column, container, pick_list, progress_bar, row, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::i18n::Strings;
use crate::queue_store::{HistoryMessage, PanelTab, checksum_text, result_text, time_text};
use crate::transfers::{Direction, TransferAction};
use crate::ui::{self, Choice};
use crate::{format, theme};

// Constants

const LIST_HEIGHT: f32 = 220.0;
const DETAILS_WIDTH: f32 = 300.0;
const POLICY_WIDTH: f32 = 190.0;
const EXPORT_WIDTH: f32 = 260.0;
const TIME_WIDTH: f32 = 130.0;

// Panel

pub fn view(app: &Hoplix) -> Option<Element<'_, Message>> {
    if app.transfer_jobs.is_empty() && app.history.history.is_empty() {
        return None;
    }
    let strings = &app.strings;
    let toggle = button(
        text(if app.transfer_panel_open {
            format!("▾ {}", strings.get("transfer.title"))
        } else {
            format!("▸ {}", strings.get("transfer.title"))
        })
        .size(14),
    )
    .style(button::text)
    .on_press(Message::Transfer(TransferAction::TogglePanel));
    let mut header = row![toggle, text(summary(app)).size(13)]
        .spacing(8)
        .align_y(Alignment::Center);
    if app.transfer_panel_open {
        header = header.push(tab_button(
            strings.get("transfer.tab.queue").to_string(),
            PanelTab::Queue,
            app.history.tab,
        ));
        header = header.push(tab_button(
            strings.format(
                "transfer.tab.history",
                &[("count", &app.history.history.len().to_string())],
            ),
            PanelTab::History,
            app.history.tab,
        ));
    }
    header = header.push(Space::new().width(Length::Fill));
    if app.transfer_panel_open && app.history.tab == PanelTab::History {
        return Some(panel(column![header, history_view(app)].spacing(4)));
    }
    if app.transfer_panel_open {
        header = header
            .push(text(strings.get("transfer.policy")).size(13))
            .push(policy_picker(app))
            .push(action_button(
                strings.get("transfer.pause_all"),
                TransferAction::PauseAll,
            ))
            .push(action_button(
                strings.get("transfer.resume_all"),
                TransferAction::ResumeAll,
            ))
            .push(action_button(
                strings.get("transfer.cancel_all"),
                TransferAction::CancelAll,
            ))
            .push(action_button(
                strings.get("transfer.clear"),
                TransferAction::ClearFinished,
            ));
    }
    let mut content = column![header].spacing(4);
    if app.transfer_panel_open {
        content = content.push(
            scrollable(
                Column::with_children(queue_rows(app))
                    .spacing(2)
                    .padding(iced::Padding::ZERO.right(12)),
            )
            .height(Length::Fixed(LIST_HEIGHT)),
        );
    }
    Some(panel(content))
}

fn panel(content: Column<'_, Message>) -> Element<'_, Message> {
    container(content)
        .padding([4, 12])
        .width(Length::Fill)
        .style(theme::bar)
        .into()
}

fn tab_button<'a>(label: String, tab: PanelTab, current: PanelTab) -> Element<'a, Message> {
    button(text(label).size(13))
        .style(if tab == current {
            button::secondary
        } else {
            button::text
        })
        .padding([2, 8])
        .on_press(Message::History(HistoryMessage::ShowTab(tab)))
        .into()
}

fn queue_rows(app: &Hoplix) -> Vec<Element<'_, Message>> {
    let mut groups: Vec<(&Location, Vec<&JobSnapshot>)> = Vec::new();
    let mut others = Vec::new();
    for job in &app.transfer_jobs {
        match app.waiting_location(job.id) {
            Some(location) => match groups.iter_mut().find(|(known, _)| *known == location) {
                Some((_, jobs)) => jobs.push(job),
                None => groups.push((location, vec![job])),
            },
            None => others.push(job),
        }
    }
    let mut rows = Vec::new();
    for (location, jobs) in groups {
        let strings = &app.strings;
        let first = jobs.first().map(|job| job.id).unwrap_or_default();
        rows.push(
            row![
                text(strings.format(
                    "transfer.waiting.title",
                    &[("name", &app.location_name(location))]
                ))
                .size(13)
                .style(text::primary)
                .width(Length::Fill),
                button(text(strings.get("transfer.waiting.connect")).size(12))
                    .padding([2, 8])
                    .style(theme::secondary_button)
                    .on_press(Message::Transfer(TransferAction::Connect(first))),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into(),
        );
        rows.extend(jobs.into_iter().map(|job| job_row(app, job)));
    }
    rows.extend(others.into_iter().map(|job| job_row(app, job)));
    rows
}

// History

fn history_view(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let mut filters = vec![Choice {
        value: None,
        label: strings.get("history.filter.all").to_string(),
    }];
    filters.extend(HistoryResult::ALL.into_iter().map(|result| Choice {
        value: Some(result),
        label: result_text(strings, result),
    }));
    let selected = filters
        .iter()
        .find(|choice| choice.value == app.history.filter)
        .cloned();
    let history_message = |message: HistoryMessage| Message::History(message);
    let controls = row![
        text(strings.get("history.filter")).size(13),
        pick_list(filters, selected, move |choice| {
            history_message(HistoryMessage::Filter(choice.value))
        })
        .text_size(13)
        .width(POLICY_WIDTH),
        history_button(strings.get("history.copy"), HistoryMessage::Copy),
        history_button(strings.get("history.clear"), HistoryMessage::Clear),
        Space::new().width(Length::Fill),
        text(strings.get("history.export.path")).size(13),
        text_input("", &app.history.export_path)
            .on_input(|path| Message::History(HistoryMessage::ExportPath(path)))
            .size(13)
            .padding(4)
            .width(Length::Fixed(EXPORT_WIDTH)),
        history_button(strings.get("history.export.csv"), HistoryMessage::ExportCsv),
        history_button(
            strings.get("history.export.text"),
            HistoryMessage::ExportText
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let entries = app.visible_history();
    let mut list = Column::new()
        .spacing(2)
        .padding(iced::Padding::ZERO.right(12));
    if let Some(notice) = &app.history.notice {
        list = list.push(text(notice.clone()).size(12).style(text::primary));
    }
    if !app.settings.history_enabled {
        list = list.push(text(strings.get("history.off")).size(12));
    }
    if entries.is_empty() {
        list = list.push(text(strings.get("history.empty")).size(13));
    }
    for entry in entries {
        list = list.push(history_row(strings, entry));
    }
    column![
        controls,
        scrollable(list).height(Length::Fixed(LIST_HEIGHT - 32.0))
    ]
    .spacing(4)
    .into()
}

fn history_button(label: &str, message: HistoryMessage) -> Element<'_, Message> {
    button(text(label).size(13))
        .style(button::text)
        .on_press(Message::History(message))
        .into()
}

fn history_row<'a>(strings: &'a Strings, entry: &'a HistoryEntry) -> Element<'a, Message> {
    let arrow = match entry.direction {
        TransferDirection::Download => "↓",
        TransferDirection::Upload => "↑",
        _ => "→",
    };
    let mut outcome = result_text(strings, entry.result);
    if !entry.error.is_empty() {
        outcome = format!("{outcome}: {}", entry.error);
    }
    if let Some(checksum) = entry.checksum {
        outcome = format!("{outcome} · {}", checksum_text(strings, checksum));
    }
    let separator = strings.get("number.decimal_separator");
    let mut details = vec![
        format::size(entry.size, separator),
        duration(entry.duration_ms / 1000),
    ];
    if let Some(speed) = entry.bytes_per_second() {
        details.push(speed_text(strings, speed));
    }
    row![
        text(time_text(strings, entry.finished))
            .size(12)
            .width(Length::Fixed(TIME_WIDTH)),
        text(arrow).size(14).width(18),
        text(format!("{} → {}", entry.source, entry.target))
            .size(12)
            .width(Length::Fill),
        text(details.join(" · "))
            .size(12)
            .width(Length::Fixed(DETAILS_WIDTH * 0.7)),
        text(outcome).size(12).width(Length::Fixed(DETAILS_WIDTH)),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn summary(app: &Hoplix) -> String {
    let strings = &app.strings;
    let jobs = &app.transfer_jobs;
    let active = jobs.iter().filter(|job| job.status.is_active()).count();
    let queued = jobs
        .iter()
        .filter(|job| job.status == JobStatus::Queued)
        .count();
    let failed = jobs
        .iter()
        .filter(|job| matches!(job.status, JobStatus::Failed(_)))
        .count();
    let speed: f64 = jobs.iter().map(|job| job.bytes_per_second).sum();
    let mut parts = vec![strings.format(
        "transfer.summary",
        &[
            ("active", &active.to_string()),
            ("queued", &queued.to_string()),
        ],
    )];
    if failed > 0 {
        parts.push(strings.format("transfer.summary_failed", &[("count", &failed.to_string())]));
    }
    if speed >= 1.0 {
        parts.push(speed_text(strings, speed));
    }
    parts.join(" · ")
}

fn policy_picker(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let policies: Vec<Choice<ConflictPolicy>> = [
        ConflictPolicy::Ask,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Resume,
        ConflictPolicy::Rename,
        ConflictPolicy::Skip,
    ]
    .into_iter()
    .map(|value| Choice {
        value,
        label: strings
            .get(match value {
                ConflictPolicy::Ask => "transfer.policy.ask",
                ConflictPolicy::Overwrite => "transfer.policy.overwrite",
                ConflictPolicy::Resume => "transfer.policy.resume",
                ConflictPolicy::Rename => "transfer.policy.rename",
                ConflictPolicy::Skip => "transfer.policy.skip",
            })
            .to_string(),
    })
    .collect();
    let current = app.transfers.policy();
    let selected = policies.iter().find(|c| c.value == current).cloned();
    pick_list(policies, selected, |choice| {
        Message::Transfer(TransferAction::PolicySelected(choice.value))
    })
    .text_size(13)
    .width(POLICY_WIDTH)
    .into()
}

fn action_button(label: &str, action: TransferAction) -> Element<'_, Message> {
    button(text(label).size(13))
        .style(button::text)
        .on_press(Message::Transfer(action))
        .into()
}

// Jobs

fn job_row<'a>(app: &'a Hoplix, job: &'a JobSnapshot) -> Element<'a, Message> {
    let strings = &app.strings;
    let arrow = match app
        .transfer_targets
        .get(&job.id)
        .map(|target| target.direction)
    {
        Some(Direction::Download) => "↓",
        Some(Direction::Upload) => "↑",
        _ => "→",
    };
    let mut name = job.name.clone();
    if job.is_directory
        && let Some(file) = &job.current_file
        && job.status.is_active()
    {
        name = format!("{name} – {file}");
    }
    let title = column![
        row![text(arrow).size(14).width(18), text(name).size(13)]
            .spacing(4)
            .align_y(Alignment::Center),
        progress_bar(0.0..=1.0, job.fraction()).girth(6),
    ]
    .spacing(3)
    .width(Length::Fill);
    let id = job.id;
    let mut buttons = row![].spacing(2).align_y(Alignment::Center);
    let small = |label: &'a str, action: TransferAction| {
        button(text(label).size(12))
            .padding([2, 6])
            .style(button::text)
            .on_press(Message::Transfer(action))
    };
    let waiting = app.is_waiting(id);
    match &job.status {
        JobStatus::Paused if waiting => {
            buttons = buttons
                .push(small(
                    strings.get("transfer.waiting.connect"),
                    TransferAction::Connect(id),
                ))
                .push(small(
                    strings.get("transfer.cancel"),
                    TransferAction::Cancel(id),
                ));
        }
        JobStatus::Paused => {
            buttons = buttons
                .push(small(
                    strings.get("transfer.resume"),
                    TransferAction::Resume(id),
                ))
                .push(small(
                    strings.get("transfer.cancel"),
                    TransferAction::Cancel(id),
                ));
        }
        JobStatus::Failed(_) | JobStatus::Cancelled => {
            buttons = buttons
                .push(small(
                    strings.get("transfer.retry"),
                    TransferAction::Retry(id),
                ))
                .push(small(
                    strings.get("transfer.remove"),
                    TransferAction::Remove(id),
                ));
        }
        JobStatus::Completed => {
            buttons = buttons.push(small(
                strings.get("transfer.remove"),
                TransferAction::Remove(id),
            ));
        }
        JobStatus::WaitingForDecision => {
            buttons = buttons.push(small(
                strings.get("transfer.cancel"),
                TransferAction::Cancel(id),
            ));
        }
        JobStatus::Queued
        | JobStatus::Scanning
        | JobStatus::Running
        | JobStatus::Retrying(_)
        | JobStatus::Verifying => {
            buttons = buttons
                .push(small(
                    strings.get("transfer.pause"),
                    TransferAction::Pause(id),
                ))
                .push(small(
                    strings.get("transfer.cancel"),
                    TransferAction::Cancel(id),
                ));
        }
    }
    let status = match app.held_by_site_limit(job.id) {
        Some(max) if job.status == JobStatus::Queued => {
            strings.format("transfer.status.site_limit", &[("count", &max.to_string())])
        }
        _ if waiting && job.status == JobStatus::Paused => format!(
            "{} · {}",
            strings.get("transfer.status.disconnected"),
            amount(strings, job)
        ),
        _ => details(strings, job, app.transfers.retry_policy().attempts),
    };
    row![
        title,
        text(status).size(12).width(DETAILS_WIDTH),
        container(buttons)
            .width(Length::Fixed(150.0))
            .align_x(Alignment::End),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn amount(strings: &Strings, job: &JobSnapshot) -> String {
    let separator = strings.get("number.decimal_separator");
    match job.total {
        Some(total) => strings.format(
            "transfer.amount",
            &[
                ("done", &format::size(job.transferred, separator)),
                ("total", &format::size(total, separator)),
            ],
        ),
        None => format::size(job.transferred, separator),
    }
}

fn details(strings: &Strings, job: &JobSnapshot, retries: u32) -> String {
    let amount = amount(strings, job);
    match &job.status {
        JobStatus::Queued => strings.get("transfer.status.queued").to_string(),
        JobStatus::Scanning => strings.get("transfer.status.scanning").to_string(),
        JobStatus::Running => {
            let mut parts = vec![amount];
            if job.bytes_per_second >= 1.0 {
                parts.push(speed_text(strings, job.bytes_per_second));
            }
            if let Some(seconds) = job.remaining_seconds() {
                parts.push(strings.format("transfer.remaining", &[("time", &duration(seconds))]));
            }
            parts.join(" · ")
        }
        JobStatus::Retrying(attempt) => strings.format(
            "transfer.status.retrying",
            &[
                ("attempt", &attempt.to_string()),
                ("max", &retries.to_string()),
            ],
        ),
        JobStatus::WaitingForDecision => strings.get("transfer.status.waiting").to_string(),
        JobStatus::Verifying => strings.get("transfer.status.verifying").to_string(),
        JobStatus::Paused => format!("{} · {amount}", strings.get("transfer.status.paused")),
        JobStatus::Completed if job.skipped > 0 => strings.format(
            "transfer.status.completed_skipped",
            &[("count", &job.skipped.to_string())],
        ),
        JobStatus::Completed => match job.checksum {
            Some(checksum) => format!(
                "{} · {amount} · {}",
                strings.get("transfer.status.completed"),
                checksum_text(strings, checksum)
            ),
            None => format!("{} · {amount}", strings.get("transfer.status.completed")),
        },
        JobStatus::Failed(error) => ui::error_text(strings, error),
        JobStatus::Cancelled => strings.get("transfer.status.cancelled").to_string(),
    }
}

// Formatting

fn speed_text(strings: &Strings, bytes_per_second: f64) -> String {
    let separator = strings.get("number.decimal_separator");
    strings.format(
        "transfer.speed",
        &[("speed", &format::size(bytes_per_second as u64, separator))],
    )
}

pub fn duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_as_clock() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(65), "1:05");
        assert_eq!(duration(3723), "1:02:03");
    }
}
