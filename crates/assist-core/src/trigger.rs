//! Trigger rules, without the clock or the OS (PLAN.md section 3.12): when a schedule is
//! next due, and when a fact condition turns true. The daemon reads the clock and watches
//! files; these decide.

use std::time::Duration;

use jiff::{civil::Weekday, SignedDuration, ZonedDateTime as Zoned};

use crate::condition::Condition;
use crate::facts::Facts;

/// Shortest `every`: anything faster is a loop, not a schedule.
pub const MIN_EVERY: Duration = Duration::from_secs(60);

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// A time of day in the local time zone, on some weekdays (Monday first).
    At {
        /// Hour, 0 to 23.
        hour: i8,
        /// Minute, 0 to 59.
        minute: i8,
        /// Which weekdays, Monday first.
        days: [bool; 7],
    },
    /// A fixed interval from the last run.
    Every(Duration),
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

impl Schedule {
    /// Parses `at` ("07:30", "7:30 mon-fri", "18:00 sat,sun") or `every` ("15m", "1h").
    pub fn parse(at: Option<&str>, every: Option<&str>) -> Result<Self, String> {
        match (at, every) {
            (Some(at), None) => parse_at(at),
            (None, Some(every)) => {
                let interval = crate::config::parse_duration(every)
                    .ok_or_else(|| format!("every {every:?}: use e.g. 15m, 1h, 1d"))?;
                if interval < MIN_EVERY {
                    return Err(format!("every {every:?}: at least 1m"));
                }
                Ok(Self::Every(interval))
            }
            _ => Err("give either at or every".to_owned()),
        }
    }

    /// The first time after `now` it fires, in `now`'s time zone. A time that does not
    /// exist on a daylight-saving day moves forward.
    pub fn next_after(&self, now: &Zoned) -> Option<Zoned> {
        match self {
            Self::Every(interval) => now
                .checked_add(SignedDuration::try_from(*interval).ok()?)
                .ok(),
            Self::At { hour, minute, days } => {
                let mut date = now.date();
                for _ in 0..8 {
                    let day = usize::try_from(date.weekday().to_monday_zero_offset()).ok()?;
                    if days.get(day) == Some(&true) {
                        let at = date
                            .at(*hour, *minute, 0, 0)
                            .to_zoned_datetime(now.time_zone().clone());
                        match at {
                            Ok(at) if at > *now => return Some(at),
                            _ => {}
                        }
                    }
                    date = date.tomorrow().ok()?;
                }
                None
            }
        }
    }
}

fn parse_at(text: &str) -> Result<Schedule, String> {
    let error = || format!("at {text:?}: use e.g. 07:30, 07:30 mon-fri, 18:00 sat,sun");
    let mut parts = text.split_whitespace();
    let (hour, minute) = parts
        .next()
        .and_then(|t| t.split_once(':'))
        .ok_or_else(error)?;
    let hour: i8 = hour
        .parse()
        .ok()
        .filter(|h| (0..24).contains(h))
        .ok_or_else(error)?;
    let minute: i8 = minute
        .parse()
        .ok()
        .filter(|m| (0..60).contains(m))
        .ok_or_else(error)?;
    let days = match parts.next() {
        None | Some("daily") => [true; 7],
        Some(spec) => parse_days(spec).ok_or_else(error)?,
    };
    if parts.next().is_some() {
        return Err(error());
    }
    Ok(Schedule::At { hour, minute, days })
}

/// "mon-fri", "sat,sun", "fri-mon" (wraps) into Monday-first flags.
fn parse_days(spec: &str) -> Option<[bool; 7]> {
    let index = |day: &str| DAYS.iter().position(|d| d.eq_ignore_ascii_case(day));
    let mut days = [false; 7];
    for part in spec.split(',') {
        let (from, to) = match part.split_once('-') {
            Some((from, to)) => (index(from)?, index(to)?),
            None => (index(part)?, index(part)?),
        };
        let mut day = from;
        loop {
            days[day] = true;
            if day == to {
                break;
            }
            day = (day + 1) % 7;
        }
    }
    Some(days)
}

