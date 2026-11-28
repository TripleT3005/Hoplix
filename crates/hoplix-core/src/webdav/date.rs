use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Constants

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

// Parsing

pub(crate) fn parse_http_date(text: &str) -> Option<SystemTime> {
    let mut month = None;
    let mut day = None;
    let mut year = None;
    let mut time = None;
    for token in text
        .split([' ', ',', '-'])
        .filter(|token| !token.is_empty())
    {
        let lower = token.to_ascii_lowercase();
        let alphabetic = lower.len() >= 3 && lower.bytes().all(|byte| byte.is_ascii_alphabetic());
        if alphabetic && let Some(index) = MONTHS.iter().position(|name| lower.starts_with(name)) {
            month.get_or_insert(index as i64 + 1);
        } else if token.contains(':') {
            time.get_or_insert(parse_time(token)?);
        } else if token.bytes().all(|byte| byte.is_ascii_digit()) {
            let value: i64 = token.parse().ok()?;
            match token.len() {
                1 | 2 if day.is_none() => day = Some(value),
                2 => {
                    year = Some(if value < 70 {
                        2000 + value
                    } else {
                        1900 + value
                    })
                }
                4 => year = Some(value),
                _ => return None,
            }
        }
    }
    let (month, day, year, (hour, minute, second)) = (month?, day?, year?, time?);
    if !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    let seconds = u64::try_from(seconds).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn parse_time(token: &str) -> Option<(i64, i64, i64)> {
    let mut parts = token.split(':').map(|part| part.parse::<i64>().ok());
    let hour = parts.next()??;
    let minute = parts.next()??;
    let second = parts.next().unwrap_or(Some(0))?;
    let valid = parts.next().is_none()
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..=60).contains(&second);
    valid.then_some((hour, minute, second))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(text: &str) -> Option<u64> {
        parse_http_date(text).map(|time| {
            time.duration_since(UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs()
        })
    }

    #[test]
    fn rfc_1123_dates_are_parsed() {
        assert_eq!(
            seconds("Sun, 15 Mar 2026 13:00:00 GMT"),
            Some(1_773_579_600)
        );
        assert_eq!(seconds("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            seconds("Sun, 29 Feb 2004 23:59:59 GMT"),
            Some(1_078_099_199)
        );
        assert_eq!(seconds("15 Mar 2026 13:00:00 GMT"), Some(1_773_579_600));
    }

    #[test]
    fn older_formats_are_parsed() {
        assert_eq!(
            seconds("Sunday, 15-Mar-26 13:00:00 GMT"),
            Some(1_773_579_600)
        );
        assert_eq!(seconds("Sun Mar 15 13:00:00 2026"), Some(1_773_579_600));
    }

    #[test]
    fn broken_dates_are_ignored() {
        for text in [
            "",
            "yesterday",
            "Sun, 15 Mar 2026",
            "Sun, 32 Mar 2026 13:00:00 GMT",
            "Sun, 15 Foo 2026 13:00:00 GMT",
            "Sun, 15 Mar 2026 25:00:00 GMT",
            "Sun, 15 Mar 2026 13:00:xx GMT",
            "Wed, 31 Dec 1969 23:59:59 GMT",
        ] {
            assert_eq!(seconds(text), None, "{text}");
        }
    }
}
