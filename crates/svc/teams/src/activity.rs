//! What an activity from Teams asks, once [`crate::verify`] has let it in.
//!
//! Followed: the Bot Connector's Activity schema
//! (<https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference>),
//! Teams' conversation events
//! (<https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/subscribe-to-conversation-events>),
//! mentions in Teams
//! (<https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/channel-and-group-conversations>)
//! and the `adaptiveCard/action` invoke
//! (<https://learn.microsoft.com/en-us/adaptive-cards/authoring-cards/universal-action-model>).
//!
//! Who did something is their Entra object id (`aadObjectId`), which
//! Microsoft sets; a name is for showing only. From a card's answer only the
//! verb and the approval id are read (ADR 0065 §13).

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::cards::{VERB_APPROVE, VERB_DENY, VERB_REFRESH};

/// An activity, with the fields the relay reads. Unknown fields are
/// ignored; a missing or `null` field is empty.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// `message`, `invoke`, `conversationUpdate`, `installationUpdate`, …
    #[serde(rename = "type", default, deserialize_with = "nullable")]
    pub kind: String,
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    /// An invoke's or an event's name: `adaptiveCard/action`.
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// Where to answer: the Bot Connector for this conversation.
    #[serde(default, deserialize_with = "nullable")]
    pub service_url: String,
    /// `msteams`.
    #[serde(default, deserialize_with = "nullable")]
    pub channel_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub from: Account,
    #[serde(default, deserialize_with = "nullable")]
    pub recipient: Account,
    #[serde(default, deserialize_with = "nullable")]
    pub conversation: ConversationAccount,
    #[serde(default, deserialize_with = "nullable")]
    pub text: String,
    #[serde(default, deserialize_with = "nullable")]
    pub entities: Vec<Value>,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub channel_data: Value,
    /// The message this one answers: for a card's answer, the card.
    #[serde(default, deserialize_with = "nullable")]
    pub reply_to_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub members_added: Vec<Account>,
    #[serde(default, deserialize_with = "nullable")]
    pub members_removed: Vec<Account>,
    /// `installationUpdate`: `add`, `remove`, `add-upgrade`, `remove-upgrade`.
    #[serde(default, deserialize_with = "nullable")]
    pub action: String,
}

/// A person or a bot in Teams.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// A person's is `29:…`, unique to this app; a bot's is `28:<app id>`.
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// A person's Entra object id.
    #[serde(default, deserialize_with = "nullable")]
    pub aad_object_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAccount {
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    /// `personal`, `groupChat` or `channel`.
    #[serde(default, deserialize_with = "nullable")]
    pub conversation_type: String,
    #[serde(default, deserialize_with = "nullable")]
    pub tenant_id: String,
}

impl Activity {
    /// The tenant it came from: the conversation's, or `channelData`'s.
    /// `None` if neither says, or they disagree.
    pub fn tenant(&self) -> Option<&str> {
        let conversation = Some(self.conversation.tenant_id.as_str()).filter(|t| !t.is_empty());
        let data = self.channel_data["tenant"]["id"]
            .as_str()
            .filter(|t| !t.is_empty());
        match (conversation, data) {
            (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => None,
            (a, b) => a.or(b),
        }
    }

    fn scope(&self) -> Option<Scope> {
        match self.conversation.conversation_type.as_str() {
            "personal" => Some(Scope::Personal),
            "groupChat" => Some(Scope::GroupChat),
            "channel" => Some(Scope::Channel),
            _ => None,
        }
    }

    /// Where to answer it, with its sender as the user.
    fn reference(&self) -> Option<ConversationRef> {
        Some(ConversationRef {
            service_url: self.service_url.clone(),
            conversation_id: self.conversation.id.clone(),
            tenant_id: self.tenant()?.to_owned(),
            bot_id: self.recipient.id.clone(),
            user_id: self.from.id.clone(),
            user_aad_object_id: self.from.aad_object_id.clone(),
            scope: self.scope()?,
        })
        .filter(|r| {
            ![&r.service_url, &r.conversation_id, &r.bot_id, &r.user_id]
                .iter()
                .any(|s| s.is_empty())
        })
    }

    /// Who sent it, if Microsoft said who they are in Entra.
    fn who(&self) -> Option<Who> {
        (!self.from.aad_object_id.is_empty() && !self.from.id.is_empty()).then(|| Who {
            aad_object_id: self.from.aad_object_id.clone(),
            teams_id: self.from.id.clone(),
            name: self.from.name.clone(),
        })
    }
}

/// Where to reach a person in Teams later (proactive messages): keep one
/// per person, from their [`Incoming::Installed`], and send to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRef {
    /// The Bot Connector for this conversation (`https://smba.trafficmanager.net/amer/`).
    pub service_url: String,
    pub conversation_id: String,
    pub tenant_id: String,
    /// The app in the conversation: `28:<app id>`.
    pub bot_id: String,
    /// The person's Teams id (`29:…`), for a card's refresh list.
    pub user_id: String,
    /// The person's Entra object id: who they are.
    pub user_aad_object_id: String,
    pub scope: Scope,
}

