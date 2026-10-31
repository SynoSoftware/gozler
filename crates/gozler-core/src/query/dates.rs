//! The date resolver: a `dm:` or `dc:` value turned into the UTC bounds the
//! daemon compares, from the query start and its time zone.
//!
//! Calendar periods run between local midnights, with weeks starting on the
//! user's first day of the week, so a daylight-saving change can make a day
//! last 23 or 25 hours. Day and week durations end at the query start and
//! count whole multiples of 24 hours. A comparison compares times, as es
//! does at the value's granularity: `>today` is after today, `<=today` up to
//! the end of today, and `<7days` older than the last 7 days. A range
//! `lower..upper` runs from the start of `lower` to the end of `upper`.
//!
//! The client resolves with its own clock and time zone, and the daemon's
//! text path with its own, so the bounds travel as UTC and the daemon never
//! needs the client's time zone.

use super::parse::{Malformed, QueryError};
use crate::output::unix_ms_to_filetime;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};
use serde::{Deserialize, Serialize};

/// Which date of an entry a date function tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateField {
    /// `dm:`
    Modified,
    /// `dc:`
    Created,
}

impl DateField {
    /// The function's name as a query writes it, with its colon.
    pub fn function(self) -> &'static str {
        match self {
            DateField::Modified => "dm:",
            DateField::Created => "dc:",
        }
    }
}

impl std::fmt::Display for DateField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.function())
    }
}

/// The clock and regional calendar against which date values resolve.
#[derive(Clone, Debug)]
pub struct QueryStart {
    pub now: Timestamp,
    pub time_zone: TimeZone,
    pub week_start: Weekday,
}

/// An inclusive range of FILETIMEs: 100-nanosecond ticks since 1601-01-01
/// UTC, the form the index holds dates in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub min: i64,
    pub max: i64,
}

impl DateRange {
    pub fn contains(self, filetime: i64) -> bool {
        self.min <= filetime && filetime <= self.max
    }
}

/// The times from `start` up to, not including, `end`.
#[derive(Clone, Copy)]
struct Period {
    start: Timestamp,
    end: Timestamp,
}

/// The [`DateRange`] of a value of `field`'s function, read against `start`.
pub(crate) fn date_range(
    value: &str,
    start: &QueryStart,
    field: DateField,
) -> Result<DateRange, QueryError> {
    let value = value.trim();
    // The first tick at or after `at`, and the last tick before it.
    let from = |at: Timestamp| unix_ms_to_filetime(at.as_millisecond());
    let before = |at: Timestamp| from(at) - 1;

    if let Some((lower, upper)) = value.split_once("..") {
        let lower = (!lower.trim().is_empty())
            .then(|| period(lower, start, field))
            .transpose()?;
        let upper = (!upper.trim().is_empty())
            .then(|| period(upper, start, field))
            .transpose()?;
        if let (Some(lower), Some(upper)) = (lower, upper)
            && lower.start > upper.start
        {
            return Err(QueryError::Malformed(Malformed::ReversedDates(
                field,
                value.to_string(),
            )));
        }
        return Ok(DateRange {
            min: lower.map_or(i64::MIN, |p| from(p.start)),
            max: upper.map_or(i64::MAX, |p| before(p.end)),
        });
    }

    // Two-character operators before their one-character prefixes.
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = value.strip_prefix(op) {
            let p = period(rest, start, field)?;
            return Ok(match op {
                ">=" => DateRange {
                    min: from(p.start),
                    max: i64::MAX,
                },
                ">" => DateRange {
                    min: from(p.end),
                    max: i64::MAX,
                },
                "<=" => DateRange {
                    min: i64::MIN,
                    max: before(p.end),
                },
                "<" => DateRange {
                    min: i64::MIN,
                    max: before(p.start),
                },
                // The only remaining op.
                _ => DateRange {
                    min: from(p.start),
                    max: before(p.end),
                },
            });
        }
    }
    let p = period(value, start, field)?;
    Ok(DateRange {
        min: from(p.start),
        max: before(p.end),
    })
}

