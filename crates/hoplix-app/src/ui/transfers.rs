use hoplix_core::{ConflictPolicy, JobSnapshot, JobStatus};
use iced::widget::{
    Column, Space, button, column, container, pick_list, progress_bar, row, scrollable, text,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::i18n::Strings;
use crate::transfers::{Direction, TransferAction};
use crate::ui::{self, Choice};
use crate::{format, theme};

// Constants

const LIST_HEIGHT: f32 = 220.0;
const DETAILS_WIDTH: f32 = 300.0;
const POLICY_WIDTH: f32 = 190.0;

// Panel

pub fn view(app: &Hoplix) -> Option<Element<'_, Message>> {
    if app.transfer_jobs.is_empty() {
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
    header = header.push(Space::new().width(Length::Fill));
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
        let rows = app.transfer_jobs.iter().map(|job| job_row(app, job));
        content = content.push(
            scrollable(
                Column::with_children(rows)
                    .spacing(2)
                    .padding(iced::Padding::ZERO.right(12)),
            )
            .height(Length::Fixed(LIST_HEIGHT)),
        );
    }
    Some(
        container(content)
            .padding([4, 12])
            .width(Length::Fill)
            .style(theme::bar)
            .into(),
    )
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
    match &job.status {
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
        JobStatus::Queued | JobStatus::Scanning | JobStatus::Running | JobStatus::Retrying(_) => {
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
    row![
        title,
        text(details(strings, job)).size(12).width(DETAILS_WIDTH),
        container(buttons)
            .width(Length::Fixed(150.0))
            .align_x(Alignment::End),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn details(strings: &Strings, job: &JobSnapshot) -> String {
    let separator = strings.get("number.decimal_separator");
    let amount = match job.total {
        Some(total) => strings.format(
            "transfer.amount",
            &[
                ("done", &format::size(job.transferred, separator)),
                ("total", &format::size(total, separator)),
            ],
        ),
        None => format::size(job.transferred, separator),
    };
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
                ("max", &hoplix_core::MAX_RETRIES.to_string()),
            ],
        ),
        JobStatus::WaitingForDecision => strings.get("transfer.status.waiting").to_string(),
        JobStatus::Paused => format!("{} · {amount}", strings.get("transfer.status.paused")),
        JobStatus::Completed if job.skipped > 0 => strings.format(
            "transfer.status.completed_skipped",
            &[("count", &job.skipped.to_string())],
        ),
        JobStatus::Completed => {
            format!("{} · {amount}", strings.get("transfer.status.completed"))
        }
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
