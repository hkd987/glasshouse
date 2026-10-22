//! When a routine runs by itself (DA-4): a few words a person can read and
//! a model can write, such as `weekdays 09:00`, checked against the
//! computer's local time once a minute.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::Weekday;

/// Days, Monday first, and a time of day or every hour on the hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Schedule {
    days: [bool; 7],
    /// `None`: every hour, on the hour.
    at: Option<(u8, u8)>,
}

const DAYS: [(&str, &str); 7] = [
    ("mon", "Monday"),
    ("tue", "Tuesday"),
    ("wed", "Wednesday"),
    ("thu", "Thursday"),
    ("fri", "Friday"),
    ("sat", "Saturday"),
    ("sun", "Sunday"),
];
const EVERY: [bool; 7] = [true; 7];
const WEEKDAYS: [bool; 7] = [true, true, true, true, true, false, false];
const WEEKENDS: [bool; 7] = [false, false, false, false, false, true, true];

/// What a schedule's text should look like, for refusals and tool schemas.
pub const FORMS: &str = "\"daily 18:30\", \"weekdays 09:00\", \"weekends 10:00\", \
                         \"mon,thu 08:30\" or \"hourly\", in this computer's local time";

impl Schedule {
    /// Whether it runs in the minute that starts at `hour:minute` on `day`,
    /// in local time.
    pub fn due(&self, day: Weekday, hour: u8, minute: u8) -> bool {
        self.days[usize::from(day.number_days_from_monday())]
            && match self.at {
                Some(at) => at == (hour, minute),
                None => minute == 0,
            }
    }

    /// In words: "every weekday at 09:00".
    pub fn describe(&self) -> String {
        let days = match self.days {
            EVERY => "every day".to_owned(),
            WEEKDAYS => "every weekday".to_owned(),
            WEEKENDS => "every Saturday and Sunday".to_owned(),
            days => {
                let names: Vec<&str> = DAYS
                    .iter()
                    .zip(days)
                    .filter(|(_, on)| *on)
                    .map(|((_, name), _)| *name)
                    .collect();
                match names.as_slice() {
                    [one] => format!("every {one}"),
                    [rest @ .., last] => format!("every {} and {last}", rest.join(", ")),
                    [] => "never".to_owned(),
                }
            }
        };
        match self.at {
            Some((h, m)) => format!("{days} at {h:02}:{m:02}"),
            None if self.days == EVERY => "every hour".to_owned(),
            None => format!("{days}, every hour"),
        }
    }
}

/// The local minute `unix_secs` falls in, `offset_secs` from UTC: its day,
/// hour and minute, and its number since 1970, which changes once a minute.
pub fn local_minute(unix_secs: i64, offset_secs: i32) -> (Weekday, u8, u8, i64) {
    let local = unix_secs + i64::from(offset_secs);
    let at = time::OffsetDateTime::from_unix_timestamp(local)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    (at.weekday(), at.hour(), at.minute(), local.div_euclid(60))
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = match self.days {
            EVERY => "daily".to_owned(),
            WEEKDAYS => "weekdays".to_owned(),
            WEEKENDS => "weekends".to_owned(),
            days => DAYS
                .iter()
                .zip(days)
                .filter(|(_, on)| *on)
                .map(|((short, _), _)| *short)
                .collect::<Vec<_>>()
                .join(","),
        };
        match self.at {
            Some((h, m)) => write!(f, "{days} {h:02}:{m:02}"),
            None if self.days == EVERY => f.write_str("hourly"),
            None => write!(f, "{days} hourly"),
        }
    }
}

impl FromStr for Schedule {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let wrong = || format!("{text:?} isn't a schedule: say it like {FORMS}");
        let lower = text.trim().to_ascii_lowercase();
        let words: Vec<&str> = lower
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| !w.is_empty() && !matches!(*w, "every" | "at" | "on" | "and"))
            .collect();
        let (time, days) = match words.split_last() {
            Some((last, rest)) if last.contains(':') => (Some(*last), rest),
            Some((last, rest)) if matches!(*last, "hourly" | "hour") => (None, rest),
            _ => return Err(wrong()),
        };
        let at = match time {
            Some(time) => {
                let (h, m) = time.split_once(':').ok_or_else(wrong)?;
                let (h, m) = (h.parse::<u8>(), m.parse::<u8>());
                match (h, m) {
                    (Ok(h), Ok(m)) if h < 24 && m < 60 => Some((h, m)),
                    _ => return Err(wrong()),
                }
            }
            None => None,
        };
        let mut set = [false; 7];
        for word in days {
            let these = match *word {
                "day" | "days" | "daily" => EVERY,
                "weekday" | "weekdays" => WEEKDAYS,
                "weekend" | "weekends" => WEEKENDS,
                word => {
                    let day = DAYS
                        .iter()
                        .position(|(short, name)| {
                            let name = name.to_ascii_lowercase();
                            word == *short || word == name || word.strip_suffix('s') == Some(&name)
                        })
                        .ok_or_else(wrong)?;
                    let mut one = [false; 7];
                    one[day] = true;
                    one
                }
            };
            for (on, add) in set.iter_mut().zip(these) {
                *on |= add;
            }
        }
        if days.is_empty() {
            set = EVERY;
        }
        Ok(Self { days: set, at })
    }
}

