//! Acting for people on a shared agent host (ADR 0067). glassd knows a
//! caller by the uid the kernel reports for its control socket; on an
//! agent host people have no accounts, so the front, known by its uid,
//! says who a connection is for (`session.as`), and glassd checks the
//! proof itself before it believes it: the load balancer's signed
//! assertion, a `glass` token it issued, or, in development only, a name.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glass_api::{IssuedToken, LoginCode, PersonView, SessionAs};
use glass_audit::{AuditEvent, EventKind};
use glass_types::Timestamp;

use crate::people::{Claims, People, Person, verify_load_balancer};
use crate::{Caller, Runtime, RuntimeError};

type Result<T> = std::result::Result<T, RuntimeError>;

/// AWS's public key for a load balancer key id.
pub type KeyFetch =
    dyn Fn(&str) -> std::result::Result<glass_crypto::PublicKey, String> + Send + Sync;

/// How often a person's sessions of one kind are put on the record: the
/// front opens one per request.
const SESSION_RECORD_EVERY: Duration = Duration::from_secs(600);

/// Who may say who a connection is for, and how glassd checks it.
pub struct Identity {
    pub people: Arc<People>,
    /// The front's account: the one uid that may call `session.as`.
    pub front_uid: u32,
    /// This stack's load balancer's ARN: its assertions' `signer`. Empty
    /// takes none.
    pub signer: String,
    pub keys: Arc<KeyFetch>,
    /// Takes `SessionAs::Dev`: development only.
    pub dev_people: bool,
    /// People's folders on this host, one each, theirs.
    pub homes: PathBuf,
    recorded: Mutex<HashMap<(u32, String), Instant>>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("front_uid", &self.front_uid)
            .field("signer", &self.signer)
            .field("dev_people", &self.dev_people)
            .finish_non_exhaustive()
    }
}

impl Identity {
    pub fn new(
        people: Arc<People>,
        front_uid: u32,
        signer: String,
        keys: Arc<KeyFetch>,
        dev_people: bool,
        homes: PathBuf,
    ) -> Self {
        Self {
            people,
            front_uid,
            signer,
            keys,
            dev_people,
            homes,
            recorded: Mutex::new(HashMap::new()),
        }
    }
}

fn now() -> i64 {
    Timestamp::now().unix_seconds()
}

fn at(secs: i64) -> Result<Timestamp> {
    Timestamp::from_unix_seconds(secs)
        .ok_or_else(|| RuntimeError::Internal(format!("{secs} isn't a time")))
}

fn view(person: &Person, proven_by: &str) -> PersonView {
    PersonView {
        key: person.key.clone(),
        name: person.name.clone(),
        email: person.email.clone(),
        proven_by: proven_by.into(),
    }
}

impl Runtime {
    /// Set once at startup on an agent host (glassd's `[agents]`).
    pub fn set_identity(&self, identity: Identity) {
        let _ = self.identity.set(identity);
    }

    pub fn identity(&self) -> Option<&Identity> {
        self.identity.get()
    }

    fn session_refused(&self, peer: u32, why: &str) -> RuntimeError {
        let mut event = AuditEvent::new(
            EventKind::Session,
            format!("Refused to act for someone: {why}"),
        );
        event.detail = serde_json::json!({ "peer_uid": peer });
        self.audit(event);
        RuntimeError::PermissionDenied(format!("not acting for anyone: {why}"))
    }

    fn front_only(&self, peer: u32) -> Result<&Identity> {
        let identity = self
            .identity
            .get()
            .ok_or_else(|| RuntimeError::Invalid("this host doesn't act for people".into()))?;
        if peer != identity.front_uid {
            return Err(self.session_refused(peer, "only the front may say who it acts for"));
        }
        Ok(identity)
    }

    /// `session.as`: the person `proof` proves, as a caller of their own on
    /// this host, made the first time they're seen.
    pub(crate) fn act_as(&self, peer: u32, proof: &SessionAs) -> Result<(Caller, PersonView)> {
        let identity = self.front_only(peer)?;
        let (claims, proven_by) = match proof {
            SessionAs::LoadBalancer { assertion } => {
                let claims = verify_load_balancer(
                    assertion,
                    &identity.signer,
                    identity.keys.as_ref(),
                    now(),
                )
                .map_err(|why| self.session_refused(peer, &format!("the sign-in: {why}")))?;
                (claims, "load_balancer")
            }
            SessionAs::Token { token } => {
                let person = identity
                    .people
                    .by_token(token.trim(), now())
                    .ok_or_else(|| {
                        self.session_refused(
                            peer,
                            "that token isn't one this host gave, or it expired or was revoked",
                        )
                    })?;
                return self.open_session(identity, &person, "token");
            }
            SessionAs::Dev { sub, name, email } => {
                if !identity.dev_people {
                    return Err(
                        self.session_refused(peer, "development sign-ins are off on this host")
                    );
                }
                (
                    Claims {
                        sub: sub.clone(),
                        name: name.clone(),
                        email: email.clone(),
                    },
                    "dev",
                )
            }
        };
        let person = identity
            .people
            .person(&claims)
            .map_err(|why| self.session_refused(peer, &why))?;
        self.open_session(identity, &person, proven_by)
    }

