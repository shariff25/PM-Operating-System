//! Password sign-in with a required authenticator code (`docs/password-sign-in.md`), on the
//! in-memory user store and, with `PMOS_TEST_DATABASE_URL`, Postgres: every rule, at the
//! service with a fake clock (the flows take the time as an argument), and the routes over
//! HTTP (who may call, the assertion mode, the uniform refusal, nothing secret in a log line
//! or an audit event).

mod common;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use common::TestPg;
use pmos_assertion::{unix_now, Claims, Key, KeyRing as AssertionKeys, RequestBinding};
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditQuery, Outcome};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::password::hash::{self, HashParams};
use pmos_toolserver::password::store::{CodeKind, PasswordStore};
use pmos_toolserver::password::{
    issue_setup_code, limits, totp, PasswordService, PwError, SecondStep, Secret,
};
use pmos_toolserver::users::{
    Actor, ChangeKind, HostUser, ImportMode, ImportRequest, NewUser, Op, UserRole,
};
use pmos_toolserver::vault::KeyRing;
use pmos_toolserver::{router_full, AppConfig, PersonClients, Stores, ToolService};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Fake people only. `ada` and `bob` are admins, `aud` an auditor, the rest PMs.
const USERS: &str = "members:
  - {handle: ada, slack-id: U0ADA, email: ada@example.com, roles: [pm, admin]}
  - {handle: bob, slack-id: U0BOB, email: bob@example.com, roles: [pm, admin]}
  - {handle: aud, slack-id: U0AUD, email: aud@example.com, roles: [auditor]}
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, email: ben@example.com, roles: [pm]}
  - {handle: cy, slack-id: U0CY, email: cy@example.com, roles: [pm]}
  - {handle: evals, slack-id: U0EVALS, roles: [pm]}
";
const PASSWORD: &str = "correct horse battery staple";
const OTHER_PASSWORD: &str = "a different long passphrase";

/// Quick parameters for the flows; the real ones are tested on their own below.
const QUICK: HashParams = HashParams {
    m_kib: 1024,
    t: 1,
    p: 1,
};

fn t0() -> DateTime<Utc> {
    "2026-11-20T09:00:00Z".parse().unwrap()
}

fn ops() -> Actor {
    Actor::host(HostUser::parse("ops").unwrap())
}

struct T {
    stores: Stores,
    svc: PasswordService,
    keys: KeyRing,
    pg: Option<TestPg>,
}

impl T {
    async fn new(postgres: bool) -> Option<Self> {
        let (stores, pg) = if postgres {
            let pg = TestPg::new().await?;
            (pg.store.stores(), Some(pg))
        } else {
            let (_, root) = common::content_tree();
            (Stores::in_memory(&root).0, None)
        };
        let req = ImportRequest::parse(USERS.as_bytes(), None, ImportMode::Bootstrap).unwrap();
        stores
            .users
            .apply(&Actor::import(None), Op::Import(Box::new(req)), t0())
            .await
            .unwrap();
        let keys = KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap();
        let svc = PasswordService::with_params(keys.clone(), QUICK).unwrap();
        Some(Self {
            stores,
            svc,
            keys,
            pg,
        })
    }

    async fn stop(mut self) {
        if let Some(pg) = self.pg.take() {
            pg.drop_schema().await;
        }
    }

    fn store(&self) -> &dyn PasswordStore {
        self.stores.users.passwords()
    }

    async fn issue(
        &self,
        actor: &Actor,
        handle: &str,
        now: DateTime<Utc>,
    ) -> Result<String, PwError> {
        issue_setup_code(self.store(), actor, handle, now)
            .await
            .map(|i| i.code.expose().to_string())
    }

    /// The whole setup for `handle` with `code`: the enrolment secret and the recovery codes.
    async fn setup(
        &self,
        handle: &str,
        code: &str,
        password: &str,
        now: DateTime<Utc>,
    ) -> (Vec<u8>, Vec<String>) {
        let s = self
            .svc
            .setup_begin(
                self.store(),
                &format!("{handle}@example.com"),
                &Secret::new(code),
                now,
            )
            .await
            .expect("setup begins");
        let token = s.token.expose().to_string();
        self.svc
            .setup_password(self.store(), &token, &Secret::new(password), now)
            .await
            .expect("password accepted");
        let e = self
            .svc
            .setup_authenticator(self.store(), &token, now)
            .await
            .expect("enrolment");
        let secret = totp::unbase32(e.secret.expose()).unwrap().to_vec();
        let done = self
            .svc
            .setup_confirm(
                self.store(),
                &token,
                &Secret::new(code_now(&secret, now)),
                now,
            )
            .await
            .expect("setup confirmed");
        assert_eq!(done.handle, handle);
        (
            secret,
            done.recovery_codes
                .iter()
                .map(|c| c.expose().to_string())
                .collect(),
        )
    }

    /// Issue (by the host) and set up: a user with a password, ready to sign in.
    async fn enrolled(&self, handle: &str, now: DateTime<Utc>) -> (Vec<u8>, Vec<String>) {
        let code = self.issue(&ops(), handle, now).await.unwrap();
        self.setup(handle, &code, PASSWORD, now).await
    }

    async fn sign_in(
        &self,
        handle: &str,
        password: &str,
        now: DateTime<Utc>,
    ) -> Result<String, PwError> {
        self.svc
            .sign_in(
                self.store(),
                &format!("{handle}@example.com"),
                &Secret::new(password),
                now,
            )
            .await
            .map(|s| s.token.expose().to_string())
    }

    async fn code(
        &self,
        token: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<pmos_toolserver::password::SignedIn, PwError> {
        self.svc
            .second_step(
                self.store(),
                token,
                &SecondStep::Code(Secret::new(code)),
                now,
            )
            .await
    }

    async fn events(&self, action: &str) -> Vec<(String, Outcome, Option<String>, Option<String>)> {
        let q = AuditQuery {
            before: None,
            limit: 500,
            actor: None,
            action: Some(action.into()),
            outcome: None,
            from: None,
            to: None,
        };
        self.stores
            .audit
            .search(&q)
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| {
                (
                    e.event.user,
                    e.event.outcome,
                    e.event.output_ref,
                    e.event.detail,
                )
            })
            .collect()
    }

