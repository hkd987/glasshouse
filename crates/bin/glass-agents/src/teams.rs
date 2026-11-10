//! Agents in Microsoft Teams (ADR 0065 §13), the front's side: Microsoft's
//! calls at `POST /api/teams/messages`, the person's approvals sent to
//! their chat with the app as cards, and what they asked from Teams
//! answered there. `glass_teams` decides; `glass_gateway_providers::teams`
//! reaches Microsoft.
//!
//! Who may answer: the person the front acts for, by their Entra object id
//! from Microsoft's signed activity, never by a name.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use glass_agents_api as api;
use glass_gateway_providers::teams::{BotConnector, BotKeys, Endpoints};
use glass_http::{Request, Response};
use glass_teams::{BotAuth, CardContext, ConversationRef, Incoming, Jwks};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::server::Front;

/// How often the front looks for approvals to send and cards to update.
const LOOK_EVERY: Duration = Duration::from_secs(3);

/// `--teams <file>`: the org's app registration and the person's Entra id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamsConfig {
    /// The bot's Microsoft App ID.
    pub app_id: String,
    /// The org's Entra tenant.
    pub tenant_id: String,
    /// A file holding the app's client secret, readable only by the front.
    pub secret_file: PathBuf,
    /// The Entra object id of the person this front acts for.
    pub person_aad_object_id: String,
    /// The web app as people reach it: cards' Open goes here.
    pub open_url: String,
    /// Where the front keeps the person's conversation with the app.
    pub state_file: PathBuf,
    /// Microsoft's endpoints, for tests' stand-ins.
    #[serde(default)]
    pub endpoints: Option<EndpointsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointsConfig {
    pub openid_metadata: String,
    pub login: String,
    pub service_hosts: Vec<String>,
    #[serde(default)]
    pub allow_loopback_http: bool,
}

/// What the front remembers between calls.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    /// The person's own chat with the app.
    conversation: Option<ConversationRef>,
    /// Cards sent, by approval: its message, and the approval as sent.
    #[serde(default)]
    sent: BTreeMap<String, Sent>,
    /// Tasks asked from Teams, by task: where to answer.
    #[serde(default)]
    asked: BTreeMap<String, Asked>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sent {
    activity: String,
    agent: String,
    approval: api::ApprovalView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Asked {
    agent: String,
    conversation: ConversationRef,
}

pub struct Teams {
    config: TeamsConfig,
    auth: BotAuth,
    keys: Arc<BotKeys>,
    connector: Arc<BotConnector>,
    kept: Mutex<Kept>,
}

impl Teams {
    pub fn new(config: TeamsConfig) -> anyhow::Result<Self> {
        let secret = std::fs::read_to_string(&config.secret_file)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", config.secret_file.display()))?;
        let endpoints = match &config.endpoints {
            Some(e) => Endpoints {
                openid_metadata: e.openid_metadata.clone(),
                login: e.login.clone(),
                service_hosts: e.service_hosts.clone(),
                allow_loopback_http: e.allow_loopback_http,
                ..Endpoints::default()
            },
            None => Endpoints::default(),
        };
        let keys = BotKeys::new(&endpoints).map_err(|e| anyhow::anyhow!("{e}"))?;
        let connector =
            BotConnector::new(&config.app_id, &config.tenant_id, secret.trim(), &endpoints)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        let kept = std::fs::read(&config.state_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            auth: BotAuth::new(&config.app_id, &config.tenant_id),
            config,
            keys: Arc::new(keys),
            connector: Arc::new(connector),
            kept: Mutex::new(kept),
        })
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self) {
        let text = serde_json::to_vec(&*self.kept()).unwrap_or_default();
        let making = self.config.state_file.with_extension("json.new");
        if std::fs::write(&making, text)
            .and_then(|()| std::fs::rename(&making, &self.config.state_file))
            .is_err()
        {
            tracing::warn!("couldn't keep the Teams conversation");
        }
    }

    fn is_person(&self, aad_object_id: &str) -> bool {
        !aad_object_id.is_empty() && aad_object_id == self.config.person_aad_object_id
    }

    fn card(&self, agent: &str, approval: &api::ApprovalView, answerable: bool) -> Value {
        let refresh: Vec<String> = self
            .kept()
            .conversation
            .iter()
            .map(|c| c.user_id.clone())
            .collect();
        glass_teams::approval_card(
            approval,
            &CardContext {
                agent_name: agent,
                open_url: &self.config.open_url,
                refresh_for: &refresh,
                answerable,
                answered_by: None,
            },
        )
    }

    /// Sends `activity` to a conversation, off the request's thread.
    fn send(&self, to: &ConversationRef, activity: Value) {
        let connector = Arc::clone(&self.connector);
        let (url, id) = (to.service_url.clone(), to.conversation_id.clone());
        tokio::task::spawn_blocking(move || {
            if let Err(e) = connector.send(&url, &id, &activity) {
                tracing::warn!("sending to Teams: {e}");
            }
        });
    }
}

