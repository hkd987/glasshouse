//! Agent computer hours for AWS Marketplace (ADR 0066): each minute glassd
//! counts the computers awake on this host; each hour it reports the
//! hours they made to the Metering Service, signed with the host's
//! instance role, so the buyer's AWS bill carries them. What it counted
//! is kept on disk, so a restart loses no minutes, and an hour that
//! couldn't be sent is tried again for as long as AWS takes it (six hours).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How late AWS takes an hour's report.
pub const LATE_HOURS: i64 = 6;

/// `[metering]`, written by the stack (deploy/aws).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeteringConfig {
    #[serde(default)]
    pub enabled: bool,
    /// The listing's product code.
    #[serde(default)]
    pub product_code: String,
    /// Always `agent_computer_hours`.
    #[serde(default = "dimension")]
    pub dimension: String,
    pub region: String,
    /// The Metering Service's address, for a VPC endpoint or a test.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Check that this host may meter, and report nothing.
    #[serde(default)]
    pub dry_run: bool,
}

fn dimension() -> String {
    "agent_computer_hours".into()
}

/// Hours to report: each one's start, and its computer hours.
pub type Hours = Vec<(i64, u32)>;

/// What was counted and not yet reported, kept on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meter {
    /// The hour being counted, in seconds since the epoch at its start.
    pub hour: i64,
    /// Computer-minutes counted in it.
    pub minutes: u64,
    /// Hours ended and not yet reported: their start and computer hours.
    #[serde(default)]
    pub unsent: Hours,
}

fn hour_of(at: OffsetDateTime) -> i64 {
    let secs = at.unix_timestamp();
    secs - secs.rem_euclid(3600)
}

impl Meter {
    pub fn load(path: &PathBuf) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        let making = path.with_extension("json.new");
        std::fs::write(&making, serde_json::to_vec(self).unwrap_or_default())?;
        std::fs::rename(&making, path)
    }

    /// A minute with `awake` computers on this host. An hour that ended
    /// becomes one to report: its computer-minutes rounded up to hours,
    /// as AWS counts whole ones. An hour with none is not reported.
    pub fn tick(&mut self, now: OffsetDateTime, awake: u32) {
        let hour = hour_of(now);
        if self.hour != hour {
            if self.hour != 0 && self.minutes > 0 {
                let hours = self.minutes.div_ceil(60);
                self.unsent
                    .push((self.hour, u32::try_from(hours).unwrap_or(u32::MAX)));
            }
            self.hour = hour;
            self.minutes = 0;
        }
        self.minutes += u64::from(awake);
    }

    /// The hours to report now, oldest first, and those too late to, which
    /// are dropped.
    pub fn due(&mut self, now: OffsetDateTime) -> (Hours, Hours) {
        let oldest = hour_of(now) - LATE_HOURS * 3600;
        let (late, due): (Vec<_>, Vec<_>) =
            self.unsent.drain(..).partition(|(hour, _)| *hour < oldest);
        (due, late)
    }

    /// An hour that couldn't be sent, to try again.
    pub fn retry(&mut self, hour: i64, quantity: u32) {
        self.unsent.push((hour, quantity));
        self.unsent.sort_unstable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: i64, minute: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(
            1_761_000_000 - 1_761_000_000 % 3600 + hour * 3600 + minute * 60,
        )
        .unwrap()
    }

    #[test]
    fn adr0066_computer_minutes_become_whole_hours_per_hour() {
        let mut meter = Meter::default();
        // 3 computers for 30 minutes, then 1 for 30: 120 minutes, 2 hours.
        for minute in 0..30 {
            meter.tick(at(0, minute), 3);
        }
        for minute in 30..60 {
            meter.tick(at(0, minute), 1);
        }
        assert!(meter.unsent.is_empty());
        // The next hour starts: the last is due. 61 minutes would be 2.
        meter.tick(at(1, 0), 0);
        assert_eq!(meter.unsent, [(hour_of(at(0, 0)), 2)]);
        // An hour with nothing awake reports nothing.
        meter.tick(at(2, 0), 1);
        assert_eq!(meter.unsent.len(), 1);
        meter.tick(at(3, 0), 0);
        assert_eq!(meter.unsent.len(), 2);
        assert_eq!(meter.unsent[1], (hour_of(at(2, 0)), 1));
    }

    #[test]
    fn adr0066_unsent_hours_are_tried_until_aws_wont_take_them() {
        let mut meter = Meter {
            unsent: vec![(hour_of(at(0, 0)), 4), (hour_of(at(5, 0)), 2)],
            ..Meter::default()
        };
        let (due, late) = meter.due(at(7, 10));
        assert_eq!(late, [(hour_of(at(0, 0)), 4)]);
        assert_eq!(due, [(hour_of(at(5, 0)), 2)]);
        meter.retry(due[0].0, due[0].1);
        assert_eq!(meter.unsent.len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metering.json");
        meter.save(&path).unwrap();
        assert_eq!(Meter::load(&path), meter);
    }
}