    async fn all_events(&self) -> Vec<String> {
        let q = AuditQuery {
            before: None,
            limit: 2000,
            actor: None,
            action: None,
            outcome: None,
            from: None,
            to: None,
        };
        self.stores
            .audit
            .search(&q)
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| serde_json::to_string(&e.event).unwrap())
            .collect()
    }

    async fn deactivate(&self, handle: &str, now: DateTime<Utc>) {
        self.stores
            .users
            .apply(
                &ops(),
                Op::Deactivate {
                    handle: handle.into(),
                },
                now,
            )
            .await
            .unwrap();
    }
}

fn code_now(secret: &[u8], now: DateTime<Utc>) -> String {
    totp::code_at(secret, totp::step_at(now.timestamp()))
}

/// Each case runs on the in-memory store and, with `PMOS_TEST_DATABASE_URL`, Postgres.
macro_rules! both {
    ($name:ident, |$t:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            for postgres in [false, true] {
                let Some(t) = T::new(postgres).await else {
                    continue;
                };
                {
                    let $t = &t;
                    $body
                }
                t.stop().await;
            }
        }
    };
}

both!(set_up_then_sign_in_with_the_second_step, |t| {
    let now = t0();
    let code = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    let st = t.store().password_status("ana", now).await.unwrap();
    assert!(st.setup_code_pending && !st.password_set && !st.second_step_enrolled);
    let (secret, recovery) = t.setup("ana", &code, PASSWORD, now).await;
    assert_eq!(recovery.len(), 10);
    let st = t.store().password_status("ana", now).await.unwrap();
    assert!(st.password_set && st.second_step_enrolled && !st.setup_code_pending);
    assert_eq!((st.recovery_codes_left, st.epoch), (10, 1));

    // A password alone is never a sign-in: it gives a pending token, nothing else.
    let later = now + Duration::seconds(31);
    let token = t.sign_in("ana", PASSWORD, later).await.unwrap();
    let done = t
        .code(&token, &code_now(&secret, later), later)
        .await
        .unwrap();
    assert_eq!((done.handle.as_str(), done.epoch), ("ana", 1));
    assert!(!done.recovery_code_used);
    // The pending token is single use.
    assert_eq!(
        t.code(
            &token,
            &code_now(&secret, later + Duration::seconds(30)),
            later
        )
        .await,
        Err(PwError::SignInExpired)
    );
    // Recorded: issue and set in role_changes; events by reason.
    let changes = t.stores.users.role_changes(Some("ana"), 10).await.unwrap();
    let kinds: Vec<_> = changes
        .iter()
        .map(|c| (c.change, c.detail.clone()))
        .collect();
    assert!(
        kinds.contains(&(ChangeKind::IssuePasswordCode, Some("setup".into()))),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(ChangeKind::SetPassword, Some("setup".into()))),
        "{kinds:?}"
    );
    let issue = changes
        .iter()
        .find(|c| c.change == ChangeKind::IssuePasswordCode)
        .unwrap();
    assert_eq!((issue.actor.as_str(), issue.via.as_str()), ("ada", "web"));
    let ok: Vec<_> = t.events("password.second_step").await;
    assert_eq!(ok[0].0, "ana");
    assert_eq!(ok[0].3.as_deref(), Some("authenticator"));
});

both!(every_password_step_refusal_is_the_same, |t| {
    let now = t0();
    t.enrolled("ana", now).await;
    t.enrolled("cy", now).await;
    t.deactivate("cy", now).await;
    // No such user, no password set, a wrong password, deactivated: one answer.
    for (who, pw) in [
        ("nobody", PASSWORD),
        ("ben", PASSWORD),
        ("ana", OTHER_PASSWORD),
        ("cy", PASSWORD),
    ] {
        assert_eq!(
            t.sign_in(who, pw, now).await,
            Err(PwError::SignInFailed),
            "{who}"
        );
    }
    // Not even an address: the same.
    assert_eq!(
        t.svc
            .sign_in(t.store(), "not an address", &Secret::new(PASSWORD), now)
            .await
            .err(),
        Some(PwError::SignInFailed)
    );
    // By reason in the audit log, never by value.
    let details: Vec<String> = t
        .events("password.sign_in")
        .await
        .into_iter()
        .filter(|e| e.1 == Outcome::Denied)
        .filter_map(|e| e.3)
        .collect();
    for reason in [
        "unknown_name",
        "no_password",
        "wrong_password",
        "deactivated",
    ] {
        assert!(details.iter().any(|d| d == reason), "{reason}: {details:?}");
    }
});

both!(
    a_code_is_used_once_within_its_step_and_the_skew_is_one_step,
    |t| {
        let now = t0();
        let (secret, _) = t.enrolled("ana", now).await;
        let at = now + Duration::minutes(2);
        let step = totp::step_at(at.timestamp());
        // The setup accepted the code of `now`'s step; a new step's code works once.
        let code = totp::code_at(&secret, step);
        let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
        t.code(&token, &code, at).await.unwrap();
        let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
        assert_eq!(
            t.code(&token, &code, at).await,
            Err(PwError::CodeFailed { attempts_left: 2 })
        );
        // The next step's code (a clock 30 s ahead) is accepted; two steps ahead isn't.
        assert_eq!(
            t.code(&token, &totp::code_at(&secret, step + 2), at).await,
            Err(PwError::CodeFailed { attempts_left: 1 })
        );
        t.code(&token, &totp::code_at(&secret, step + 1), at)
            .await
            .unwrap();
        // A code older than the last accepted one is refused too, even inside the window.
        let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
        assert!(t
            .code(&token, &totp::code_at(&secret, step), at)
            .await
            .is_err());
    }
);

both!(the_previous_step_is_accepted_for_a_slow_clock, |t| {
    let now = t0();
    let (secret, _) = t.enrolled("ana", now).await;
    let at = now + Duration::minutes(5);
    let step = totp::step_at(at.timestamp());
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    assert!(t
        .code(&token, &totp::code_at(&secret, step - 2), at)
        .await
        .is_err());
    t.code(&token, &totp::code_at(&secret, step - 1), at)
        .await
        .unwrap();
});

