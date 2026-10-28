#![allow(clippy::unwrap_used)]

use std::sync::OnceLock;

use glass_agents_api::{ApprovalState, ApprovalView, Channel};
use serde_json::{Value, json};

use super::*;
use crate::testing::{TestKey, claims};

const APP: &str = "8f0c5a8e-1b2c-4d3e-9f40-5a6b7c8d9e0f";
const BOT: &str = "28:8f0c5a8e-1b2c-4d3e-9f40-5a6b7c8d9e0f";
const TENANT: &str = "b28fdbfd-2b78-4f93-b0f8-8881793f0f8f";
const SERVICE: &str = "https://smba.trafficmanager.net/amer/";
const DANA: &str = "29:1ljv6N86roXr5pjPrCJVIz6xHh5QxjI-dana";
const DANA_AAD: &str = "eddfa9d4-346e-4cce-a18f-fa6261ad776b";
const CHAT: &str = "a:1qhNLqpUtmuI6U35gzjsJn7uRnCkW8NiZALHfN8AMxdbprS1uta2aT";
const NOW: i64 = 1_761_562_440;

fn key() -> &'static TestKey {
    static KEY: OnceLock<TestKey> = OnceLock::new();
    KEY.get_or_init(|| TestKey::generate("bf-key-1"))
}

fn auth() -> BotAuth {
    BotAuth::new(APP, TENANT)
}