/// What kind of conversation: approvals go only to a person's own chat with
/// the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Personal,
    GroupChat,
    Channel,
}

/// Who did something in Teams, as Microsoft said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    /// Their Entra object id: match people on this, never on the name.
    pub aad_object_id: String,
    /// Their Teams id (`29:…`).
    pub teams_id: String,
    /// Their name as Teams shows it, for display only.
    pub name: String,
}

/// What an activity asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Someone wrote to the app, maybe naming an agent.
    Message(MessageToAgent),
    /// Someone pressed Approve or Don't on a card.
    Answer(CardAnswer),
    /// Teams asks for the card's current state, as someone looks at it
    /// (its `refresh`): answer the invoke with the card for them.
    Refresh(CardRefresh),
    /// The app was installed for a person: keep this to reach them later.
    Installed(ConversationRef),
    /// The person removed the app: forget their conversation.
    Uninstalled(ConversationRef),
    /// Anything else: answer 200 and do nothing.
    Other {
        kind: String,
        /// Why it isn't one of the others, for the log.
        why: String,
    },
}

/// A message to an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageToAgent {
    pub from: Who,
    /// Where to answer.
    pub conversation: ConversationRef,
    /// The message's id, to answer it in a thread.
    pub activity_id: String,
    /// What they wrote, without the mention of the app: after `@Agent`
    /// when [`MessageToAgent::agent`] is set.
    pub text: String,
    /// The agent they named with a leading `@Reconciler`, as written.
    /// [`MessageToAgent::addressed_to`] also finds a leading name.
    pub agent: Option<String>,
    /// What they wrote, without the mention of the app, in full.
    pub full_text: String,
}

/// Approve or Don't, pressed on a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardAnswer {
    /// The approval's id, from the card's data: look it up, trust nothing
    /// else about it from Teams.
    pub approval: String,
    pub yes: bool,
    /// Who pressed it: the front checks they're the person the approval is
    /// for, or a delegate.
    pub by: Who,
    pub conversation: ConversationRef,
    /// The card's message, to update for everyone.
    pub card_activity_id: String,
}

/// A card asking for its current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRefresh {
    pub approval: String,
    /// Who is looking: they see buttons only if they may answer.
    pub by: Who,
    pub conversation: ConversationRef,
}

/// What `activity` asks.
pub fn interpret(activity: &Activity) -> Incoming {
    let other = |why: &str| Incoming::Other {
        kind: activity.kind.clone(),
        why: why.to_owned(),
    };
    match activity.kind.as_str() {
        "message" => message(activity).unwrap_or_else(&other),
        "invoke" if activity.name == "adaptiveCard/action" => {
            card_action(activity).unwrap_or_else(&other)
        }
        "invoke" => other("an invoke the app doesn't handle"),
        "installationUpdate" | "conversationUpdate" => {
            installation(activity).unwrap_or_else(&other)
        }
        _ => other("an activity the app doesn't handle"),
    }
}

fn message(activity: &Activity) -> Result<Incoming, &'static str> {
    let from = activity.who().ok_or(NO_SENDER)?;
    let conversation = activity.reference().ok_or(NO_CONVERSATION)?;
    let full_text = without_mention(activity);
    if full_text.is_empty() {
        return Err("a message with no text");
    }
    let (agent, text) = match named_agent(&full_text) {
        Some((agent, rest)) => (Some(agent.to_owned()), rest.to_owned()),
        None => (None, full_text.clone()),
    };
    Ok(Incoming::Message(MessageToAgent {
        from,
        conversation,
        activity_id: activity.id.clone(),
        text,
        agent,
        full_text,
    }))
}

const NO_SENDER: &str = "Teams didn't say who sent it (no Entra object id)";
const NO_CONVERSATION: &str = "it doesn't say which conversation, tenant or app it's in";