both!(a_recovery_code_works_once, |t| {
    let now = t0();
    let (_, recovery) = t.enrolled("ana", now).await;
    let at = now + Duration::minutes(1);
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    let done = t
        .svc
        .second_step(
            t.store(),
            &token,
            &SecondStep::Recovery(Secret::new(recovery[3].to_uppercase())),
            at,
        )
        .await
        .unwrap();
    assert!(done.recovery_code_used);
    assert_eq!(done.recovery_codes_left, 9);
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    assert_eq!(
        t.svc
            .second_step(
                t.store(),
                &token,
                &SecondStep::Recovery(Secret::new(recovery[3].clone())),
                at
            )
            .await,
        Err(PwError::CodeFailed { attempts_left: 2 })
    );
    let ev = t.events("password.second_step").await;
    assert!(
        ev.iter()
            .any(|e| e.3.as_deref() == Some("recovery_code; recovery_left 9")),
        "{ev:?}"
    );
    assert_eq!(
        t.store()
            .password_status("ana", at)
            .await
            .unwrap()
            .recovery_codes_left,
        9
    );
});

both!(setup_codes_are_single_use_expire_and_are_replaced, |t| {
    let now = t0();
    let first = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    // A new code invalidates the old one.
    let second = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    let begin = |code: String, at| async move {
        t.svc
            .setup_begin(t.store(), "ana@example.com", &Secret::new(code), at)
            .await
            .map(|s| s.kind)
    };
    assert_eq!(begin(first.clone(), now).await, Err(PwError::SetupFailed));
    assert_eq!(begin(second.clone(), now).await, Ok(CodeKind::Setup));
    // Expired after 24 hours.
    assert_eq!(
        begin(second.clone(), now + Duration::hours(24)).await,
        Err(PwError::SetupFailed)
    );
    // Used once: a finished setup spends it.
    let third = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    t.setup("ana", &third, PASSWORD, now).await;
    assert_eq!(begin(third.clone(), now).await, Err(PwError::SetupFailed));
    // A wrong code for a real user, and for nobody, look alike.
    assert_eq!(
        t.svc
            .setup_begin(t.store(), "nobody@example.com", &Secret::new(third), now)
            .await
            .err(),
        Some(PwError::SetupFailed)
    );
    let st = t.store().password_status("ana", now).await.unwrap();
    assert!(!st.setup_code_pending);
});