    fn open_session(
        &self,
        identity: &Identity,
        person: &Person,
        proven_by: &str,
    ) -> Result<(Caller, PersonView)> {
        let caller = self.caller_for(identity, person)?;
        // One line per person and kind of proof every ten minutes: the front
        // opens a session for each request.
        let due = {
            let mut recorded = identity.recorded.lock().unwrap_or_else(|e| e.into_inner());
            let key = (person.uid, proven_by.to_owned());
            let due = recorded
                .get(&key)
                .is_none_or(|last| last.elapsed() >= SESSION_RECORD_EVERY);
            if due {
                recorded.insert(key, Instant::now());
            }
            due
        };
        if due {
            let mut event = AuditEvent::new(
                EventKind::Session,
                format!(
                    "Acting for {} ({})",
                    person.name,
                    proven_by.replace('_', " ")
                ),
            );
            event.user = Some(caller.name.clone());
            event.detail = serde_json::json!({ "person": person.key, "uid": person.uid, "proven_by": proven_by });
            self.audit(event);
        }
        Ok((caller, view(person, proven_by)))
    }

    /// `person` as a caller of their own on this host, their folder made
    /// the first time.
    pub(crate) fn caller_for(&self, identity: &Identity, person: &Person) -> Result<Caller> {
        let home = identity.homes.join(person.uid.to_string());
        if !home.is_dir() {
            std::fs::create_dir_all(&home)
                .and_then(|()| {
                    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
                })
                .and_then(|()| std::os::unix::fs::chown(&home, Some(person.uid), Some(person.uid)))
                .map_err(|e| {
                    RuntimeError::Internal(format!("making {}'s folder: {e}", person.name))
                })?;
        }
        Ok(Caller {
            name: person.login(),
            uid: person.uid,
            gid: person.uid,
            home,
        })
    }

    fn person_of(&self, caller: &Caller) -> Result<(&Identity, Person)> {
        let identity = self
            .identity
            .get()
            .ok_or_else(|| RuntimeError::Invalid("this host doesn't act for people".into()))?;
        let person = identity.people.by_uid(caller.uid).ok_or_else(|| {
            RuntimeError::PermissionDenied("you aren't signed in on this host".into())
        })?;
        Ok((identity, person))
    }

    /// `token.code`: a one-time code for `glass login`, for a person who
    /// signed in at the load balancer just now.
    pub(crate) fn login_code(&self, caller: &Caller, proven_by: &str) -> Result<LoginCode> {
        if proven_by != "load_balancer" && proven_by != "dev" {
            return Err(RuntimeError::PermissionDenied(
                "sign in with your organization's sign-in to let glass in".into(),
            ));
        }
        let (identity, person) = self.person_of(caller)?;
        let (code, until) = identity
            .people
            .issue_code(&person, now())
            .map_err(RuntimeError::Internal)?;
        Ok(LoginCode {
            code,
            expires_at: at(until)?,
        })
    }

    /// `token.exchange`: the front trades a code `glass` brought, once, for
    /// a token for its person.
    pub(crate) fn token_exchange(&self, peer: u32, code: &str) -> Result<IssuedToken> {
        let identity = self.front_only(peer)?;
        let person = identity
            .people
            .take_code(code, now())
            .map_err(|why| self.session_refused(peer, &why))?;
        let (token, until) = identity
            .people
            .issue_token(&person, now())
            .map_err(RuntimeError::Internal)?;
        let mut event = AuditEvent::new(
            EventKind::Session,
            format!(
                "Gave {} a token for glass, for {} days",
                person.name,
                crate::people::TOKEN_DAYS
            ),
        );
        event.user = Some(person.login());
        event.detail = serde_json::json!({ "person": person.key });
        self.audit(event);
        Ok(IssuedToken {
            token,
            expires_at: at(until)?,
        })
    }

    /// `token.revoke`: every token of the caller's goes.
    pub(crate) fn token_revoke(&self, caller: &Caller) -> Result<u32> {
        let (identity, person) = self.person_of(caller)?;
        let gone = identity
            .people
            .revoke_tokens(&person)
            .map_err(RuntimeError::Internal)?;
        let mut event = AuditEvent::new(
            EventKind::Session,
            format!("Revoked {gone} of {}'s tokens for glass", person.name),
        );
        event.user = Some(caller.name.clone());
        self.audit(event);
        Ok(u32::try_from(gone).unwrap_or(u32::MAX))
    }
}
