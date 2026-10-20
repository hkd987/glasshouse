//! The policy document IT writes, as stored on disk (`policy.toml`).
//!
//! These types mirror the file one to one. Validation and evaluation live in
//! [`crate::Policy`]; nothing here has behavior beyond parsing.

use std::collections::BTreeMap;

use glass_types::{AgentId, Label, Tier};
use serde::{Deserialize, Serialize};

use crate::ActionPattern;

/// The current (and only) policy schema version.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    pub version: u32,
    #[serde(default)]
    pub org: OrgDefaults,
    #[serde(default, rename = "group")]
    pub groups: Vec<GroupPolicy>,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentPolicy>,
    /// Tiers for one app's own sessions (ADR 0030): what the person does in
    /// that app's window, such as a message they typed in Chat.
    #[serde(default, rename = "app", skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<AppPolicy>,
    /// Folder rules that label data by location (DP-10).
    #[serde(default, rename = "label")]
    pub labels: Vec<FolderLabel>,
    /// Plain-language names for rules, shown to users ("Payments", "Vendor
    /// bank details") where no installed app names the action itself.
    #[serde(default)]
    pub names: BTreeMap<ActionPattern, String>,
    /// What the IT self-service agent may fix (AE-4).
    #[serde(default)]
    pub fixes: Fixes,
    /// The approved models and where each label's data may go (DP-2,
    /// ADR 0018).
    #[serde(default)]
    pub models: Models,
    /// The org's mail service, so Mail is set up with no steps on every
    /// device (NV-3). Never credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail: Option<MailSettings>,
    /// Remote help's tiers (CF-81, ADR 0028): which directory groups may
    /// help people, with which tools.
    #[serde(default, skip_serializing_if = "Support::is_empty")]
    pub support: Support,
}

/// Remote help (CF-80, CF-81, ADR 0028): the roles support staff sign in
/// to the console with, each a set of tools for some people's computers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Support {
    /// How long the person has to answer a request to help them.
    #[serde(default = "default_answer_within_secs")]
    pub answer_within_secs: u64,
    #[serde(default, rename = "role")]
    pub roles: Vec<SupportRole>,
}

impl Default for Support {
    fn default() -> Self {
        Self {
            answer_within_secs: default_answer_within_secs(),
            roles: Vec::new(),
        }
    }
}

impl Support {
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.answer_within_secs == default_answer_within_secs()
    }
}

fn default_answer_within_secs() -> u64 {
    120
}

/// A support tier, such as the help desk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportRole {
    /// `help-desk`.
    pub id: String,
    /// What IT calls it: "IT Help Desk".
    pub name: String,
    /// The directory groups whose members hold it.
    pub groups: Vec<String>,
    pub tools: Vec<SupportTool>,
    /// The policy groups whose computers it may help with.
    #[serde(default = "everyone")]
    pub people: Vec<String>,
    /// Device types it may reach with nobody there to answer; none by
    /// default.
    #[serde(default)]
    pub unattended: Vec<String>,
    /// The longest a session may last.
    #[serde(default = "default_max_minutes")]
    pub max_minutes: u32,
}

fn everyone() -> Vec<String> {
    vec![crate::EVERYONE.to_owned()]
}

fn default_max_minutes() -> u32 {
    60
}

/// What a support role may do (CF-81). A closed list: new tools come with
/// the code that carries them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportTool {
    /// See the person's screen, once they allow it.
    View,
    /// Use their mouse and keyboard, once they allow that too.
    Control,
    InstallApps,
    CollectLogs,
    RestartServices,
    /// Cut a computer off the network except from IT.
    Isolate,
    Forensics,
}

impl SupportTool {
    pub const ALL: [SupportTool; 7] = [
        Self::View,
        Self::Control,
        Self::InstallApps,
        Self::CollectLogs,
        Self::RestartServices,
        Self::Isolate,
        Self::Forensics,
    ];

    /// What IT reads.
    pub fn title(self) -> &'static str {
        match self {
            Self::View => "View the screen",
            Self::Control => "Control with consent",
            Self::InstallApps => "Install approved apps",
            Self::CollectLogs => "Collect logs",
            Self::RestartServices => "Restart services",
            Self::Isolate => "Isolate the computer",
            Self::Forensics => "Collect forensic data",
        }
    }
}