fn json(value: &Value) -> Response {
    Response::json(200, value)
}

impl Front {
    /// `POST /api/teams/messages`: only calls Microsoft signed for this app
    /// get past the first lines; anything else is a bare 401.
    pub async fn teams_messages(&self, request: &Request) -> Response {
        let Some(teams) = self.teams.as_ref() else {
            return Response::text(404, "not found");
        };
        if request.method != "POST" {
            return Response::text(405, "method not allowed");
        }
        let authorization = request.header("authorization").map(str::to_owned);
        let kid = glass_teams::key_id(authorization.as_deref());
        let keys = Arc::clone(&teams.keys);
        let fetched = tokio::task::spawn_blocking(move || keys.signing_keys(kid.as_deref())).await;
        let jwks = match fetched.map(|r| r.map(|text| Jwks::parse(&text))) {
            Ok(Ok(Ok(jwks))) => jwks,
            other => {
                tracing::warn!("Microsoft's signing keys: {other:?}");
                return Response::text(503, "try again");
            }
        };
        let now = glass_types::Timestamp::now().unix_seconds();
        let activity = match glass_teams::verify(
            &teams.auth,
            authorization.as_deref(),
            &request.body,
            &jwks,
            now,
        ) {
            Ok(activity) => activity,
            Err(refusal) => {
                tracing::warn!("refused a call to the Teams endpoint: {refusal}");
                return Response::new(401, "text/plain", "");
            }
        };
        match glass_teams::interpret(&activity) {
            Incoming::Installed(conversation) => {
                if teams.is_person(&conversation.user_aad_object_id) {
                    teams.kept().conversation = Some(conversation.clone());
                    teams.save();
                    teams.send(
                        &conversation,
                        glass_teams::text_activity(&format!(
                            "Hi {}. Your agents will ask you here when they need you. Write \
                             to one by its name, like \"Reconciler, which invoices don't match?\"",
                            self.person.name
                        )),
                    );
                }
                json(&Value::Null)
            }
            Incoming::Uninstalled(conversation) => {
                if teams.is_person(&conversation.user_aad_object_id) {
                    teams.kept().conversation = None;
                    teams.save();
                }
                json(&Value::Null)
            }
            Incoming::Message(message) => {
                let reply = if teams.is_person(&message.from.aad_object_id) {
                    self.teams_ask(teams, &message).await
                } else {
                    format!(
                        "These agents work for {}; ask your own in Glasshouse Agents.",
                        self.person.name
                    )
                };
                teams.send(&message.conversation, glass_teams::text_activity(&reply));
                json(&Value::Null)
            }
            Incoming::Answer(answer) => {
                if !teams.is_person(&answer.by.aad_object_id) {
                    return json(&glass_teams::invoke_message(&format!(
                        "Only {} can answer this.",
                        self.person.name
                    )));
                }
                let sent = teams.kept().sent.get(&answer.approval).cloned();
                let agent = sent
                    .as_ref()
                    .map_or("Your agent", |s| s.agent.as_str())
                    .to_owned();
                match self
                    .answer(
                        &answer.approval,
                        api::Answer {
                            yes: answer.yes,
                            via: api::Channel::Teams,
                        },
                    )
                    .await
                {
                    Ok(view) => {
                        if let Some(sent) = teams.kept().sent.get_mut(&answer.approval) {
                            sent.approval = view.clone();
                        }
                        teams.save();
                        json(&glass_teams::invoke_response(
                            teams.card(&agent, &view, false),
                        ))
                    }
                    Err(failure) => json(&glass_teams::invoke_message(&failure.message)),
                }
            }
            Incoming::Refresh(refresh) => {
                let sent = teams.kept().sent.get(&refresh.approval).cloned();
                match sent {
                    Some(sent) => {
                        let answerable = teams.is_person(&refresh.by.aad_object_id)
                            && sent.approval.state == api::ApprovalState::Pending;
                        json(&glass_teams::invoke_response(teams.card(
                            &sent.agent,
                            &sent.approval,
                            answerable,
                        )))
                    }
                    None => json(&glass_teams::invoke_error(404, "That question is gone.")),
                }
            }
            Incoming::Other { .. } => json(&Value::Null),
        }
    }