/// Calendar periods use local midnight; durations end at the query start.
fn period(value: &str, start: &QueryStart, field: DateField) -> Result<Period, QueryError> {
    let value = value.trim();
    let malformed = |reason: fn(DateField, String) -> Malformed| {
        QueryError::Malformed(reason(field, value.to_string()))
    };
    let out_of_range = |_| malformed(Malformed::DateOutOfRange);
    let today = || start.now.to_zoned(start.time_zone.clone()).date();
    let lower = value.to_ascii_lowercase();
    let (first, end) = match lower.as_str() {
        "today" => (today(), today().tomorrow().map_err(out_of_range)?),
        "yesterday" => (today().yesterday().map_err(out_of_range)?, today()),
        "tomorrow" => {
            let first = today().tomorrow().map_err(out_of_range)?;
            (first, first.tomorrow().map_err(out_of_range)?)
        }
        _ => {
            let duration = lower.strip_prefix("last").unwrap_or(&lower);
            for (unit, seconds) in [
                ("minutes", 60),
                ("hours", 3_600),
                ("days", 86_400),
                ("weeks", 604_800),
            ] {
                let Some(count) = duration.strip_suffix(unit) else {
                    continue;
                };
                let count = count
                    .parse::<u32>()
                    .ok()
                    .filter(|&count| count > 0)
                    .ok_or_else(|| {
                        malformed(if unit == "days" {
                            Malformed::Days
                        } else {
                            Malformed::Date
                        })
                    })?;
                let length = SignedDuration::from_secs(i64::from(count) * seconds);
                let first = start.now.checked_sub(length).map_err(out_of_range)?;
                return Ok(Period {
                    start: first,
                    end: start.now,
                });
            }
            match calendar_period(&lower, today(), start.week_start).map_err(out_of_range)? {
                Some(period) => period,
                None => {
                    let date = iso_date(value).ok_or_else(|| malformed(Malformed::Date))?;
                    (date, date.tomorrow().map_err(out_of_range)?)
                }
            }
        }
    };
    let midnight = |date: Date| -> Result<Timestamp, QueryError> {
        Ok(date
            .to_zoned(start.time_zone.clone())
            .map_err(out_of_range)?
            .timestamp())
    };
    Ok(Period {
        start: midnight(first)?,
        end: midnight(end)?,
    })
}

fn calendar_period(
    value: &str,
    today: Date,
    week_start: Weekday,
) -> Result<Option<(Date, Date)>, jiff::Error> {
    let Some((unit, offset)) = [
        ("this", 0),
        ("current", 0),
        ("last", -1),
        ("past", -1),
        ("prev", -1),
        ("next", 1),
        ("coming", 1),
    ]
    .into_iter()
    .find_map(|(prefix, offset)| value.strip_prefix(prefix).map(|unit| (unit, offset))) else {
        return Ok(None);
    };
    let (first, length) = match unit {
        "week" => (
            today.checked_sub(Span::new().days(today.weekday().since(week_start)))?,
            Span::new().weeks(1),
        ),
        "month" => (today.first_of_month(), Span::new().months(1)),
        "year" => (today.first_of_year(), Span::new().years(1)),
        _ => return Ok(None),
    };
    let first = first.checked_add(length.checked_mul(offset)?)?;
    Ok(Some((first, first.checked_add(length)?)))
}