both!(who_may_issue_a_code, |t| {
    let now = t0();
    let web = Actor::web("ada");
    // An Admin's or Auditor's code comes from the host only.
    assert_eq!(t.issue(&web, "bob", now).await, Err(PwError::HostOnly));
    assert_eq!(t.issue(&web, "aud", now).await, Err(PwError::HostOnly));
    assert!(t.issue(&ops(), "bob", now).await.is_ok());
    assert!(t.issue(&ops(), "aud", now).await.is_ok());
    // Nobody for themselves; only an active admin; never the evals account.
    assert_eq!(t.issue(&web, "ada", now).await, Err(PwError::SelfChange));
    assert_eq!(
        t.issue(&Actor::web("ana"), "ben", now).await,
        Err(PwError::Forbidden)
    );
    assert_eq!(
        t.issue(&Actor::bot("ada"), "ben", now).await,
        Err(PwError::Forbidden)
    );
    assert_eq!(
        t.issue(&ops(), "evals", now).await,
        Err(PwError::EvalsAccount)
    );
    assert_eq!(
        t.issue(&web, "evals", now).await,
        Err(PwError::EvalsAccount)
    );
    assert_eq!(t.issue(&web, "nobody", now).await, Err(PwError::NotFound));
    t.deactivate("cy", now).await;
    assert_eq!(t.issue(&web, "cy", now).await, Err(PwError::NotActive));
    // An unconfirmed sign-in name (a Slack join) can't be issued one.
    t.stores
        .users
        .apply(
            &Actor::bot("ada"),
            Op::CreateUser {
                user: NewUser {
                    handle: "sam".into(),
                    display_name: "Sam".into(),
                    sign_in_name: Some("sam@example.com".into()),
                    atlassian_email: Some("sam@example.com".into()),
                    github_login: None,
                    slack_id: Some("U0SAM".into()),
                    roles: [UserRole::Pm].into(),
                },
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        t.issue(&web, "sam", now).await,
        Err(PwError::SignInUnconfirmed)
    );
    assert_eq!(
        t.issue(&ops(), "sam", now).await,
        Err(PwError::SignInUnconfirmed)
    );
    // Every refusal is audited with its code.
    let ev = t.events("password.setup_code").await;
    for code in [
        "host_only",
        "self_change",
        "forbidden",
        "not_found",
        "not_active",
        "sign_in_unconfirmed",
    ] {
        assert!(
            ev.iter()
                .any(|e| e.1 == Outcome::Denied && e.3.as_deref() == Some(code)),
            "{code}: {ev:?}"
        );
    }
    assert!(
        ev.iter()
            .any(|e| e.3.as_deref() == Some("host_user ops; setup")),
        "{ev:?}"
    );
});

both!(
    a_web_issued_code_is_refused_once_its_user_holds_admin,
    |t| {
        let now = t0();
        let code = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
        t.stores
            .users
            .apply(
                &ops(),
                Op::GrantAdmin {
                    handle: "ana".into(),
                    entra: pmos_toolserver::users::AdminEntra::Keep,
                },
                now,
            )
            .await
            .ok();
        if t.stores
            .users
            .directory(now)
            .await
            .unwrap()
            .get("ana")
            .unwrap()
            .has(UserRole::Admin)
        {
            assert_eq!(
                t.svc
                    .setup_begin(t.store(), "ana@example.com", &Secret::new(code), now)
                    .await
                    .err(),
                Some(PwError::HostOnly)
            );
        } else {
            // grant-admin needs an Entra link; grant the auditor role in the web instead.
            t.stores
                .users
                .apply(
                    &Actor::web("ada"),
                    Op::SetRoles {
                        handle: "ana".into(),
                        roles: [UserRole::Pm, UserRole::Reviewer].into(),
                    },
                    now,
                )
                .await
                .unwrap();
            t.stores
                .users
                .apply(
                    &Actor::web("ada"),
                    Op::SetRoles {
                        handle: "ana".into(),
                        roles: [UserRole::Auditor].into(),
                    },
                    now,
                )
                .await
                .unwrap();
            assert_eq!(
                t.svc
                    .setup_begin(t.store(), "ana@example.com", &Secret::new(code), now)
                    .await
                    .err(),
                Some(PwError::HostOnly)
            );
        }
    }
);

both!(
    failures_wait_longer_up_to_fifteen_minutes_and_answer_alike,
    |t| {
        let now = t0();
        let (secret, _) = t.enrolled("ana", now).await;
        let mut at = now + Duration::minutes(1);
        for _ in 0..4 {
            assert_eq!(
                t.sign_in("ana", OTHER_PASSWORD, at).await,
                Err(PwError::SignInFailed)
            );
        }
        // Four failures: no wait yet.
        t.sign_in("ana", PASSWORD, at).await.unwrap();
        // The fifth: 30 seconds, during which even the right password is the same refusal.
        assert!(t.sign_in("ana", OTHER_PASSWORD, at).await.is_err());
        assert_eq!(
            t.sign_in("ana", PASSWORD, at).await,
            Err(PwError::SignInFailed)
        );
        assert_eq!(
            t.sign_in("ana", PASSWORD, at + Duration::seconds(29)).await,
            Err(PwError::SignInFailed)
        );
        // Attempts during the wait aren't counted: after it, the next failure is the sixth (60 s).
        let mut wait = Duration::seconds(30);
        for _ in 0..8 {
            at += wait;
            assert!(t.sign_in("ana", OTHER_PASSWORD, at).await.is_err());
            wait = (wait * 2).min(Duration::minutes(15));
            assert_eq!(
                t.sign_in("ana", PASSWORD, at + wait - Duration::seconds(1))
                    .await,
                Err(PwError::SignInFailed),
                "still waiting {wait}"
            );
        }
        assert_eq!(
            wait,
            limits::MAX_DELAY,
            "capped at 15 minutes, never permanent"
        );
        // A wrong second-step code counts too; a full sign-in clears it all.
        at += wait;
        let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
        t.code(&token, &code_now(&secret, at), at).await.unwrap();
        at += Duration::minutes(1);
        for _ in 0..4 {
            t.sign_in("ana", OTHER_PASSWORD, at).await.ok();
        }
        let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
        assert_eq!(
            t.code(&token, "000000", at).await,
            Err(PwError::CodeFailed { attempts_left: 2 })
        );
        assert_eq!(
            t.sign_in("ana", PASSWORD, at).await,
            Err(PwError::SignInFailed),
            "5th failure"
        );
        let locks = t.events("password.lockout").await;
        assert!(
            locks.iter().any(|e| e.3.as_deref() == Some("failures 5")),
            "{locks:?}"
        );
        assert!(
            locks.iter().any(|e| e.3.as_deref() == Some("failures 13")),
            "{locks:?}"
        );
    }
);

both!(setup_code_guesses_wait_the_same_way, |t| {
    let now = t0();
    let code = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    let wrong = "AAAAA-AAAAA-AAAAA-AAAAA";
    let begin = |c: &str, at| {
        let c = c.to_string();
        async move {
            t.svc
                .setup_begin(t.store(), "ana@example.com", &Secret::new(c), at)
                .await
                .map(drop)
        }
    };
    for _ in 0..5 {
        assert_eq!(begin(wrong, now).await, Err(PwError::SetupFailed));
    }
    assert_eq!(
        begin(&code, now).await,
        Err(PwError::SetupFailed),
        "waiting"
    );
    assert_eq!(begin(&code, now + Duration::seconds(30)).await, Ok(()));
    // A sign-in isn't held up by setup-code guesses: they have their own counter.
    let ev = t.events("password.lockout").await;
    assert!(
        ev.iter()
            .any(|e| e.3.as_deref() == Some("failures 5; setup")),
        "{ev:?}"
    );
});

both!(parallel_guesses_cannot_pass_the_threshold, |t| {
    let now = t0();
    t.enrolled("ana", now).await;
    let at = now + Duration::minutes(1);
    let tries: Vec<_> = (0..12)
        .map(|_| t.sign_in("ana", OTHER_PASSWORD, at))
        .collect();
    let out = futures_join(tries).await;
    assert!(out.iter().all(|r| *r == Err(PwError::SignInFailed)));
    let wrong = t
        .events("password.sign_in")
        .await
        .into_iter()
        .filter(|e| e.3.as_deref() == Some("wrong_password"))
        .count();
    assert_eq!(wrong, limits::THRESHOLD as usize, "only five were checked");
});

both!(the_pending_token_grants_three_tries_for_five_minutes, |t| {
    let now = t0();
    let (secret, _) = t.enrolled("ana", now).await;
    let at = now + Duration::minutes(1);
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    for left in [2, 1, 0] {
        assert_eq!(
            t.code(&token, "000000", at).await,
            Err(PwError::CodeFailed {
                attempts_left: left
            })
        );
    }
    assert_eq!(
        t.code(&token, &code_now(&secret, at), at).await,
        Err(PwError::SignInExpired)
    );
    let at = at + Duration::minutes(1);
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    let late = at + Duration::minutes(5);
    assert_eq!(
        t.code(&token, &code_now(&secret, late), late).await,
        Err(PwError::SignInExpired)
    );
    // A made-up token, and a setup token, are no sign-in.
    assert_eq!(
        t.code(&"x".repeat(43), "123456", at).await,
        Err(PwError::SignInExpired)
    );
    let c = t.issue(&Actor::web("ada"), "ben", at).await.unwrap();
    let s = t
        .svc
        .setup_begin(t.store(), "ben@example.com", &Secret::new(c), at)
        .await
        .unwrap();
    assert_eq!(
        t.code(s.token.expose(), "123456", at).await,
        Err(PwError::SignInExpired)
    );
});

both!(a_deactivation_after_the_password_ends_the_sign_in, |t| {
    let now = t0();
    let (secret, _) = t.enrolled("ana", now).await;
    let at = now + Duration::minutes(1);
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    t.deactivate("ana", at).await;
    assert_eq!(
        t.code(&token, &code_now(&secret, at), at).await,
        Err(PwError::NotAMember)
    );
    // And a deactivated person can't set up either (a correct code says why).
    let code = t.issue(&ops(), "ben", at).await.unwrap();
    t.deactivate("ben", at).await;
    assert_eq!(
        t.svc
            .setup_begin(t.store(), "ben@example.com", &Secret::new(code), at)
            .await
            .err(),
        Some(PwError::NotAMember)
    );
});

both!(
    a_reset_replaces_password_authenticator_and_recovery_codes,
    |t| {
        let now = t0();
        let (old_secret, old_recovery) = t.enrolled("ana", now).await;
        let at = now + Duration::minutes(1);
        let code = t.issue(&Actor::web("ada"), "ana", at).await.unwrap();
        // The old password still works until the reset is used.
        t.sign_in("ana", PASSWORD, at).await.unwrap();
        let s = t
            .svc
            .setup_begin(t.store(), "ana@example.com", &Secret::new(code.clone()), at)
            .await
            .unwrap();
        assert_eq!(s.kind, CodeKind::Reset);
        let (secret, _) = t.setup("ana", &code, OTHER_PASSWORD, at).await;
        assert_ne!(secret, old_secret);
        let st = t.store().password_status("ana", at).await.unwrap();
        assert_eq!(st.epoch, 2, "every session of theirs ends");
        let later = at + Duration::minutes(1);
        assert_eq!(
            t.sign_in("ana", PASSWORD, later).await,
            Err(PwError::SignInFailed)
        );
        let token = t.sign_in("ana", OTHER_PASSWORD, later).await.unwrap();
        assert!(t
            .svc
            .second_step(
                t.store(),
                &token,
                &SecondStep::Recovery(Secret::new(old_recovery[0].clone())),
                later
            )
            .await
            .is_err());
        assert!(t
            .code(&token, &code_now(&old_secret, later), later)
            .await
            .is_err());
        t.code(&token, &code_now(&secret, later), later)
            .await
            .unwrap();
        let changes = t.stores.users.role_changes(Some("ana"), 10).await.unwrap();
        assert!(changes
            .iter()
            .any(|c| c.change == ChangeKind::SetPassword && c.detail.as_deref() == Some("reset")));
    }
);

both!(change_password_and_regenerate_recovery_codes, |t| {
    let now = t0();
    let (secret, recovery) = t.enrolled("ana", now).await;
    let at = now + Duration::minutes(1);
    let code = Secret::new(code_now(&secret, at));
    let change = |current: &str, code: &Secret, new: &str| {
        let (current, new) = (Secret::new(current), Secret::new(new));
        let code = Secret::new(code.expose());
        async move {
            t.svc
                .change(t.store(), "ana", &current, &code, &new, at)
                .await
        }
    };
    assert_eq!(
        change(OTHER_PASSWORD, &code, "yet another long one").await,
        Err(PwError::ReauthFailed)
    );
    assert_eq!(
        change(PASSWORD, &Secret::new("000000"), "yet another long one").await,
        Err(PwError::ReauthFailed)
    );
    assert_eq!(
        change(PASSWORD, &code, "short").await,
        Err(PwError::Weak(hash::Weak::TooShort))
    );
    assert_eq!(
        change(PASSWORD, &code, "ana@example.com").await,
        Err(PwError::Weak(hash::Weak::MatchesName))
    );
    assert_eq!(
        change(PASSWORD, &code, "password1234").await,
        Err(PwError::Weak(hash::Weak::Common))
    );
    assert_eq!(change(PASSWORD, &code, "yet another long one").await, Ok(2));
    assert_eq!(
        t.sign_in("ana", PASSWORD, at).await,
        Err(PwError::SignInFailed)
    );
    // Recovery codes: an authenticator code replaces all ten.
    let next = at + Duration::minutes(1);
    let fresh = t
        .svc
        .regenerate_recovery_codes(
            t.store(),
            "ana",
            &Secret::new(code_now(&secret, next)),
            next,
        )
        .await
        .unwrap();
    assert_eq!(fresh.len(), 10);
    let token = t
        .sign_in("ana", "yet another long one", next)
        .await
        .unwrap();
    assert!(t
        .svc
        .second_step(
            t.store(),
            &token,
            &SecondStep::Recovery(Secret::new(recovery[0].clone())),
            next
        )
        .await
        .is_err());
    t.svc
        .second_step(
            t.store(),
            &token,
            &SecondStep::Recovery(Secret::new(fresh[0].expose())),
            next,
        )
        .await
        .unwrap();
    assert_eq!(
        t.svc
            .regenerate_recovery_codes(t.store(), "ana", &Secret::new("000000"), next)
            .await
            .err(),
        Some(PwError::ReauthFailed)
    );
    assert_eq!(
        t.svc
            .change(
                t.store(),
                "ben",
                &Secret::new(PASSWORD),
                &code,
                &Secret::new("whatever long one"),
                next
            )
            .await,
        Err(PwError::NoPassword)
    );
});

both!(raised_parameters_rehash_at_the_next_sign_in, |t| {
    let now = t0();
    let (secret, _) = t.enrolled("ana", now).await;
    let stronger = HashParams {
        m_kib: 2048,
        t: 2,
        p: 1,
    };
    let raised = PasswordService::with_params(t.keys.clone(), stronger).unwrap();
    let at = now + Duration::minutes(1);
    let token = raised
        .sign_in(t.store(), "ana@example.com", &Secret::new(PASSWORD), at)
        .await
        .unwrap();
    raised
        .second_step(
            t.store(),
            token.token.expose(),
            &SecondStep::Code(Secret::new(code_now(&secret, at))),
            at,
        )
        .await
        .unwrap();
    let ev = t.events("password.sign_in").await;
    assert!(
        ev.iter()
            .any(|e| e.3.as_deref() == Some("password; rehashed")),
        "{ev:?}"
    );
    // Next time, at the raised parameters, nothing to do; the old service still verifies it.
    let again = raised
        .sign_in(t.store(), "ana@example.com", &Secret::new(PASSWORD), at)
        .await;
    assert!(again.is_ok());
    let n = t
        .events("password.sign_in")
        .await
        .iter()
        .filter(|e| e.3.as_deref() == Some("password; rehashed"))
        .count();
    assert_eq!(n, 1);
    assert!(t.sign_in("ana", PASSWORD, at).await.is_ok());
});

both!(the_global_limit_holds_every_caller, |t| {
    let svc = PasswordService::with_params(t.keys.clone(), QUICK)
        .unwrap()
        .with_global_limit(3);
    let now = t0();
    for _ in 0..3 {
        assert_eq!(
            svc.sign_in(t.store(), "nobody@example.com", &Secret::new(PASSWORD), now)
                .await
                .err(),
            Some(PwError::SignInFailed)
        );
    }
    assert_eq!(
        svc.sign_in(t.store(), "nobody@example.com", &Secret::new(PASSWORD), now)
            .await
            .err(),
        Some(PwError::TooMany { retry_after: 60 })
    );
    assert_eq!(
        svc.second_step(
            t.store(),
            &"x".repeat(43),
            &SecondStep::Code(Secret::new("1")),
            now
        )
        .await
        .err(),
        Some(PwError::TooMany { retry_after: 60 })
    );
    assert!(svc
        .sign_in(
            t.store(),
            "nobody@example.com",
            &Secret::new(PASSWORD),
            now + Duration::minutes(1)
        )
        .await
        .is_err_and(|e| e == PwError::SignInFailed));
});

both!(the_grace_end_deletes_the_credential, |t| {
    let now = t0();
    t.enrolled("ana", now).await;
    t.deactivate("ana", now).await;
    t.stores
        .users
        .apply(
            &Actor::system(),
            Op::EndGrace {
                handle: "ana".into(),
            },
            now + Duration::hours(25),
        )
        .await
        .unwrap();
    let st = t.store().password_status("ana", now).await.unwrap();
    assert!(!st.password_set && !st.second_step_enrolled && st.recovery_codes_left == 0);
});

both!(nothing_secret_reaches_the_audit_log, |t| {
    let now = t0();
    let code = t.issue(&Actor::web("ada"), "ana", now).await.unwrap();
    let (secret, recovery) = t.setup("ana", &code, PASSWORD, now).await;
    let at = now + Duration::minutes(1);
    t.sign_in("ana", OTHER_PASSWORD, at).await.ok();
    let token = t.sign_in("ana", PASSWORD, at).await.unwrap();
    t.code(&token, "000000", at).await.ok();
    t.code(&token, &code_now(&secret, at), at).await.unwrap();
    let all = t.all_events().await.join("\n");
    let b32 = totp::base32(&secret);
    for secret_value in [
        PASSWORD,
        OTHER_PASSWORD,
        code.as_str(),
        token.as_str(),
        b32.as_str(),
        recovery[0].as_str(),
        "ana@example.com",
    ] {
        assert!(
            !all.contains(secret_value),
            "an audit event holds a secret value"
        );
    }
    for (_, _, _, detail) in t
        .events("password.sign_in")
        .await
        .into_iter()
        .chain(t.events("password.second_step").await)
    {
        let d = detail.unwrap();
        assert!(pmos_toolserver::password::is_safe_detail(&d), "{d}");
    }
});

/// The real parameters: OWASP's minimum, stored in the PHC string.
#[test]
fn the_real_parameters_parse_from_the_stored_hash() {
    let h = hash::hash(PASSWORD.as_bytes(), hash::CURRENT).unwrap();
    assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
    assert_eq!(hash::params_of(&h), Some(hash::CURRENT));
    assert!(hash::verify(PASSWORD.as_bytes(), &h));
    assert!(!hash::needs_rehash(&h, hash::CURRENT));
    assert!(hash::needs_rehash(
        &h,
        HashParams {
            m_kib: 47_104,
            t: 1,
            p: 1
        }
    ));
}

async fn futures_join<F: std::future::Future>(fs: Vec<F>) -> Vec<F::Output> {
    let mut out = Vec::new();
    let pinned: Vec<_> = fs.into_iter().map(Box::pin).collect();
    let mut pending: Vec<_> = pinned.into_iter().map(Some).collect();
    // A small join_all without another dependency: poll all until each is done.
    std::future::poll_fn(|cx| {
        let mut all = true;
        for slot in pending.iter_mut() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        out.push(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all = false,
                }
            }
        }
        if all {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    out
}

// --- Over HTTP --------------------------------------------------------------------------------

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test keys only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const CONSOLE_KEY: &str = "Y29uc29sZS10b29sc2VydmVyLXRlc3Qta2V5LTAxMjM=";

struct Http {
    _dir: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cancel: CancellationToken,
    t: T,
}

impl Http {
    async fn start(postgres: bool, require: bool, passwords: bool) -> Option<Self> {
        let t = T::new(postgres).await?;
        let (dir, root) = common::content_tree();
        let _dir = dir;
        let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
        let connectors: Vec<Arc<dyn Connector>> = vec![
            Arc::new(FakeConnector::jira()),
            Arc::new(FakeConnector::confluence()),
            Arc::new(FakeConnector::slack()),
            Arc::new(FakeConnector::docs()),
        ];
        let mut svc = ToolService::new(catalog, t.stores.clone(), connectors).unwrap();
        if passwords {
            svc = svc.with_passwords(Arc::new(PasswordService::new(t.keys.clone()).unwrap()));
        }
        let keys = format!("web:toolserver:w1:{WEB_KEY},console:toolserver:c1:{CONSOLE_KEY}");
        let assertions = if require {
            let keys = parse_keys(&keys, "the tool server", |iss, aud| {
                aud == AUDIENCE && ISSUERS.contains(&iss)
            })
            .unwrap();
            Assertions::new(Mode::Require, keys).unwrap()
        } else {
            let lookup = move |k: &str| match k {
                "PMOS_ASSERTION_MODE" => Some("off".to_string()),
                "PMOS_ASSERTION_INSECURE_OFF" => Some("1".to_string()),
                _ => None,
            };
            Assertions::from_lookup(lookup, &[("web", "PMOS_WEB_TOKEN")]).unwrap()
        };
        let cancel = CancellationToken::new();
        let app = router_full(
            Arc::new(svc),
            AppConfig {
                service_token: SERVICE_TOKEN.into(),
                bot: Some(BotAccess {
                    token: BOT_TOKEN.into(),
                    skills: vec!["docs".into()],
                }),
                console_token: Some(CONSOLE_TOKEN.into()),
                content_root: root.clone(),
                oauth: None,
                allowed_hosts: vec!["127.0.0.1".into()],
            },
            None,
            PersonClients {
                web_token: Some(WEB_TOKEN.into()),
                assertions: Arc::new(assertions),
            },
            cancel.clone(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let ct = cancel.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await
                .unwrap();
        });
        Some(Self {
            _dir,
            base,
            http: reqwest::Client::new(),
            cancel,
            t,
        })
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.t.stop().await;
    }

    async fn send(
        &self,
        method: Method,
        target: &str,
        token: Option<&str>,
        assertion: Option<&str>,
        body: Option<&Value>,
    ) -> (u16, Value, reqwest::header::HeaderMap) {
        let mut r = self.http.request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(serde_json::to_vec(b).unwrap());
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            headers,
        )
    }

    async fn web(&self, path: &str, body: Value) -> (u16, Value) {
        let (s, b, _) = self
            .send(Method::POST, path, Some(WEB_TOKEN), None, Some(&body))
            .await;
        (s, b)
    }

    /// `pmos-web` for signed-in `actor`, with its assertion.
    async fn signed(
        &self,
        actor: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let target = format!("{path}?user={actor}");
        let bytes = body
            .as_ref()
            .map(|b| serde_json::to_vec(b).unwrap())
            .unwrap_or_default();
        let ring = AssertionKeys::new(Key::from_base64("w1", WEB_KEY).unwrap());
        let claims = Claims::new("web", "toolserver", actor, &[], "s-test").unwrap();
        let binding = RequestBinding::new(method.as_str(), &target, &bytes).unwrap();
        let a = ring.sign(&claims, unix_now(), 60, Some(&binding)).unwrap();
        let (s, b, _) = self
            .send(method, &target, Some(WEB_TOKEN), Some(&a), body.as_ref())
            .await;
        (s, b)
    }
}

macro_rules! http {
    ($name:ident, $require:expr, $passwords:expr, |$s:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            for postgres in [false, true] {
                let Some(server) = Http::start(postgres, $require, $passwords).await else {
                    continue;
                };
                {
                    let $s = &server;
                    $body
                }
                server.stop().await;
            }
        }
    };
}