fn keys() -> Jwks {
    Jwks::parse(&key().jwks()).unwrap()
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// A message in a group chat that mentions the app, as Teams sends it.
fn mention_message() -> Value {
    json!({
        "text": "<at>Glasshouse Agents</at>&nbsp;@Reconciler which invoices don't match?",
        "textFormat": "plain",
        "attachments": [{
            "contentType": "text/html",
            "content": "<div><div><span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Glasshouse Agents</span>&nbsp;@Reconciler which invoices don't match?</div></div>"
        }],
        "type": "message",
        "timestamp": "2026-10-27T10:54:00.1234567Z",
        "localTimestamp": "2026-10-27T11:54:00.1234567+01:00",
        "id": "1761562440123",
        "channelId": "msteams",
        "serviceUrl": SERVICE,
        "from": {"id": DANA, "name": "Dana Whitfield", "aadObjectId": DANA_AAD},
        "conversation": {
            "isGroup": true,
            "conversationType": "groupChat",
            "tenantId": TENANT,
            "id": "19:3c5d1b7e0a2f4e6c8d9b@thread.v2"
        },
        "recipient": {"id": BOT, "name": "Glasshouse Agents"},
        "entities": [
            {
                "mentioned": {"id": BOT, "name": "Glasshouse Agents"},
                "text": "<at>Glasshouse Agents</at>",
                "type": "mention"
            },
            {"locale": "en-US", "country": "US", "platform": "Web", "timezone": "Europe/London", "type": "clientInfo"}
        ],
        "channelData": {"tenant": {"id": TENANT}},
        "locale": "en-US",
        "localTimezone": "Europe/London"
    })
}

/// Approve pressed on a card in Dana's chat with the app.
fn answer_invoke(verb: &str, data: Value) -> Value {
    json!({
        "name": "adaptiveCard/action",
        "type": "invoke",
        "timestamp": "2026-10-27T10:58:00.511Z",
        "localTimestamp": "2026-10-27T06:58:00.511-04:00",
        "id": "f:3a2b7c1d-5e4f-4a3b-8c2d-1e0f9a8b7c6d",
        "channelId": "msteams",
        "serviceUrl": SERVICE,
        "from": {"id": DANA, "name": "Dana Whitfield", "aadObjectId": DANA_AAD},
        "conversation": {"conversationType": "personal", "tenantId": TENANT, "id": CHAT},
        "recipient": {"id": BOT, "name": "Glasshouse Agents"},
        "entities": [{"locale": "en-US", "country": "US", "platform": "Windows", "timezone": "America/New_York", "type": "clientInfo"}],
        "channelData": {"tenant": {"id": TENANT}, "source": {"name": "message"}, "legacy": {"replyToId": "1761562500456"}},
        "replyToId": "1761562500456",
        "value": {
            "action": {"type": "Action.Execute", "title": "Send 3 emails", "verb": verb, "data": data},
            "trigger": "manual"
        },
        "locale": "en-US",
        "localTimezone": "America/New_York"
    })
}

/// The app installed for Dana by the org's Teams admin.
fn install(action: &str, conversation_type: &str) -> Value {
    json!({
        "type": "installationUpdate",
        "id": "f:816eb23d-bfa1-afa3-dfeb-d2aa338e9541",
        "timestamp": "2026-10-27T09:00:00.91Z",
        "serviceUrl": SERVICE,
        "channelId": "msteams",
        "from": {"id": DANA, "aadObjectId": DANA_AAD},
        "recipient": {"id": BOT, "name": "Glasshouse Agents"},
        "locale": "en-US",
        "entities": [{"type": "clientInfo", "locale": "en-US"}],
        "conversation": {"conversationType": conversation_type, "tenantId": TENANT, "id": CHAT},
        "channelData": {"tenant": {"id": TENANT}, "source": {"name": "message"}},
        "action": action
    })
}

fn activity(json: &Value) -> Activity {
    serde_json::from_value(json.clone()).unwrap()
}

fn check(token: &str, body: &Value) -> Result<Activity, Refusal> {
    verify(
        &auth(),
        Some(&bearer(token)),
        body.to_string().as_bytes(),
        &keys(),
        NOW,
    )
}

#[test]
fn adr0065_only_microsofts_signed_calls_reach_the_relay() {
    let body = mention_message();
    let good = key().sign(&claims(APP, SERVICE, NOW));
    let got = check(&good, &body).unwrap();
    assert_eq!(got.kind, "message");
    assert_eq!(got.from.aad_object_id, DANA_AAD);
    assert_eq!(key_id(Some(&bearer(&good))).as_deref(), Some("bf-key-1"));

    // A token for another app.
    let other_app = key().sign(&claims(
        "11111111-2222-3333-4444-555555555555",
        SERVICE,
        NOW,
    ));
    assert_eq!(check(&other_app, &body).unwrap_err(), Refusal::Audience);
    // An audience list naming us is fine.
    let mut listed = claims(APP, SERVICE, NOW);
    listed["aud"] = json!(["api://other", APP]);
    assert!(check(&key().sign(&listed), &body).is_ok());

    // Expired, past the five minutes' skew; within it, still good.
    let mut old = claims(APP, SERVICE, NOW - 7200);
    old["exp"] = json!(NOW - CLOCK_SKEW_SECS - 1);
    assert_eq!(
        check(&key().sign(&old), &body).unwrap_err(),
        Refusal::Expired
    );
    old["exp"] = json!(NOW - 60);
    assert!(check(&key().sign(&old), &body).is_ok());
    let mut early = claims(APP, SERVICE, NOW);
    early["nbf"] = json!(NOW + CLOCK_SKEW_SECS + 60);
    assert_eq!(
        check(&key().sign(&early), &body).unwrap_err(),
        Refusal::NotYetValid
    );
    let mut forever = claims(APP, SERVICE, NOW);
    forever.as_object_mut().unwrap().remove("exp");
    assert!(matches!(
        check(&key().sign(&forever), &body).unwrap_err(),
        Refusal::Malformed(_)
    ));

    // A bad signature: another key under the same id, or a changed payload.
    let impostor = TestKey::generate("bf-key-1");
    assert_eq!(
        check(&impostor.sign(&claims(APP, SERVICE, NOW)), &body).unwrap_err(),
        Refusal::BadSignature
    );
    let parts: Vec<&str> = good.split('.').collect();
    let mut changed = claims(APP, SERVICE, NOW);
    changed["aud"] = json!(APP);
    changed["exp"] = json!(NOW + 999_999);
    let forged = format!(
        "{}.{}.{}",
        parts[0],
        crate::b64::encode(changed.to_string().as_bytes()),
        parts[2]
    );
    assert_eq!(check(&forged, &body).unwrap_err(), Refusal::BadSignature);

    // A key Microsoft hasn't published (or we haven't fetched yet).
    let unknown = TestKey::generate("bf-key-2");
    let token = unknown.sign(&claims(APP, SERVICE, NOW));
    assert_eq!(
        check(&token, &body).unwrap_err(),
        Refusal::UnknownKey("bf-key-2".into())
    );
    assert_eq!(key_id(Some(&bearer(&token))).as_deref(), Some("bf-key-2"));

    // The serviceurl claim must be the activity's serviceUrl: a token for
    // one conversation's service can't send another somewhere else.
    let elsewhere = key().sign(&claims(APP, "https://evil.example/teams/", NOW));
    assert_eq!(check(&elsewhere, &body).unwrap_err(), Refusal::ServiceUrl);
    let mut none = claims(APP, SERVICE, NOW);
    none.as_object_mut().unwrap().remove("serviceurl");
    assert_eq!(
        check(&key().sign(&none), &body).unwrap_err(),
        Refusal::ServiceUrl
    );
}

#[test]
fn adr0065_tokens_are_rs256_from_the_bot_connector_for_teams_in_the_org() {
    let body = mention_message();
    let auth_header = |t: &str| Some(format!("Bearer {t}"));
    let good = key().sign(&claims(APP, SERVICE, NOW));

    // No token, another scheme, or not a JWT.
    for header in [None, Some(format!("Basic {good}")), Some("Bearer ".into())] {
        let got = verify(&auth(), header.as_deref(), b"{}", &keys(), NOW);
        assert_eq!(got.unwrap_err(), Refusal::NoToken);
    }
    for token in ["a.b", "a.b.c.d", "!!.e30.sig", &"x".repeat(20_000)] {
        let got = verify(&auth(), auth_header(token).as_deref(), b"{}", &keys(), NOW);
        assert!(
            matches!(got.unwrap_err(), Refusal::Malformed(_)),
            "{token:.20}"
        );
    }

    // Only RS256: no "none", no HMAC with the public key as its secret.
    for alg in ["none", "HS256", "RS512", "PS256"] {
        let header = json!({"alg": alg, "kid": "bf-key-1"});
        let token = key().sign_with_header(&header, &claims(APP, SERVICE, NOW));
        assert_eq!(
            check(&token, &body).unwrap_err(),
            Refusal::Algorithm(alg.into())
        );
    }

    // Issued by someone else.
    let mut other = claims(APP, SERVICE, NOW);
    other["iss"] = json!("https://sts.windows.net/d6d49420-f39b-4df7-a1dc-d59a935871db/");
    assert_eq!(
        check(&key().sign(&other), &body).unwrap_err(),
        Refusal::Issuer
    );

    // A key endorsed for another channel only.
    let webchat = Jwks::parse(&json!({"keys": [key().jwk(&["webchat"])]}).to_string()).unwrap();
    let got = verify(
        &auth(),
        auth_header(&good).as_deref(),
        body.to_string().as_bytes(),
        &webchat,
        NOW,
    );
    assert_eq!(got.unwrap_err(), Refusal::NotEndorsed);

    // Another channel, or another tenant.
    let mut web = body.clone();
    web["channelId"] = json!("webchat");
    assert_eq!(check(&good, &web).unwrap_err(), Refusal::Channel);
    let mut foreign = body.clone();
    foreign["conversation"]["tenantId"] = json!("00000000-0000-0000-0000-000000000000");
    foreign["channelData"]["tenant"]["id"] = json!("00000000-0000-0000-0000-000000000000");
    assert_eq!(check(&good, &foreign).unwrap_err(), Refusal::Tenant);
    let mut disagrees = body.clone();
    disagrees["channelData"]["tenant"]["id"] = json!("00000000-0000-0000-0000-000000000000");
    assert_eq!(check(&good, &disagrees).unwrap_err(), Refusal::Tenant);
    let got = verify(
        &auth(),
        auth_header(&good).as_deref(),
        b"not json",
        &keys(),
        NOW,
    );
    assert!(matches!(got.unwrap_err(), Refusal::Activity(_)));

    // A key too short to trust.
    let small = glass_crypto::rsa::testing::RsaTestKey::generate(1024);
    let mut weak = key().jwk(&["msteams"]);
    weak["n"] = json!(crate::b64::encode(&small.modulus()));
    let weak = Jwks::parse(&json!({"keys": [weak]}).to_string()).unwrap();
    let got = verify(
        &auth(),
        auth_header(&good).as_deref(),
        body.to_string().as_bytes(),
        &weak,
        NOW,
    );
    assert_eq!(got.unwrap_err(), Refusal::BadKey("bf-key-1".into()));

    // Every refusal is a bare 401, and its words never carry the token.
    let refusals = [
        Refusal::NoToken,
        Refusal::BadSignature,
        Refusal::UnknownKey("k".into()),
        Refusal::ServiceUrl,
        Refusal::Tenant,
        Refusal::Activity("x".into()),
    ];
    for refusal in refusals {
        assert_eq!(refusal.status(), 401);
        assert!(!refusal.to_string().contains(&good));
    }
    // What an unverified token names is cleaned before it's logged.
    let header = json!({"alg": "RS256", "kid": "evil\nkey<script>"});
    let token = key().sign_with_header(&header, &claims(APP, SERVICE, NOW));
    assert_eq!(
        check(&token, &body).unwrap_err(),
        Refusal::UnknownKey("evilkeyscript".into())
    );
}

#[test]
fn adr0065_a_message_names_its_sender_by_entra_id_and_its_agent() {
    let Incoming::Message(m) = interpret(&activity(&mention_message())) else {
        panic!("a message");
    };
    assert_eq!(m.from.aad_object_id, DANA_AAD);
    assert_eq!(m.from.teams_id, DANA);
    assert_eq!(m.from.name, "Dana Whitfield");
    assert_eq!(m.full_text, "@Reconciler which invoices don't match?");
    assert_eq!(m.agent.as_deref(), Some("Reconciler"));
    assert_eq!(m.text, "which invoices don't match?");
    assert_eq!(m.activity_id, "1761562440123");
    assert_eq!(
        m.conversation,
        ConversationRef {
            service_url: SERVICE.into(),
            conversation_id: "19:3c5d1b7e0a2f4e6c8d9b@thread.v2".into(),
            tenant_id: TENANT.into(),
            bot_id: BOT.into(),
            user_id: DANA.into(),
            user_aad_object_id: DANA_AAD.into(),
            scope: Scope::GroupChat,
        }
    );
    // The agent by a leading name, against the person's agents.
    assert_eq!(
        m.addressed_to(&["Invoices", "Reconciler"]),
        Some((1, "which invoices don't match?".into()))
    );

    let mut plain = mention_message();
    plain["text"] = json!("reconciler, which invoices don't match?");
    plain["entities"] = json!([]);
    plain["conversation"]["conversationType"] = json!("personal");
    let Incoming::Message(m) = interpret(&activity(&plain)) else {
        panic!("a message");
    };
    assert_eq!(m.agent, None);
    assert_eq!(m.text, "reconciler, which invoices don't match?");
    assert_eq!(m.conversation.scope, Scope::Personal);
    assert_eq!(
        m.addressed_to(&["Invoice", "Invoice Checker", "Reconciler"]),
        Some((2, "which invoices don't match?".into()))
    );
    let mut checker = m.clone();
    checker.full_text = "Invoice Checker: any duplicates?".into();
    assert_eq!(
        checker.addressed_to(&["Invoice", "Invoice Checker"]),
        Some((1, "any duplicates?".into()))
    );
    checker.full_text = "Reconciliation is late".into();
    assert_eq!(checker.addressed_to(&["Reconciler"]), None);

    // Other people's mentions stay in the text.
    let mut others = mention_message();
    others["text"] = json!("<at>Glasshouse Agents</at> ask <at>Sam Lee</at> too");
    others["entities"] = json!([
        {"type": "mention", "mentioned": {"id": BOT, "name": "Glasshouse Agents"}, "text": "<at>Glasshouse Agents</at>"},
        {"type": "mention", "mentioned": {"id": "29:sam", "name": "Sam Lee"}, "text": "<at>Sam Lee</at>"}
    ]);
    let Incoming::Message(m) = interpret(&activity(&others)) else {
        panic!("a message");
    };
    assert_eq!(m.full_text, "ask <at>Sam Lee</at> too");

    // No Entra object id: nobody we can name, so nothing for an agent.
    let mut anonymous = mention_message();
    anonymous["from"] = json!({"id": DANA, "name": "Dana Whitfield"});
    assert!(matches!(
        interpret(&activity(&anonymous)),
        Incoming::Other { .. }
    ));
}

#[test]
fn adr0065_a_card_answer_carries_only_its_verb_and_approval_id() {
    let Incoming::Answer(a) = interpret(&activity(&answer_invoke(
        "approve",
        json!({"approval": "a1"}),
    ))) else {
        panic!("an answer");
    };
    assert_eq!(a.approval, "a1");
    assert!(a.yes);
    assert_eq!(a.by.aad_object_id, DANA_AAD);
    assert_eq!(a.card_activity_id, "1761562500456");
    assert_eq!(a.conversation.scope, Scope::Personal);
    assert_eq!(a.conversation.conversation_id, CHAT);

    // Anything else in the data is ignored: the verb decides, and who
    // answered is who Teams says pressed it.
    let crafted = answer_invoke(
        "deny",
        json!({"approval": "a1", "yes": true, "by": "ceo", "aadObjectId": "ffffffff-0000"}),
    );
    let Incoming::Answer(a) = interpret(&activity(&crafted)) else {
        panic!("an answer");
    };
    assert!(!a.yes);
    assert_eq!(a.by.aad_object_id, DANA_AAD);

    // The card looking for its state.
    let mut refresh = answer_invoke("refresh", json!({"approval": "a1"}));
    refresh["value"]["trigger"] = json!("automatic");
    let Incoming::Refresh(r) = interpret(&activity(&refresh)) else {
        panic!("a refresh");
    };
    assert_eq!((r.approval.as_str(), r.by.teams_id.as_str()), ("a1", DANA));

    for bad in [
        answer_invoke("approve", json!({})),
        answer_invoke("approve", json!({"approval": "../../etc"})),
        answer_invoke("approve", json!({"approval": 7})),
        answer_invoke("delete", json!({"approval": "a1"})),
    ] {
        assert!(
            matches!(interpret(&activity(&bad)), Incoming::Other { .. }),
            "{bad}"
        );
    }
    let mut submit = answer_invoke("approve", json!({"approval": "a1"}));
    submit["value"]["action"]["type"] = json!("Action.Submit");
    assert!(matches!(
        interpret(&activity(&submit)),
        Incoming::Other { .. }
    ));
    let mut nameless = answer_invoke("approve", json!({"approval": "a1"}));
    nameless["from"]["aadObjectId"] = Value::Null;
    assert!(matches!(
        interpret(&activity(&nameless)),
        Incoming::Other { .. }
    ));
    let mut other_invoke = answer_invoke("approve", json!({"approval": "a1"}));
    other_invoke["name"] = json!("composeExtension/query");
    assert!(matches!(
        interpret(&activity(&other_invoke)),
        Incoming::Other { .. }
    ));
}

#[test]
fn adr0065_installing_the_app_keeps_where_to_reach_the_person() {
    let want = ConversationRef {
        service_url: SERVICE.into(),
        conversation_id: CHAT.into(),
        tenant_id: TENANT.into(),
        bot_id: BOT.into(),
        user_id: DANA.into(),
        user_aad_object_id: DANA_AAD.into(),
        scope: Scope::Personal,
    };
    assert_eq!(
        interpret(&activity(&install("add", "personal"))),
        Incoming::Installed(want.clone())
    );
    assert_eq!(
        interpret(&activity(&install("remove", "personal"))),
        Incoming::Uninstalled(want.clone())
    );
    // Kept as JSON, read back the same.
    let stored = serde_json::to_string(&want).unwrap();
    assert!(stored.contains("\"scope\":\"personal\""));
    assert_eq!(
        serde_json::from_str::<ConversationRef>(&stored).unwrap(),
        want
    );

    // conversationUpdate when the app joins a one-to-one chat, as Teams
    // sends it: the tenant only in channelData.
    let joined = json!({
        "membersAdded": [{"id": BOT}, {"id": DANA, "aadObjectId": DANA_AAD}],
        "type": "conversationUpdate",
        "timestamp": "2026-10-27T09:00:01.349Z",
        "id": "f:5f85c2ad",
        "channelId": "msteams",
        "serviceUrl": SERVICE,
        "from": {"id": DANA, "aadObjectId": DANA_AAD},
        "conversation": {"conversationType": "personal", "id": CHAT},
        "recipient": {"id": BOT, "name": "Glasshouse Agents"},
        "channelData": {"tenant": {"id": TENANT}}
    });
    assert_eq!(interpret(&activity(&joined)), Incoming::Installed(want));

    // Installed in a team: not anyone's own chat, never a place for
    // approvals.
    let mut team = install("add", "channel");
    team["conversation"]["id"] = json!("19:0b7f32667e064dd9b25d7969801541f4@thread.tacv2");
    assert!(matches!(
        interpret(&activity(&team)),
        Incoming::Other { .. }
    ));
    // Someone else joining a chat isn't an install.
    let mut member = joined.clone();
    member["membersAdded"] = json!([{"id": "29:sam", "aadObjectId": "a1b2"}]);
    assert!(matches!(
        interpret(&activity(&member)),
        Incoming::Other { .. }
    ));
    let typing = json!({"type": "typing", "channelId": "msteams", "serviceUrl": SERVICE});
    assert_eq!(
        interpret(&activity(&typing)),
        Incoming::Other {
            kind: "typing".into(),
            why: "an activity the app doesn't handle".into()
        }
    );
}

fn view(state: ApprovalState, confidential: bool) -> ApprovalView {
    ApprovalView {
        id: "a1".into(),
        action: "mail.send".into(),
        summary: "Send 3 emails to Northwind about invoice 4471?".into(),
        detail: "To: ap@northwind.example, [click](https://phish.example)".into(),
        yes_label: "Send 3 emails".into(),
        state,
        confidential,
    }
}

const OPEN: &str = "https://agents.acme.example/agents/reconciler?approval=a1";

fn ctx(refresh_for: &[String]) -> CardContext<'_> {
    CardContext {
        agent_name: "Reconciler",
        open_url: OPEN,
        refresh_for,
        answerable: true,
        answered_by: Some("Dana Whitfield"),
    }
}