/// The date `YYYY-MM-DD` writes, when it is a real one.
fn iso_date(value: &str) -> Option<Date> {
    let bytes = value.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if !shaped {
        return None;
    }
    let year = value[..4].parse().ok()?;
    let month = value[5..7].parse().ok()?;
    let day = value[8..].parse().ok()?;
    Date::new(year, month, day).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// US Eastern time as a POSIX rule, so the tests do not depend on the
    /// time zone database or the machine's zone: daylight saving from the
    /// second Sunday of March to the first Sunday of November.
    fn eastern() -> TimeZone {
        TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").expect("a valid rule")
    }

    fn start(now: &str, time_zone: TimeZone) -> QueryStart {
        QueryStart {
            week_start: jiff::civil::Weekday::Monday,
            now: now.parse().expect("an RFC 3339 instant"),
            time_zone,
        }
    }

    /// The FILETIME of an RFC 3339 instant.
    fn ft(instant: &str) -> i64 {
        let at: Timestamp = instant.parse().expect("an RFC 3339 instant");
        unix_ms_to_filetime(at.as_millisecond())
    }

    /// The range from the first tick of `from` to the last tick before
    /// `until`.
    fn between(from: &str, until: &str) -> DateRange {
        DateRange {
            min: ft(from),
            max: ft(until) - 1,
        }
    }

    fn range(value: &str, start: &QueryStart) -> DateRange {
        date_range(value, start, DateField::Modified).unwrap_or_else(|e| panic!("{value}: {e}"))
    }

    #[test]
    fn recent_minutes_and_hours_end_at_the_query_start() {
        let start = start("2026-10-31T12:00:00Z", TimeZone::UTC);
        assert_eq!(
            range("10minutes", &start),
            between("2026-10-31T11:50:00Z", "2026-10-31T12:00:00Z")
        );
        assert_eq!(
            range("last2hours", &start),
            between("2026-10-31T10:00:00Z", "2026-10-31T12:00:00Z")
        );
        for value in ["0minutes", "-1hours", "999999999999hours"] {
            assert!(
                date_range(value, &start, DateField::Modified).is_err(),
                "{value}"
            );
        }
    }

    /// A calendar day starts at local midnight, not at UTC midnight or at the
    /// query start: half an hour after midnight, a file from one tick before
    /// midnight is yesterday's.
    #[test]
    fn a_day_runs_from_local_midnight_to_the_next() {
        let start = start("2026-10-18T04:30:00Z", eastern());
        let today = range("today", &start);
        assert_eq!(
            today,
            between("2026-10-18T04:00:00Z", "2026-10-19T04:00:00Z")
        );
        assert_eq!(range("2026-10-18", &start), today);
        let midnight = ft("2026-10-18T04:00:00Z");
        assert!(today.contains(midnight));
        assert!(!today.contains(midnight - 1));
        assert!(range("yesterday", &start).contains(midnight - 1));
    }

    /// `Ndays` ends at the query start, so a timestamp in the future, which a
    /// skewed clock or an archive can give a file, is not in the last days,
    /// though it is in today and after the last days.
    #[test]
    fn a_future_timestamp_is_today_but_not_in_the_last_days() {
        let start = start("2026-10-18T12:00:00Z", TimeZone::UTC);
        let future = ft("2026-10-18T18:00:00Z");
        let week = range("7days", &start);
        assert_eq!(
            week,
            between("2026-10-11T12:00:00Z", "2026-10-18T12:00:00Z")
        );
        assert!(!week.contains(future));
        assert!(range("today", &start).contains(future));
        assert!(range(">7days", &start).contains(future));
        assert!(!range(">today", &start).contains(future));
        assert_eq!(range("last7days", &start), week);
        assert_eq!(range("1weeks", &start), week);
    }

    #[test]
    fn calendar_periods_follow_the_regional_week_start() {
        let mut start = start("2026-10-18T12:00:00Z", TimeZone::UTC);
        assert_eq!(
            range("thisweek", &start),
            between("2026-10-12T00:00:00Z", "2026-10-19T00:00:00Z")
        );
        start.week_start = Weekday::Sunday;
        assert_eq!(
            range("thisweek", &start),
            between("2026-10-18T00:00:00Z", "2026-10-25T00:00:00Z")
        );
        assert_eq!(
            range("lastweek", &start),
            between("2026-10-11T00:00:00Z", "2026-10-18T00:00:00Z")
        );
    }

    #[test]
    fn calendar_periods_cross_month_and_year_boundaries() {
        let start = start("2024-01-15T12:00:00Z", TimeZone::UTC);
        for (value, from, until) in [
            ("lastmonth", "2023-12-01T00:00:00Z", "2024-01-01T00:00:00Z"),
            ("thismonth", "2024-01-01T00:00:00Z", "2024-02-01T00:00:00Z"),
            ("nextmonth", "2024-02-01T00:00:00Z", "2024-03-01T00:00:00Z"),
            ("lastyear", "2023-01-01T00:00:00Z", "2024-01-01T00:00:00Z"),
            ("thisyear", "2024-01-01T00:00:00Z", "2025-01-01T00:00:00Z"),
        ] {
            assert_eq!(range(value, &start), between(from, until), "{value}");
        }
        assert_eq!(range("pastmonth", &start), range("lastmonth", &start));
        assert_eq!(range(">=lastmonth", &start).min, ft("2023-12-01T00:00:00Z"));
    }

    /// A day of a daylight-saving change lasts 23 or 25 hours, and the last
    /// days stay whole multiples of 24 hours across the change.
    #[test]
    fn a_daylight_saving_change_moves_midnight_but_not_the_last_days() {
        let start = start("2026-03-10T12:00:00Z", eastern());
        assert_eq!(
            range("2026-03-08", &start),
            between("2026-03-08T05:00:00Z", "2026-03-09T04:00:00Z")
        );
        assert_eq!(
            range("2026-11-01", &start),
            between("2026-11-01T04:00:00Z", "2026-11-02T05:00:00Z")
        );
        assert_eq!(range("7days", &start).min, ft("2026-03-03T12:00:00Z"));
        assert_eq!(
            range("lastweek", &start),
            between("2026-03-02T05:00:00Z", "2026-03-09T04:00:00Z")
        );
    }

    /// A comparison compares times at the value's granularity, as es does,
    /// and a range runs from the start of its lower value to the end of its
    /// upper one.
    #[test]
    fn comparisons_and_ranges_bound_by_whole_days() {
        let start = start("2026-10-18T12:00:00Z", TimeZone::UTC);
        let day = |date: &str| format!("{date}T00:00:00Z");
        assert_eq!(range(">2026-10-17", &start).min, ft(&day("2026-10-18")));
        assert_eq!(range(">=2026-10-17", &start).min, ft(&day("2026-10-17")));
        assert_eq!(range("<2026-10-17", &start).max, ft(&day("2026-10-17")) - 1);
        assert_eq!(
            range("<=2026-10-17", &start).max,
            ft(&day("2026-10-18")) - 1
        );
        assert_eq!(
            range("2026-01-01..2026-03-31", &start),
            between(&day("2026-01-01"), &day("2026-04-01"))
        );
        assert_eq!(
            range("2026-10-17..", &start),
            DateRange {
                min: ft(&day("2026-10-17")),
                max: i64::MAX,
            }
        );
    }

    /// A value that names no period is invalid input, not a test that
    /// matches nothing, and so is a range whose lower value starts after its
    /// upper one.
    #[test]
    fn a_bad_date_or_a_reversed_range_is_refused() {
        let start = start("2026-10-18T12:00:00Z", TimeZone::UTC);
        for value in [
            "2026-03-31..2026-01-01",
            "7days..30days",
            "2026-02-30",
            "0days",
            "0weeks",
            "lastdays",
            "999999999999weeks",
            "10/18/2026",
            "",
        ] {
            assert!(
                matches!(
                    date_range(value, &start, DateField::Modified),
                    Err(QueryError::Malformed(
                        Malformed::ReversedDates(..) | Malformed::Days(..) | Malformed::Date(..)
                    ))
                ),
                "{value}"
            );
        }
    }
}