const ROUTES: [&str; 10] = [
    "/internal/password/sign-in",
    "/internal/password/second-step",
    "/internal/password/setup/begin",
    "/internal/password/setup/password",
    "/internal/password/setup/authenticator",
    "/internal/password/setup/confirm",
    "/internal/password/change",
    "/internal/password/recovery-codes",
    "/internal/password/setup-codes",
    "/internal/password/status",
];

http!(
    only_the_web_token_reaches_the_password_routes,
    true,
    true,
    |s| {
        for route in ROUTES {
            let method = if route.ends_with("/status") {
                Method::GET
            } else {
                Method::POST
            };
            for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
                let (status, _, _) = s
                    .send(method.clone(), route, Some(token), None, Some(&json!({})))
                    .await;
                assert_eq!(status, 403, "{route}");
            }
            let (status, _, _) = s
                .send(method.clone(), route, None, None, Some(&json!({})))
                .await;
            assert_eq!(status, 401, "{route}");
            let (status, _, _) = s
                .send(
                    method.clone(),
                    route,
                    Some("not-a-token-at-all-0123456789abcdef"),
                    None,
                    Some(&json!({})),
                )
                .await;
            assert_eq!(status, 401, "{route}");
            let (status, body, _) = s
                .send(method, route, Some(WEB_TOKEN), None, Some(&json!({})))
                .await;
            assert!(
                ![401, 403].contains(&status) || body["code"] == "bad_assertion",
                "{route}: {status} {body}"
            );
        }
    }
);