/// Every string in `card`, keys and values.
fn strings(card: &Value) -> Vec<String> {
    match card {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, v)| std::iter::once(k.clone()).chain(strings(v)))
            .collect(),
        _ => Vec::new(),
    }
}

fn text_of(card: &Value) -> String {
    let mut out = String::new();
    for block in card["body"].as_array().unwrap() {
        match block["type"].as_str().unwrap() {
            "RichTextBlock" => {
                for run in block["inlines"].as_array().unwrap() {
                    assert_eq!(run["type"], "TextRun");
                    out.push_str(run["text"].as_str().unwrap());
                }
            }
            "TextBlock" => out.push_str(block["text"].as_str().unwrap()),
            other => panic!("unexpected {other}"),
        }
        out.push('\n');
    }
    out
}

#[test]
fn adr0065_a_pending_card_asks_with_approve_dont_and_open() {
    let people = vec![DANA.to_string(), "29:delegate-sam".to_string()];
    let card = approval_card(&view(ApprovalState::Pending, false), &ctx(&people));
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(card["version"], "1.5");
    assert_eq!(
        text_of(&card),
        "Reconciler asks\nSend 3 emails to Northwind about invoice 4471?\nTo: ap@northwind.example, [click](https://phish.example)\n"
    );
    // The agent's words only in TextRuns, never parsed as Markdown.
    assert!(
        card["body"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] == "RichTextBlock")
    );
    assert_eq!(
        card["actions"],
        json!([
            {"type": "Action.Execute", "title": "Send 3 emails", "verb": "approve", "data": {"approval": "a1"}, "style": "positive"},
            {"type": "Action.Execute", "title": "Don't", "verb": "deny", "data": {"approval": "a1"}, "style": "default"},
            {"type": "Action.OpenUrl", "title": "Open", "url": OPEN}
        ])
    );
    assert_eq!(
        card["refresh"],
        json!({
            "action": {"type": "Action.Execute", "title": "Refresh", "verb": "refresh", "data": {"approval": "a1"}},
            "userIds": [DANA, "29:delegate-sam"]
        })
    );

    // Someone who may not answer sees it without the buttons.
    let mut looking = ctx(&people);
    looking.answerable = false;
    let card = approval_card(&view(ApprovalState::Pending, false), &looking);
    assert_eq!(
        card["actions"],
        json!([{"type": "Action.OpenUrl", "title": "Open", "url": OPEN}])
    );

    // At most 60 people refresh it themselves.
    let crowd: Vec<String> = (0..80).map(|i| format!("29:p{i}")).collect();
    let card = approval_card(&view(ApprovalState::Pending, false), &ctx(&crowd));
    assert_eq!(
        card["refresh"]["userIds"].as_array().unwrap().len(),
        MAX_REFRESH_USERS
    );

    // Sent as a message with the card attached.
    let message = approval_activity(&view(ApprovalState::Pending, false), &ctx(&people));
    assert_eq!(message["type"], "message");
    assert_eq!(message["attachments"][0]["contentType"], CARD_CONTENT_TYPE);
    assert_eq!(message["attachments"][0]["content"]["version"], "1.5");
    assert_eq!(
        message["summary"],
        "Reconciler: Send 3 emails to Northwind about invoice 4471?"
    );
}