/// What to do when a schedule's time passed while the device slept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Missed {
    /// Run once on wake.
    Run,
    /// Wait for the next time.
    #[default]
    Skip,
}

/// A fact condition that fires when it turns true, not on every check while it stays true.
#[derive(Debug, Clone)]
pub struct FactEdge {
    condition: Condition,
    was_true: bool,
}

impl FactEdge {
    /// Starts as not true, so a condition already true at start fires once.
    pub fn new(condition: Condition) -> Self {
        Self {
            condition,
            was_true: false,
        }
    }

    /// Whether the condition just turned true.
    pub fn check(&mut self, facts: &Facts) -> bool {
        let now = self.condition.eval(&|name| facts.get(name));
        let fired = now && !self.was_true;
        self.was_true = now;
        fired
    }
}

/// Weekday names as written in schedules, for messages.
pub fn weekday_name(day: Weekday) -> &'static str {
    let i = usize::try_from(day.to_monday_zero_offset()).unwrap_or(0);
    DAYS.get(i).copied().unwrap_or("mon")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Value;
    use jiff::tz::{Offset, TimeZone};

    fn at(text: &str) -> Zoned {
        let tz = TimeZone::fixed(Offset::from_hours(5).unwrap());
        text.parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned_datetime(tz)
            .unwrap()
    }

    #[test]
    fn at_schedules_find_the_next_matching_day() {
        // 2026-11-27 is a Friday.
        let s = Schedule::parse(Some("07:30 mon-fri"), None).unwrap();
        let next = s.next_after(&at("2026-11-27T06:00")).unwrap();
        assert_eq!(next.datetime(), "2026-11-27T07:30".parse().unwrap());
        let next = s.next_after(&at("2026-11-27T08:00")).unwrap();
        assert_eq!(next.datetime(), "2026-11-30T07:30".parse().unwrap());
        assert_eq!(weekday_name(next.date().weekday()), "mon");
        let weekend = Schedule::parse(Some("18:00 sat,sun"), None).unwrap();
        let next = weekend.next_after(&at("2026-11-29T19:00")).unwrap();
        assert_eq!(next.datetime(), "2026-12-05T18:00".parse().unwrap());
        let daily = Schedule::parse(Some("0:05"), None).unwrap();
        let next = daily.next_after(&at("2026-11-27T00:05")).unwrap();
        assert_eq!(next.datetime(), "2026-11-28T00:05".parse().unwrap());
    }

    #[test]
    fn every_and_bad_schedules() {
        let s = Schedule::parse(None, Some("15m")).unwrap();
        let next = s.next_after(&at("2026-11-27T06:00")).unwrap();
        assert_eq!(next.datetime(), "2026-11-27T06:15".parse().unwrap());
        for (a, e) in [
            (Some("25:00"), None),
            (Some("07:30 someday"), None),
            (Some("07:30 mon extra"), None),
            (None, Some("10s")),
            (Some("07:30"), Some("1h")),
            (None, None),
        ] {
            assert!(Schedule::parse(a, e).is_err(), "{a:?} {e:?}");
        }
        assert_eq!(
            parse_days("fri-mon").unwrap(),
            [true, false, false, false, true, true, true]
        );
    }

    #[test]
    fn fact_edges_fire_once_per_change() {
        let facts = Facts::default();
        let mut edge = FactEdge::new(Condition::parse("node.battery < 20").unwrap());
        assert!(!edge.check(&facts));
        facts.set("node.battery", Value::Number(15.0));
        assert!(edge.check(&facts));
        assert!(!edge.check(&facts));
        facts.set("node.battery", Value::Number(50.0));
        assert!(!edge.check(&facts));
        facts.set("node.battery", Value::Number(10.0));
        assert!(edge.check(&facts));
    }
}