/// The text, without the mentions of this app (`<at>Glasshouse Agents</at>`).
fn without_mention(activity: &Activity) -> String {
    let mut text = activity.text.clone();
    for entity in &activity.entities {
        if entity["type"].as_str() == Some("mention")
            && entity["mentioned"]["id"].as_str() == Some(activity.recipient.id.as_str())
            && let Some(said) = entity["text"].as_str().filter(|s| !s.is_empty())
        {
            text = text.replace(said, "");
        }
    }
    text.replace("&nbsp;", " ")
        .replace('\u{a0}', " ")
        .trim()
        .to_owned()
}

/// A leading `@Name`, and what follows it.
fn named_agent(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let (name, after) = rest.split_at(end);
    (!name.is_empty()).then(|| (name, after.trim_start_matches([',', ':']).trim()))
}

impl MessageToAgent {
    /// Which of `names` (the person's agents) the message is for, by a
    /// leading `@Name` or `Name` (any case, followed by a space, `,` or `:`),
    /// and what it says to them. Longer names win: "Invoice Checker" over
    /// "Invoice".
    pub fn addressed_to(&self, names: &[&str]) -> Option<(usize, String)> {
        let text = self.full_text.strip_prefix('@').unwrap_or(&self.full_text);
        let mut order: Vec<usize> = (0..names.len()).filter(|i| !names[*i].is_empty()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(names[*i].len()));
        order.into_iter().find_map(|i| {
            let name = names[i];
            let (head, rest) = (text.get(..name.len())?, text.get(name.len()..)?);
            let ends = rest.is_empty() || rest.starts_with([' ', ',', ':', '\n']);
            if !ends || head.to_lowercase() != name.to_lowercase() {
                return None;
            }
            Some((i, rest.trim_start_matches([',', ':']).trim().to_owned()))
        })
    }
}

/// An `adaptiveCard/action` invoke: `value.action` is `{type, verb, data}`.
fn card_action(activity: &Activity) -> Result<Incoming, &'static str> {
    let action = &activity.value["action"];
    if action["type"].as_str() != Some("Action.Execute") {
        return Err("a card action that isn't Action.Execute");
    }
    let approval = action["data"]["approval"]
        .as_str()
        .filter(|id| valid_approval_id(id))
        .ok_or("a card action without an approval id")?
        .to_owned();
    let by = activity.who().ok_or(NO_SENDER)?;
    let conversation = activity.reference().ok_or(NO_CONVERSATION)?;
    let yes = match action["verb"].as_str() {
        Some(VERB_APPROVE) => true,
        Some(VERB_DENY) => false,
        Some(VERB_REFRESH) => {
            return Ok(Incoming::Refresh(CardRefresh {
                approval,
                by,
                conversation,
            }));
        }
        _ => return Err("a card action with a verb the app doesn't know"),
    };
    Ok(Incoming::Answer(CardAnswer {
        approval,
        yes,
        by,
        conversation,
        card_activity_id: activity.reply_to_id.clone(),
    }))
}

/// Whether `id` could be an approval's: 1 to 128 of letters, digits and
/// `-_.:`. The front looks it up; this keeps junk out of its logs.
fn valid_approval_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// The app added to or removed from a person's own chat. Installs in a
/// team or a group chat aren't a person's, so they're never kept to send
/// approvals to.
fn installation(activity: &Activity) -> Result<Incoming, &'static str> {
    let bot = activity.recipient.id.as_str();
    let (added, removed) = if activity.kind == "installationUpdate" {
        (
            matches!(activity.action.as_str(), "add" | "add-upgrade"),
            matches!(activity.action.as_str(), "remove" | "remove-upgrade"),
        )
    } else {
        let names_bot =
            |members: &[Account]| !bot.is_empty() && members.iter().any(|m| m.id == bot);
        (
            names_bot(&activity.members_added),
            names_bot(&activity.members_removed),
        )
    };
    if !added && !removed {
        return Err("a change to the conversation that isn't the app's install");
    }
    if activity.scope() != Some(Scope::Personal) {
        return Err("the app added to or removed from a team or group chat, not a person's chat");
    }
    activity.who().ok_or(NO_SENDER)?;
    let conversation = activity.reference().ok_or(NO_CONVERSATION)?;
    Ok(if added {
        Incoming::Installed(conversation)
    } else {
        Incoming::Uninstalled(conversation)
    })
}

/// Reads `null` as the type's default, as Bot Framework JSON has it.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