http!(nothing_runs_without_assertion_require, false, true, |s| {
    for route in ROUTES {
        let method = if route.ends_with("/status") {
            Method::GET
        } else {
            Method::POST
        };
        let (status, body, _) = s
            .send(method, route, Some(WEB_TOKEN), None, Some(&json!({})))
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (503, Some("assertion_required")),
            "{route}"
        );
    }
});

http!(
    without_a_vault_key_password_sign_in_is_off,
    true,
    false,
    |s| {
        let (status, body) = s
            .web(
                "/internal/password/sign-in",
                json!({"sign_in_name": "ana@example.com", "password": PASSWORD}),
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (501, Some("not_configured"))
        );
    }
);

http!(
    the_routes_end_to_end_with_one_answer_for_every_refusal,
    true,
    true,
    |s| {
        // An admin issues a code (signed), the person sets up, signs in.
        let (status, issued) = s
            .signed(
                "ada",
                Method::POST,
                "/internal/password/setup-codes",
                Some(json!({"handle": "ana"})),
            )
            .await;
        assert_eq!(status, 201, "{issued}");
        assert_eq!(issued["kind"], "setup");
        let code = issued["setup_code"].as_str().unwrap().to_string();
        // Signed for someone else: refused.
        let (status, body) = s
            .signed(
                "ana",
                Method::POST,
                "/internal/password/setup-codes",
                Some(json!({"handle": "ben"})),
            )
            .await;
        assert_eq!((status, body["code"].as_str()), (403, Some("forbidden")));
        let (status, body) = s
            .signed(
                "ada",
                Method::POST,
                "/internal/password/setup-codes",
                Some(json!({"handle": "bob"})),
            )
            .await;
        assert_eq!((status, body["code"].as_str()), (403, Some("host_only")));
        // No assertion on a signed-in route: 401.
        let (status, _, _) = s
            .send(
                Method::POST,
                "/internal/password/setup-codes?user=ada",
                Some(WEB_TOKEN),
                None,
                Some(&json!({"handle": "ana"})),
            )
            .await;
        assert_eq!(status, 401);

        let (status, begun) = s
            .web(
                "/internal/password/setup/begin",
                json!({"sign_in_name": "Ana@Example.com", "setup_code": code}),
            )
            .await;
        assert_eq!(status, 200, "{begun}");
        let setup = begun["setup_token"].as_str().unwrap().to_string();
        let (status, body) = s
            .web(
                "/internal/password/setup/authenticator",
                json!({"setup_token": setup}),
            )
            .await;
        assert_eq!((status, body["code"].as_str()), (409, Some("wrong_step")));
        let (status, body) = s
            .web(
                "/internal/password/setup/password",
                json!({"setup_token": setup, "password": "password1234"}),
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str(), body["reason"].as_str()),
            (422, Some("weak_password"), Some("common"))
        );
        let (status, _) = s
            .web(
                "/internal/password/setup/password",
                json!({"setup_token": setup, "password": PASSWORD}),
            )
            .await;
        assert_eq!(status, 200);
        let (status, enrol) = s
            .web(
                "/internal/password/setup/authenticator",
                json!({"setup_token": setup}),
            )
            .await;
        assert_eq!(status, 200, "{enrol}");
        assert!(enrol["otpauth_uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/PM%20OS:ana@example.com?secret="));
        assert_eq!(
            (enrol["digits"].as_u64(), enrol["period"].as_u64()),
            (Some(6), Some(30))
        );
        let secret = totp::unbase32(enrol["secret"].as_str().unwrap())
            .unwrap()
            .to_vec();
        let (status, done) = s
            .web(
                "/internal/password/setup/confirm",
                json!({"setup_token": setup, "code": code_now(&secret, Utc::now())}),
            )
            .await;
        assert_eq!(status, 200, "{done}");
        assert_eq!(done["recovery_codes"].as_array().unwrap().len(), 10);
        assert_eq!(
            (
                done["user"]["handle"].as_str(),
                done["password_epoch"].as_i64()
            ),
            (Some("ana"), Some(1))
        );

        // The four refusals at the password step: one status, one body.
        s.t.enrolled("cy", Utc::now()).await;
        s.t.deactivate("cy", Utc::now()).await;
        let mut answers = Vec::new();
        for (name, pw) in [
            ("nobody@example.com", PASSWORD),
            ("ben@example.com", PASSWORD),
            ("ana@example.com", OTHER_PASSWORD),
            ("cy@example.com", PASSWORD),
        ] {
            answers.push(
                s.web(
                    "/internal/password/sign-in",
                    json!({"sign_in_name": name, "password": pw}),
                )
                .await,
            );
        }
        for a in &answers {
            assert_eq!(a, &answers[0]);
        }
        assert_eq!(
            (answers[0].0, answers[0].1["code"].as_str()),
            (401, Some("sign_in_failed"))
        );

        let (status, started) = s
            .web(
                "/internal/password/sign-in",
                json!({"sign_in_name": "ana@example.com", "password": PASSWORD}),
            )
            .await;
        assert_eq!(status, 200, "{started}");
        let pending = started["pending_token"].as_str().unwrap().to_string();
        let (status, body) = s
            .web(
                "/internal/password/second-step",
                json!({"pending_token": pending, "code": "000000"}),
            )
            .await;
        assert_eq!(
            (
                status,
                body["code"].as_str(),
                body["attempts_left"].as_i64()
            ),
            (401, Some("code_failed"), Some(2))
        );
        let (status, body) = s
            .web(
                "/internal/password/second-step",
                json!({"pending_token": pending, "code": "1", "recovery_code": "x"}),
            )
            .await;
        assert_eq!(status, 400, "{body}");
        let recovery = done["recovery_codes"][0].as_str().unwrap();
        let (status, signed) = s
            .web(
                "/internal/password/second-step",
                json!({"pending_token": pending, "recovery_code": recovery}),
            )
            .await;
        assert_eq!(status, 200, "{signed}");
        assert_eq!(signed["user"]["handle"], "ana");
        assert_eq!(signed["recovery_codes_left"], 9);
        assert!(signed["user"].get("sign_in_name").is_none());

        // Her own status, signed; the Team page's booleans; resolve's epoch for pmos-web only.
        let (status, st) = s
            .signed("ana", Method::GET, "/internal/password/status", None)
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            st,
            json!({"enabled": true, "password_set": true, "second_step_enrolled": true,
                          "setup_code_pending": false, "recovery_codes_left": 9})
        );
        let (_, list) = s
            .signed("ada", Method::GET, "/internal/admin/users", None)
            .await;
        let ana = list["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["handle"] == "ana")
            .unwrap();
        assert_eq!(
            (
                ana["password_set"].as_bool(),
                ana["second_step_enrolled"].as_bool(),
                ana["setup_code_pending"].as_bool()
            ),
            (Some(true), Some(true), Some(false))
        );
        let (_, resolved, _) = s
            .send(
                Method::GET,
                "/internal/users/resolve?by=handle&handle=ana",
                Some(WEB_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(resolved["password_epoch"], 1);
        let (_, resolved, _) = s
            .send(
                Method::GET,
                "/internal/users/resolve?by=handle&handle=ben",
                Some(WEB_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(resolved["password_epoch"], Value::Null);
        let (_, resolved, _) = s
            .send(
                Method::GET,
                "/internal/users/resolve?by=handle&handle=ana",
                Some(CONSOLE_TOKEN),
                None,
                None,
            )
            .await;
        assert!(resolved.get("password_epoch").is_none(), "{resolved}");

        // Change and regenerate, signed.
        let later = Utc::now() + Duration::seconds(31);
        let (status, body) = s
        .signed("ana", Method::POST, "/internal/password/change",
                Some(json!({"current_password": OTHER_PASSWORD, "code": "000000", "new_password": "a brand new passphrase"})))
        .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (403, Some("reauth_failed"))
        );
        let (status, body) = s
            .signed(
                "ana",
                Method::POST,
                "/internal/password/recovery-codes",
                Some(json!({"code": code_now(&secret, later)})),
            )
            .await;
        assert!(
            status == 200 || body["code"] == "reauth_failed",
            "{status} {body}"
        );
    }
);

http!(no_log_line_holds_a_secret, true, true, |s| {
    let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = {
        let logs = logs.clone();
        move || LogWriter(logs.clone())
    };
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (_, issued) = s
        .signed(
            "ada",
            Method::POST,
            "/internal/password/setup-codes",
            Some(json!({"handle": "ana"})),
        )
        .await;
    let code = issued["setup_code"].as_str().unwrap().to_string();
    let (_, begun) = s
        .web(
            "/internal/password/setup/begin",
            json!({"sign_in_name": "ana@example.com", "setup_code": code}),
        )
        .await;
    let setup = begun["setup_token"].as_str().unwrap().to_string();
    s.web(
        "/internal/password/setup/password",
        json!({"setup_token": setup, "password": PASSWORD}),
    )
    .await;
    let (_, enrol) = s
        .web(
            "/internal/password/setup/authenticator",
            json!({"setup_token": setup}),
        )
        .await;
    let b32 = enrol["secret"].as_str().unwrap().to_string();
    let secret = totp::unbase32(&b32).unwrap().to_vec();
    let (_, done) = s
        .web(
            "/internal/password/setup/confirm",
            json!({"setup_token": setup, "code": code_now(&secret, Utc::now())}),
        )
        .await;
    let recovery = done["recovery_codes"][0].as_str().unwrap().to_string();
    s.web(
        "/internal/password/sign-in",
        json!({"sign_in_name": "ana@example.com", "password": OTHER_PASSWORD}),
    )
    .await;
    let (_, started) = s
        .web(
            "/internal/password/sign-in",
            json!({"sign_in_name": "ana@example.com", "password": PASSWORD}),
        )
        .await;
    let pending = started["pending_token"].as_str().unwrap().to_string();
    s.web(
        "/internal/password/second-step",
        json!({"pending_token": pending, "code": "000000"}),
    )
    .await;
    s.web(
        "/internal/password/second-step",
        json!({"pending_token": pending, "recovery_code": recovery}),
    )
    .await;
    let text = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("password route refused"),
        "the logs were captured"
    );
    for value in [
        PASSWORD,
        OTHER_PASSWORD,
        &code,
        &setup,
        &b32,
        &recovery,
        &pending,
        "ana@example.com",
        WEB_TOKEN,
    ] {
        assert!(!text.contains(value), "a log line holds a secret value");
    }
});

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