/// Where the org's mail is (NV-3): set by IT in the console, reaching
/// devices with policy. The person's own address comes from the org's
/// directory; `address_template` stands in when the directory has none.
/// Credentials are never in policy: a device's mail secrets stay on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailSettings {
    pub service: MailService,
    /// The person's address from their login, such as
    /// `{login}@acme.example`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_template: Option<String>,
    /// The Microsoft 365 tenant or Google Workspace domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Where mail is read from (IMAP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming: Option<MailServer>,
    /// Where mail is sent through (SMTP submission).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<MailServer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MailService {
    /// Any IMAP and SMTP service.
    Imap,
    #[serde(rename = "microsoft-365")]
    Microsoft365,
    GoogleWorkspace,
}

impl MailService {
    /// What people call it.
    pub fn title(self) -> &'static str {
        match self {
            Self::Imap => "IMAP and SMTP",
            Self::Microsoft365 => "Microsoft 365",
            Self::GoogleWorkspace => "Google Workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailServer {
    pub host: String,
    pub port: u16,
    pub security: MailSecurity,
}

/// How the connection is protected; there is no plain option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailSecurity {
    /// TLS from the first byte (IMAPS 993, SMTPS 465).
    Tls,
    /// Plain, then STARTTLS before anything else (submission 587).
    Starttls,
}

/// IT's model allowlist and routing (DP-2, DP-3, DP-5, MP-8). Empty means no
/// agent can call a model, which is what a new org ships with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// The model calls go to when a label has no route of its own. Public and
    /// internal data only: confidential data needs an explicit route.
    #[serde(default)]
    pub default: Option<String>,
    /// The label of content no rule labels, for routing.
    #[serde(default = "default_unlabeled")]
    pub unlabeled: Label,
    /// The most a call may ask the model to write.
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    /// Keep the (redacted) prompt and reply text in the audit log. Off by
    /// default: records carry sizes and SHA-256 hashes only (DP-5).
    #[serde(default)]
    pub log_prompts: bool,
    /// Where each label's data may go: model ids in order. The first is
    /// used; the rest take over on an outage or rate limit (MP-8).
    /// Restricted data never goes to a model.
    #[serde(default)]
    pub routes: BTreeMap<Label, Vec<String>>,
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderEntry>,
    /// The allowlist: every model an agent may be routed to.
    #[serde(default, rename = "model")]
    pub models: Vec<ModelEntry>,
    /// What is replaced before anything leaves (DP-3). Secrets are always
    /// redacted; these add PII.
    #[serde(default)]
    pub redact: Redaction,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            default: None,
            unlabeled: default_unlabeled(),
            max_output_tokens: default_max_output_tokens(),
            log_prompts: false,
            routes: BTreeMap::new(),
            providers: Vec::new(),
            models: Vec::new(),
            redact: Redaction::default(),
        }
    }
}

fn default_unlabeled() -> Label {
    Label::Internal
}

fn default_max_output_tokens() -> u32 {
    4096
}

/// A model service glassd may call (MP-1 … MP-4). Its credentials live in
/// the secrets file under the same id, never here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub id: String,
    pub kind: ProviderKind,
    /// As users see it: "Amazon Bedrock".
    pub name: String,
    /// The AWS region Bedrock calls are pinned to (MP-2).
    #[serde(default)]
    pub region: Option<String>,
    /// Another endpoint than the provider's public one, such as a PrivateLink
    /// endpoint (MP-2). HTTPS only.
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Amazon Bedrock in the org's AWS account (MP-1, the PRD's default).
    Bedrock,
    /// The Anthropic API with an org or team key (MP-4).
    Anthropic,
    /// OpenRouter, with a key: the org's model credits bought from us, or
    /// an individual's own (ADR 0039). Every call asks for providers that
    /// keep no data and don't train on it.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Canned, deterministic replies made on this machine; nothing leaves.
    /// For development and tests.
    Scripted,
}

/// One approved model (DP-2, MP-9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// The name routes, agents and group lists use.
    pub id: String,
    /// The [`ProviderEntry`] it runs on.
    pub provider: String,
    /// The provider's own model id.
    pub model: String,
    /// As users see it on the handoff card: "Claude Sonnet".
    pub name: String,
    /// Price per million input tokens, for the cost in each audit record.
    #[serde(default)]
    pub usd_per_mtok_in: Option<f64>,
    #[serde(default)]
    pub usd_per_mtok_out: Option<f64>,
}

