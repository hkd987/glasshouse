//! A person's device settings (UX-1, ADR 0029), as the desktop applies
//! them: `settings.get` on the control socket, and the `settings` app's
//! actions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who decided a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingFrom {
    /// IT, locked or as a default.
    It,
    /// The person, for a setting IT left to them.
    You,
    /// Nobody: the computer's own default.
    Default,
}

/// One setting as the caller's computer applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingView {
    /// Such as `clock.hour24`.
    pub id: String,
    /// `true`, a number, text or a list of text, as the setting takes.
    pub value: Value,
    /// IT locked it.
    #[serde(default)]
    pub locked: bool,
    pub from: SettingFrom,
}

/// The result of `settings.get`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    pub settings: Vec<SettingView>,
    /// The computer's offset from UTC now, in seconds east (ADR 0031): the
    /// desktop's clock follows a new time zone without signing out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_secs: Option<i32>,
}

impl SettingsView {
    fn value(&self, id: &str) -> Option<&Value> {
        self.settings.iter().find(|s| s.id == id).map(|s| &s.value)
    }

    /// A toggle's value, or `default` when it isn't there.
    pub fn flag(&self, id: &str, default: bool) -> bool {
        self.value(id).and_then(Value::as_bool).unwrap_or(default)
    }

    /// A number's value, or `default` when it isn't there.
    pub fn number(&self, id: &str, default: i64) -> i64 {
        self.value(id).and_then(Value::as_i64).unwrap_or(default)
    }
}