    /// A message to one of the person's agents: it's asked, and answers in
    /// the same chat when it's done.
    async fn teams_ask(&self, teams: &Teams, message: &glass_teams::MessageToAgent) -> String {
        let agents = match self.agents().await {
            Ok(agents) => agents,
            Err(failure) => return failure.message,
        };
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        let Some((index, text)) = message.addressed_to(&names) else {
            return if agents.is_empty() {
                "You have no agents yet: make one in Glasshouse Agents.".into()
            } else {
                format!(
                    "Which agent? Start with its name: {}.",
                    names
                        .iter()
                        .map(|n| format!("\"{n}, …\""))
                        .collect::<Vec<_>>()
                        .join(" or ")
                )
            };
        };
        let agent = &agents[index];
        match self
            .ask(
                &agent.id,
                api::Ask {
                    text,
                    files: Vec::new(),
                    via: api::Channel::Teams,
                },
            )
            .await
        {
            Ok(asked) => {
                teams.kept().asked.insert(
                    asked.task,
                    Asked {
                        agent: agent.name.clone(),
                        conversation: message.conversation.clone(),
                    },
                );
                teams.save();
                format!("{} is on it. It'll answer here.", agent.name)
            }
            Err(failure) => format!("{} couldn't start: {}", agent.name, failure.message),
        }
    }

    /// Sends new approvals to the person's chat, turns cards answered
    /// anywhere into their answer, and answers what was asked from Teams.
    pub async fn teams_loop(self: Arc<Self>) {
        let Some(teams) = self.teams.as_ref() else {
            return;
        };
        loop {
            tokio::time::sleep(LOOK_EVERY).await;
            if let Err(e) = self.teams_look(teams).await {
                tracing::warn!("Teams: {e}");
            }
        }
    }

    /// One look, for tests.
    #[cfg(test)]
    pub async fn teams_look_once(&self) -> Result<(), String> {
        match self.teams.as_ref() {
            Some(teams) => self.teams_look(teams).await,
            None => Ok(()),
        }
    }

    async fn teams_look(&self, teams: &Teams) -> Result<(), String> {
        let pending = self.approvals().await.map_err(|f| f.message)?;
        let conversation = teams.kept().conversation.clone();
        // New questions go to the person's chat.
        if let Some(conversation) = &conversation {
            for p in &pending {
                if teams.kept().sent.contains_key(&p.approval.id) {
                    continue;
                }
                let card = teams.card(&p.agent_name, &p.approval, true);
                let summary = format!("{} needs you", p.agent_name);
                let activity = glass_teams::card_activity(card, &summary);
                let connector = Arc::clone(&teams.connector);
                let (url, id) = (
                    conversation.service_url.clone(),
                    conversation.conversation_id.clone(),
                );
                let sent =
                    tokio::task::spawn_blocking(move || connector.send(&url, &id, &activity))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                teams.kept().sent.insert(
                    p.approval.id.clone(),
                    Sent {
                        activity: sent,
                        agent: p.agent_name.clone(),
                        approval: p.approval.clone(),
                    },
                );
                teams.save();
            }
        }
        // Cards no longer pending: answered (here, the web, the CLI) or gone.
        let gone: Vec<(String, Sent)> = teams
            .kept()
            .sent
            .iter()
            .filter(|(id, _)| !pending.iter().any(|p| &p.approval.id == *id))
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect();
        for (id, mut sent) in gone {
            if sent.approval.state == api::ApprovalState::Pending {
                sent.approval.state = self
                    .answered_state(&id)
                    .unwrap_or(api::ApprovalState::Expired);
                if let Some(conversation) = &conversation {
                    let card = teams.card(&sent.agent, &sent.approval, false);
                    let activity = glass_teams::card_activity(card, &sent.approval.summary);
                    let connector = Arc::clone(&teams.connector);
                    let (url, cid, aid) = (
                        conversation.service_url.clone(),
                        conversation.conversation_id.clone(),
                        sent.activity.clone(),
                    );
                    let _ = tokio::task::spawn_blocking(move || {
                        connector.update(&url, &cid, &aid, &activity)
                    })
                    .await;
                }
            }
            // Its card now says how it ended, with nothing to refresh.
            teams.kept().sent.remove(&id);
            teams.save();
        }
        // What was asked from Teams, answered there once it's done.
        let asked: Vec<(String, Asked)> = teams
            .kept()
            .asked
            .iter()
            .map(|(t, a)| (t.clone(), a.clone()))
            .collect();
        for (task, asked) in asked {
            let text = match self.task_answer(&task).await {
                crate::server::TaskAnswer::NotYet => continue,
                crate::server::TaskAnswer::Ended(text) => text,
                // Said once, where the person is waiting for an answer.
                crate::server::TaskAnswer::Gone => {
                    "I lost track of that before it was done (the computer it ran on started \
                     again). Ask me again."
                        .into()
                }
            };
            teams.send(
                &asked.conversation,
                glass_teams::text_activity(&format!("{}: {text}", asked.agent)),
            );
            teams.kept().asked.remove(&task);
            teams.save();
        }
        Ok(())
    }
}