/// PII redaction on top of the built-in secret detectors (DP-3).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redaction {
    /// Built-in PII detectors to turn on.
    #[serde(default)]
    pub pii: Vec<PiiKind>,
    /// IT's own patterns, such as employee ids.
    #[serde(default, rename = "pattern")]
    pub patterns: Vec<RedactPattern>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PiiKind {
    Email,
    /// US Social Security numbers written `123-45-6789`.
    UsSsn,
    /// Payment card numbers that pass the Luhn check.
    CardNumber,
}

/// A configured PII shape. In `pattern`, `#` is a digit, `@` a letter, `?` a
/// letter or digit, and `\` makes the next character literal; everything else
/// matches itself. A match must not run on into more letters or digits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactPattern {
    /// Names the placeholder: `employee-id` gives `[redacted:employee-id#1]`.
    pub name: String,
    pub pattern: String,
}

/// IT's approved fix list (AE-4). Checks and cache cleanup need no list;
/// anything that changes how the machine runs does.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixes {
    /// rc.d services an agent may restart, such as the VPN client.
    #[serde(default)]
    pub services: Vec<String>,
}

/// The org-wide baseline every group and agent rule overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgDefaults {
    /// Highest data label any agent may open unless a rule lowers it.
    #[serde(default = "default_max_label")]
    pub max_label: Label,
    /// How long an unanswered Ask holds the agent before the task pauses
    /// (DA-13).
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
    /// Baseline tier per action pattern. Actions with no matching rule use
    /// the default tier their app declared.
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
}

impl Default for OrgDefaults {
    fn default() -> Self {
        Self {
            max_label: default_max_label(),
            approval_timeout_secs: default_approval_timeout_secs(),
            tiers: BTreeMap::new(),
        }
    }
}

fn default_max_label() -> Label {
    Label::Internal
}

fn default_approval_timeout_secs() -> u64 {
    15 * 60
}

/// Rules for a directory group. In managed fleets groups and memberships are
/// synced from Entra ID or Okta via SCIM (AD-1); `members` is the local
/// fallback used before enrollment and in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPolicy {
    pub name: String,
    /// Login names. The special group name `everyone` matches all users.
    #[serde(default)]
    pub members: Vec<String>,
    /// Agents members of this group may run.
    #[serde(default)]
    pub agents: Vec<AgentId>,
    /// Apps in members' profiles (AD-7, role-based delivery): the launcher offers
    /// them and glassd opens them for members only, so IT's tools reach the
    /// people whose role grants them and nobody else. Web apps by their
    /// manifest id, native apps such as `activity-monitor`, and `console`,
    /// the admin console. Apps and agents share one namespace.
    #[serde(default)]
    pub apps: Vec<AgentId>,
    #[serde(default)]
    pub max_label: Option<Label>,
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
    /// Models members' agents may use, such as Bedrock only for a group that
    /// handles confidential data (MP-6). Unset means the org's list.
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// Device settings for members (UX-1), by id from
    /// [`crate::settings::table`]: a default they may change, or locked.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, Managed>,
}

/// A setting IT set for a group (UX-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Managed {
    pub value: SettingValue,
    /// The person can't change it; otherwise it is their default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

/// A setting's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Number(i64),
    Text(String),
    List(Vec<String>),
}

/// Tiers for one app's sessions (ADR 0030). They are the session's own
/// rules, as an agent's are, so a group's stricter tier still wins; the
/// app's manifest still says what it may call at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPolicy {
    pub id: AgentId,
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
}

/// Rules for one catalog agent (AD-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    pub id: AgentId,
    /// Actions this agent may call at all. Anything else is Never and shows
    /// under "Can't touch".
    pub allowed_actions: Vec<ActionPattern>,
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
    #[serde(default)]
    pub max_label: Option<Label>,
    /// Network destinations (`host:port`) the agent's sandbox may reach.
    /// Empty means no network at all.
    #[serde(default)]
    pub egress: Vec<String>,
    /// Models this agent may use (AD-2). Unset means the org's list; groups
    /// and this list only ever narrow it.
    #[serde(default)]
    pub models: Option<Vec<String>>,
}

/// Labels everything under a path (DP-10). `*` matches exactly one path
/// segment, so `/home/*/Finance` covers every user's Finance folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderLabel {
    pub path: String,
    pub label: Label,
}
