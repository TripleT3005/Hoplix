use std::time::{SystemTime, UNIX_EPOCH};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

// Conversion

pub fn local(time: SystemTime) -> Option<DateTime> {
    let seconds = unix_seconds(time)?;
    platform_local(seconds).or_else(|| Some(utc(seconds)))
}

pub fn utc(seconds: i64) -> DateTime {
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: u32::try_from(remainder / 3600).unwrap_or(0),
        minute: u32::try_from(remainder % 3600 / 60).unwrap_or(0),
    }
}

pub fn format(time: &DateTime, pattern: &str) -> String {
    pattern
        .replace("{year}", &format!("{:04}", time.year))
        .replace("{month}", &format!("{:02}", time.month))
        .replace("{day}", &format!("{:02}", time.day))
        .replace("{hour}", &format!("{:02}", time.hour))
        .replace("{minute}", &format!("{:02}", time.minute))
}

// Helpers

fn unix_seconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs()).ok().map(|s| -s),
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
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

// Platform

#[cfg(windows)]
#[allow(unsafe_code)]
fn platform_local(seconds: i64) -> Option<DateTime> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    const EPOCH_DIFFERENCE: i64 = 11_644_473_600;
    let ticks = u64::try_from(seconds.checked_add(EPOCH_DIFFERENCE)?)
        .ok()?
        .checked_mul(10_000_000)?;
    let file_time = FILETIME {
        dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut universal: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let converted = unsafe {
        FileTimeToSystemTime(&raw const file_time, &raw mut universal) != 0
            && SystemTimeToTzSpecificLocalTime(
                std::ptr::null(),
                &raw const universal,
                &raw mut local,
            ) != 0
    };
    converted.then(|| DateTime {
        year: i64::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        minute: u32::from(local.wMinute),
    })
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn platform_local(seconds: i64) -> Option<DateTime> {
    let time = libc::time_t::try_from(seconds).ok()?;
    let mut fields: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&raw const time, &raw mut fields) };
    (!result.is_null()).then(|| DateTime {
        year: i64::from(fields.tm_year) + 1900,
        month: u32::try_from(fields.tm_mon + 1).unwrap_or(1),
        day: u32::try_from(fields.tm_mday).unwrap_or(1),
        hour: u32::try_from(fields.tm_hour).unwrap_or(0),
        minute: u32::try_from(fields.tm_min).unwrap_or(0),
    })
}

#[cfg(not(any(windows, unix)))]
fn platform_local(_seconds: i64) -> Option<DateTime> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_conversion_handles_known_dates() {
        let check = |seconds, expected: (i64, u32, u32, u32, u32)| {
            let time = utc(seconds);
            assert_eq!(
                (time.year, time.month, time.day, time.hour, time.minute),
                expected,
                "{seconds}"
            );
        };
        check(0, (1970, 1, 1, 0, 0));
        check(951_782_400, (2000, 2, 29, 0, 0));
        check(1_795_528_740, (2026, 11, 24, 13, 59));
        check(-1, (1969, 12, 31, 23, 59));
        check(4_102_444_800, (2100, 1, 1, 0, 0));
    }

    #[test]
    fn patterns_are_filled_with_padded_fields() {
        let time = DateTime {
            year: 2026,
            month: 3,
            day: 4,
            hour: 7,
            minute: 5,
        };
        assert_eq!(
            format(&time, "{year}-{month}-{day} {hour}:{minute}"),
            "2026-03-04 07:05"
        );
        assert_eq!(
            format(&time, "{day}.{month}.{year} {hour}:{minute}"),
            "04.03.2026 07:05"
        );
    }

    #[test]
    fn local_time_is_close_to_utc() {
        let now = SystemTime::now();
        let local = local(now).expect("local time failed");
        let universal = utc(unix_seconds(now).expect("time before epoch"));
        let difference = (local.year * 372 + i64::from(local.month) * 31 + i64::from(local.day))
            - (universal.year * 372 + i64::from(universal.month) * 31 + i64::from(universal.day));
        assert!(difference.abs() <= 1, "{local:?} vs {universal:?}");
    }
}