impl TryFrom<String> for Schedule {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.parse()
    }
}

impl From<Schedule> for String {
    fn from(schedule: Schedule) -> Self {
        schedule.to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn da4_schedules_read_as_people_and_models_write_them() {
        for (text, canonical, words) in [
            ("weekdays 09:00", "weekdays 09:00", "every weekday at 09:00"),
            (
                "every weekday at 9:00",
                "weekdays 09:00",
                "every weekday at 09:00",
            ),
            ("daily 18:30", "daily 18:30", "every day at 18:30"),
            ("Every day at 7:05", "daily 07:05", "every day at 07:05"),
            (
                "mon,thu 08:30",
                "mon,thu 08:30",
                "every Monday and Thursday at 08:30",
            ),
            ("Mondays at 08:00", "mon 08:00", "every Monday at 08:00"),
            (
                "weekends 10:00",
                "weekends 10:00",
                "every Saturday and Sunday at 10:00",
            ),
            ("hourly", "hourly", "every hour"),
            ("every hour", "hourly", "every hour"),
        ] {
            let schedule: Schedule = text.parse().unwrap();
            assert_eq!(schedule.to_string(), canonical, "{text}");
            assert_eq!(schedule.describe(), words, "{text}");
            assert_eq!(canonical.parse::<Schedule>().unwrap(), schedule);
        }
        for wrong in [
            "",
            "soon",
            "weekdays",
            "25:00 daily",
            "daily 9:60",
            "fridy 09:00",
            "daily 9",
        ] {
            let err = wrong.parse::<Schedule>().unwrap_err();
            assert!(err.contains("isn't a schedule"), "{wrong}: {err}");
        }
    }

    #[test]
    fn da4_the_minute_is_the_computers_local_one() {
        // 2026-10-22 13:00:30 UTC, a Thursday: 09:00 in New York (-4h).
        let (day, hour, minute, n) = local_minute(1_792_674_030, -4 * 3600);
        assert_eq!((day, hour, minute), (Weekday::Thursday, 9, 0));
        let (_, _, _, later) = local_minute(1_792_674_030 + 29, -4 * 3600);
        assert_eq!(later, n, "the same minute");
        let (_, _, _, next) = local_minute(1_792_674_030 + 30, -4 * 3600);
        assert_eq!(next, n + 1);
        // Past midnight in Tokyo, it's Friday.
        assert_eq!(local_minute(1_792_674_030, 9 * 3600).0, Weekday::Thursday);
        assert_eq!(
            local_minute(1_792_674_030 + 11 * 3600, 9 * 3600).0,
            Weekday::Friday
        );
    }

    #[test]
    fn da4_a_schedule_is_due_in_its_minute_only() {
        let weekdays: Schedule = "weekdays 09:00".parse().unwrap();
        assert!(weekdays.due(Weekday::Thursday, 9, 0));
        assert!(!weekdays.due(Weekday::Thursday, 9, 1));
        assert!(!weekdays.due(Weekday::Saturday, 9, 0));
        let hourly: Schedule = "hourly".parse().unwrap();
        assert!(hourly.due(Weekday::Sunday, 3, 0));
        assert!(!hourly.due(Weekday::Sunday, 3, 30));
        // As it's kept: its text.
        let kept = serde_json::to_string(&weekdays).unwrap();
        assert_eq!(kept, r#""weekdays 09:00""#);
        assert_eq!(serde_json::from_str::<Schedule>(&kept).unwrap(), weekdays);
        assert!(serde_json::from_str::<Schedule>(r#""whenever""#).is_err());
    }
}