#[test]
fn adr0065_an_answered_card_says_who_when_and_how_with_no_buttons() {
    let at = "2026-10-27T10:58:00.123+02:00";
    let approved = ApprovalState::Approved {
        by: "dana".into(),
        at: at.into(),
        via: Channel::Teams,
    };
    let card = approval_card(&view(approved, false), &ctx(&[]));
    assert_eq!(
        text_of(&card),
        "Reconciler asked\nSend 3 emails to Northwind about invoice 4471?\nTo: ap@northwind.example, [click](https://phish.example)\nApproved by Dana Whitfield in Teams\n{{DATE(2026-10-27T08:58:00Z, SHORT)}} at {{TIME(2026-10-27T08:58:00Z)}}\n"
    );
    assert!(card.get("actions").is_none() && card.get("refresh").is_none());

    // Answered elsewhere first: it says so, and its buttons are gone.
    let denied = ApprovalState::Denied {
        by: "dana".into(),
        at: "2026-10-27T10:58:00Z".into(),
        via: Channel::Web,
    };
    let mut plain = ctx(&[]);
    plain.answered_by = None;
    let card = approval_card(&view(denied, false), &plain);
    let text = text_of(&card);
    assert!(
        text.contains("Answered elsewhere: Denied by dana in the web app\n"),
        "{text}"
    );
    assert!(card.get("actions").is_none() && card.get("refresh").is_none());
    let colors: Vec<String> = strings(&card);
    assert!(colors.contains(&"Attention".to_string()));

    // A time that isn't RFC 3339 is shown as it is.
    let odd = ApprovalState::Approved {
        by: "dana".into(),
        at: "yesterday".into(),
        via: Channel::Cli,
    };
    let text = text_of(&approval_card(&view(odd, false), &plain));
    assert!(
        text.ends_with("Approved by dana on the command line\nyesterday\n"),
        "{text}"
    );

    // Expired.
    let card = approval_card(&view(ApprovalState::Expired, false), &ctx(&[]));
    assert!(text_of(&card).ends_with("Expired: it wasn't answered in time, or its task ended.\n"));
    assert!(card.get("actions").is_none() && card.get("refresh").is_none());

    // The invoke's answer carries the new card.
    assert_eq!(
        invoke_response(card.clone()),
        json!({"statusCode": 200, "type": "application/vnd.microsoft.card.adaptive", "value": card})
    );
    assert_eq!(
        invoke_message("Only Dana can answer this."),
        json!({"statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "Only Dana can answer this."})
    );
    assert_eq!(
        invoke_error(400, "x")["value"],
        json!({"code": "BadRequest", "message": "x"})
    );
    assert_eq!(
        text_activity("**4 invoices**"),
        json!({"type": "message", "textFormat": "plain", "text": "**4 invoices**"})
    );
}

#[test]
fn adr0065_a_confidential_card_never_carries_its_words() {
    let people = vec![DANA.to_string()];
    let states = [
        ApprovalState::Pending,
        ApprovalState::Approved {
            by: "dana".into(),
            at: "2026-10-27T10:58:00Z".into(),
            via: Channel::Teams,
        },
        ApprovalState::Denied {
            by: "dana".into(),
            at: "2026-10-27T10:58:00Z".into(),
            via: Channel::Desktop,
        },
        ApprovalState::Expired,
    ];
    for state in states {
        let secret = view(state.clone(), true);
        let message = approval_activity(&secret, &ctx(&people));
        for s in strings(&message) {
            for words in [
                &secret.summary,
                &secret.detail,
                &secret.yes_label,
                &secret.action,
            ] {
                assert!(
                    !s.contains(words.as_str()),
                    "{state:?} carries {words:?} in {s:?}"
                );
            }
            assert!(!s.contains("Northwind") && !s.contains("emails"), "{s}");
        }
        assert_eq!(message["summary"], "Reconciler needs you");
    }
    let card = approval_card(&view(ApprovalState::Pending, true), &ctx(&people));
    assert_eq!(text_of(&card), "Reconciler needs you\n");
    assert_eq!(
        card["actions"],
        json!([{"type": "Action.OpenUrl", "title": "Open", "url": OPEN}])
    );
    assert_eq!(card["refresh"]["userIds"], json!([DANA]));
    let answered = ApprovalState::Approved {
        by: "dana".into(),
        at: "2026-10-27T10:58:00Z".into(),
        via: Channel::Teams,
    };
    let text = text_of(&approval_card(&view(answered, true), &ctx(&people)));
    assert!(
        text.starts_with("Reconciler's question\nApproved by Dana Whitfield in Teams\n"),
        "{text}"
    );
}
