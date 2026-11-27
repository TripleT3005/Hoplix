use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Constants

const SECONDS_PER_DAY: i64 = 86_400;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Timestamp {
    pub date: String,
    pub full: String,
}

// Formatting

pub(crate) fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

pub(crate) fn timestamp(seconds: i64) -> Timestamp {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let rest = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let date = format!("{year:04}{month:02}{day:02}");
    let full = format!(
        "{date}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    );
    Timestamp { date, full }
}

// Parsing

pub(crate) fn parse_iso8601(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.strip_suffix('Z').unwrap_or(time);
    let time = time.split(['.', '+']).next()?;
    clock_seconds(year, month, day, time)
}

pub(crate) fn parse_http_date(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace();
    parts.next()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let month = u32::try_from(MONTHS.iter().position(|name| name.eq_ignore_ascii_case(month))?)
        .ok()?
        + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    clock_seconds(year, month, day, parts.next()?)
}

pub(crate) fn system_time(seconds: i64) -> Option<SystemTime> {
    match u64::try_from(seconds) {
        Ok(seconds) => UNIX_EPOCH.checked_add(Duration::from_secs(seconds)),
        Err(_) => UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs())),
    }
}

fn clock_seconds(year: i64, month: u32, day: u32, time: &str) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut clock = time.split(':');
    let hours: i64 = clock.next()?.parse().ok()?;
    let minutes: i64 = clock.next()?.parse().ok()?;
    let seconds: i64 = clock.next()?.parse().ok()?;
    if hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * SECONDS_PER_DAY + hours * 3600 + minutes * 60 + seconds)
}

// Calendar

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
        + i64::from(day)
        - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARCH_15: i64 = 1_773_579_600;

    #[test]
    fn timestamps_use_the_basic_iso_format() {
        assert_eq!(
            timestamp(MARCH_15),
            Timestamp {
                date: "20260315".to_string(),
                full: "20260315T130000Z".to_string(),
            }
        );
        assert_eq!(timestamp(0).full, "19700101T000000Z");
        assert_eq!(timestamp(951_782_400).date, "20000229");
    }

    #[test]
    fn s3_and_http_dates_parse() {
        assert_eq!(parse_iso8601("2026-03-15T13:00:00.000Z"), Some(MARCH_15));
        assert_eq!(parse_iso8601("2026-03-15T13:00:00Z"), Some(MARCH_15));
        assert_eq!(
            parse_http_date("Sun, 15 Mar 2026 13:00:00 GMT"),
            Some(MARCH_15)
        );
        for text in ["", "2026-13-15T13:00:00Z", "2026-03-15", "2026-03-15T25:00:00Z"] {
            assert_eq!(parse_iso8601(text), None, "{text}");
        }
        assert_eq!(parse_http_date("Sun, 15 Foo 2026 13:00:00 GMT"), None);
    }

    #[test]
    fn calendar_conversions_round_trip() {
        for days in [-1_000, -1, 0, 59, 60, 10_957, 20_527, 100_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        let time = system_time(MARCH_15).expect("in range");
        assert_eq!(unix_seconds(time), MARCH_15);
    }
}
