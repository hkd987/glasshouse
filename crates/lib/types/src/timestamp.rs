use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// A UTC instant, serialized as RFC 3339 so audit logs stay readable by
/// auditors and SIEM tools without a schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(#[serde(with = "time::serde::rfc3339")] OffsetDateTime);

impl Timestamp {
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    pub fn from_unix_seconds(secs: i64) -> Option<Self> {
        OffsetDateTime::from_unix_timestamp(secs).ok().map(Self)
    }

    pub fn unix_seconds(&self) -> i64 {
        self.0.unix_timestamp()
    }

    /// Milliseconds since the epoch: a cursor finer than a second.
    pub fn unix_millis(&self) -> i64 {
        i64::try_from(self.0.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
    }

    /// Time of day, `HH:MM:SS`, on a clock `offset_secs` east of UTC: the
    /// computer's own clock, given its offset. Anything a person reads says
    /// the time their clock says, so there is no form of this without an
    /// offset (an instant's UTC time of day is hours off for most people).
    pub fn clock_at(&self, offset_secs: i32) -> String {
        let local = time::UtcOffset::from_whole_seconds(offset_secs)
            .map_or(self.0, |offset| self.0.to_offset(offset));
        format!(
            "{:02}:{:02}:{:02}",
            local.hour(),
            local.minute(),
            local.second()
        )
    }

    /// The date, `YYYY-MM-DD`, at `offset_secs` east of UTC: the computer's
    /// own day, given its offset.
    pub fn date_at(&self, offset_secs: i32) -> String {
        let local = time::UtcOffset::from_whole_seconds(offset_secs)
            .map_or(self.0, |offset| self.0.to_offset(offset));
        format!(
            "{:04}-{:02}-{:02}",
            local.year(),
            u8::from(local.month()),
            local.day()
        )
    }

    /// Whole seconds elapsed from `earlier` to `self` (zero if negative).
    pub fn seconds_since(&self, earlier: Timestamp) -> u64 {
        u64::try_from((self.0 - earlier.0).whole_seconds()).unwrap_or(0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.format(&Rfc3339) {
            Ok(s) => f.write_str(&s),
            Err(_) => write!(f, "{}", self.0.unix_timestamp()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_rfc3339() {
        let ts = Timestamp::from_unix_seconds(1_792_300_000).unwrap();
        let json = serde_json::to_string(&ts).unwrap();
        assert_eq!(json, "\"2026-10-18T05:06:40Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), ts);
        assert_eq!(ts.clock_at(0), "05:06:40");
    }

    #[test]
    fn the_time_of_day_is_the_computers_not_utcs() {
        let ts = Timestamp::from_unix_seconds(1_792_300_000).unwrap();
        // Chicago in winter, six hours behind: still the evening before.
        assert_eq!(ts.clock_at(-6 * 3600), "23:06:40");
        // India, five and a half hours ahead.
        assert_eq!(ts.clock_at(5 * 3600 + 1800), "10:36:40");
        // An offset no clock has is read as UTC, not a panic.
        assert_eq!(ts.clock_at(i32::MAX), "05:06:40");
    }

    #[test]
    fn elapsed_seconds_never_negative() {
        let a = Timestamp::from_unix_seconds(100).unwrap();
        let b = Timestamp::from_unix_seconds(160).unwrap();
        assert_eq!(b.seconds_since(a), 60);
        assert_eq!(a.seconds_since(b), 0);
    }
}
