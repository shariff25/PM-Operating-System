//! The user database (`docs/user-database-spec.md`, slice U1).
//!
//! The shared cases run against both stores, in memory and in Postgres (with
//! `PMOS_TEST_DATABASE_URL`), so they behave the same. The `trigger_*` cases run in Postgres
//! only and issue raw SQL with no application lock and no application checks: they prove the
//! database's triggers are the guarantee (migration 0015).

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::TestPg;
use pmos_toolserver::audit::{AuditEvent, AuditQuery, AuditSink, Client, MemoryAudit, Outcome};
use pmos_toolserver::users::rules::{self, Rules};
use pmos_toolserver::users::{
    Actor, AdminEntra, ChangeKind, Directory, EntraKey, Field, HostUser, IdentityKind, ImportMode,
    ImportRequest, MemoryUsers, NewUser, Op, Refusal, Status, UserError, UserRole, UserStore, Via,
};
use sqlx::Row;

const TID: &str = "11111111-2222-3333-4444-555555555555";
const OID_RAJ: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const OID_ANA: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const OID_OTHER: &str = "aaaaaaaa-0000-0000-0000-000000000009";

fn host() -> Actor {
    Actor::host(HostUser::parse("opsuser").unwrap())
}

fn key(oid: &str) -> EntraKey {
    EntraKey {
        tid: TID.into(),
        oid: oid.into(),
    }
}

fn person(handle: &str, roles: &[UserRole]) -> NewUser {
    NewUser {
        handle: handle.into(),
        display_name: format!("{handle} <b>Name</b>\u{7}"),
        sign_in_name: Some(format!("{handle}@contoso.com")),
        atlassian_email: Some(format!("{handle}@contoso.com")),
        github_login: Some(format!("{handle}-gh")),
        slack_id: Some(format!(
            "U0{}",
            handle.to_uppercase().replace(['-', '.'], "")
        )),
        roles: roles.iter().copied().collect(),
    }
}

fn roles(r: &[UserRole]) -> BTreeSet<UserRole> {
    r.iter().copied().collect()
}

fn refusal<T: std::fmt::Debug>(r: Result<T, UserError>) -> Refusal {
    match r {
        Err(UserError::Refused { refusal, .. }) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// One store under test, with what the shared cases need to look inside it.
struct Env {
    store: Arc<dyn UserStore>,
    audit: Arc<MemoryAudit>,
    pg: Option<TestPg>,
}

impl Env {
    fn memory() -> Self {
        let audit = Arc::new(MemoryAudit::default());
        Self {
            store: Arc::new(MemoryUsers::default().with_audit(audit.clone())),
            audit,
            pg: None,
        }
    }

    async fn postgres() -> Option<Self> {
        let pg = TestPg::new().await?;
        Some(Self {
            store: Arc::new(pg.store.clone()),
            audit: Arc::new(MemoryAudit::default()),
            pg: Some(pg),
        })
    }

    async fn events(&self) -> Vec<AuditEvent> {
        match &self.pg {
            None => self.audit.events(),
            Some(pg) => {
                let mut found = pg
                    .store
                    .search(&AuditQuery {
                        limit: 1000,
                        ..Default::default()
                    })
                    .await
                    .unwrap()
                    .unwrap();
                found.sort_by_key(|s| s.id);
                found.into_iter().map(|s| s.event).collect()
            }
        }
    }

    async fn dir(&self) -> Directory {
        self.store.directory(Utc::now()).await.unwrap()
    }

    async fn apply(&self, actor: &Actor, op: Op) -> Result<(), UserError> {
        self.store.apply(actor, op, Utc::now()).await.map(|_| ())
    }

    async fn apply_at(&self, actor: &Actor, op: Op, at: DateTime<Utc>) -> Result<(), UserError> {
        self.store.apply(actor, op, at).await.map(|_| ())
    }

    /// Make the open pilot exception lapse; returns a time after it lapsed. In memory, time
    /// moves on (31 days, for an exception of 30); in Postgres, whose trigger reads the
    /// database's clock, the row is moved into the past with the trigger off for that one
    /// statement: a fixture standing in for a month going by.
    async fn lapse_exception(&self, switched_on: DateTime<Utc>) -> DateTime<Utc> {
        match &self.pg {
            None => switched_on + Duration::days(31),
            Some(pg) => {
                let mut tx = pg.store.pool().begin().await.unwrap();
                for sql in [
                    "ALTER TABLE pilot_exceptions DISABLE TRIGGER users_invariants",
                    "UPDATE pilot_exceptions SET enabled_at = now() - interval '31 days',
                         expires_at = now() - interval '1 day' WHERE ended_at IS NULL",
                    "ALTER TABLE pilot_exceptions ENABLE TRIGGER users_invariants",
                ] {
                    sqlx::query(sql).execute(&mut *tx).await.unwrap();
                }
                tx.commit().await.unwrap();
                Utc::now()
            }
        }
    }

    async fn done(self) {
        if let Some(pg) = self.pg {
            pg.drop_schema().await;
        }
    }

    /// `raj` as the first admin (host-confirmed Entra), plus `others` as PMs.
    async fn with_team(&self, others: &[&str]) {
        self.apply(
            &host(),
            Op::BootstrapAdmin {
                user: person("raj", &[]),
                entra: Some(key(OID_RAJ)),
            },
        )
        .await
        .unwrap();
        for h in others {
            self.apply(
                &host(),
                Op::CreateUser {
                    user: person(h, &[UserRole::Pm]),
                },
            )
            .await
            .unwrap();
        }
    }
}

macro_rules! both {
    ($($case:ident),* $(,)?) => {$(
        mod $case {
            #[tokio::test]
            async fn memory() {
                let env = super::Env::memory();
                super::$case(&env).await;
                env.done().await;
            }
            #[tokio::test]
            async fn postgres() {
                let Some(env) = super::Env::postgres().await else { return };
                super::$case(&env).await;
                env.done().await;
            }
        }
    )*};
}

both!(
    bootstrap_admin_once_and_host_user_recorded,
    last_admin_is_kept,
    reactivating_a_former_admin_leaves_them_without_admin,
    pilot_exception_lifecycle,
    lapsed_exception_resolves_and_keeps_an_admin,
    lapsed_exception_only_admin_keeps_admin,
    deactivation_cap_refused_attempt_does_not_count,
    deactivation_cap_reactivation_does_not_reset,
    client_rules,
    evals_account_rule,
    unique_keys_and_addresses_never_reused,
    grant_admin_entra_rules,
    import_bootstrap_idempotent_and_conflicts,
    import_refusals_write_nothing,
    import_without_allow_admins,
    role_changes_retention,
);

async fn bootstrap_admin_once_and_host_user_recorded(env: &Env) {
    env.with_team(&[]).await;
    let dir = env.dir().await;
    let raj = dir.get("raj").unwrap();
    assert_eq!(raj.role_set(), roles(&[UserRole::Admin, UserRole::Pm]));
    assert_eq!(
        raj.display_name, "raj bName/b",
        "untrusted text is cleaned on write"
    );
    assert!(raj.sign_in_name_confirmed_at.is_some());
    let entra = raj.identity(IdentityKind::Entra).unwrap();
    assert!(entra.host_confirmed && entra.issuer == TID && entra.subject == OID_RAJ);
    assert_eq!(raj.entra_state(), "linked");

    // Once an active admin exists, bootstrap-admin is refused.
    let again = env
        .apply(
            &host(),
            Op::BootstrapAdmin {
                user: person("ana", &[]),
                entra: None,
            },
        )
        .await;
    assert_eq!(refusal(again), Refusal::AdminExists);

    // Every row records the OS user; none carries an address or an oid.
    let changes = env.store.role_changes(Some("raj"), 100).await.unwrap();
    assert!(changes.len() >= 4, "{changes:?}");
    for c in &changes {
        assert_eq!(c.via, Via::Host);
        assert_eq!(c.host_user.as_deref(), Some("opsuser"));
        assert_eq!(c.actor, "host");
        let d = c.detail.clone().unwrap_or_default();
        assert!(!d.contains('@') && !d.contains(OID_RAJ), "{d}");
    }
    let kinds: Vec<ChangeKind> = changes.iter().map(|c| c.change).collect();
    assert!(kinds.contains(&ChangeKind::Create) && kinds.contains(&ChangeKind::Grant));
    // The audit events: one ok, one denied, both naming the host user, as client `host`.
    let events = env.events().await;
    let ok = events
        .iter()
        .find(|e| e.tool == "users.bootstrap_admin" && e.outcome == Outcome::Ok)
        .unwrap();
    assert_eq!(ok.client, Client::Host);
    assert_eq!(ok.output_ref.as_deref(), Some("raj"));
    assert!(ok
        .detail
        .as_deref()
        .unwrap()
        .starts_with("host_user opsuser"));
    let denied = events
        .iter()
        .find(|e| e.tool == "users.bootstrap_admin" && e.outcome == Outcome::Denied)
        .unwrap();
    assert_eq!(
        denied.detail.as_deref(),
        Some("host_user opsuser; admin_exists")
    );
    for e in &events {
        let d = e.detail.as_deref().unwrap_or_default();
        assert!(pmos_toolserver::users::detail::is_safe(d), "{d}");
    }
    assert!(env.dir().await.version > 1);
}

async fn last_admin_is_kept(env: &Env) {
    env.with_team(&["ana"]).await;
    let only = env
        .apply(
            &host(),
            Op::RevokeAdmin {
                handle: "raj".into(),
            },
        )
        .await;
    assert_eq!(refusal(only), Refusal::LastAdmin);
    let deactivate = env
        .apply(
            &host(),
            Op::Deactivate {
                handle: "raj".into(),
            },
        )
        .await;
    assert_eq!(refusal(deactivate), Refusal::LastAdmin);
    assert!(env.dir().await.get("raj").unwrap().has(UserRole::Admin));

    // With a second admin, one of them may go; the self-change rule stops a web admin from
    // demoting themselves.
    env.apply(
        &host(),
        Op::GrantAdmin {
            handle: "ana".into(),
            entra: AdminEntra::Oid(key(OID_ANA)),
        },
    )
    .await
    .unwrap();
    let selfish = env
        .apply(
            &Actor::web("ana"),
            Op::RevokeAdmin {
                handle: "ana".into(),
            },
        )
        .await;
    assert_eq!(refusal(selfish), Refusal::SelfChange);
    env.apply(
        &Actor::web("ana"),
        Op::RevokeAdmin {
            handle: "raj".into(),
        },
    )
    .await
    .unwrap();
    let last = env
        .apply(
            &host(),
            Op::RevokeAdmin {
                handle: "ana".into(),
            },
        )
        .await;
    assert_eq!(refusal(last), Refusal::LastAdmin);
}

async fn reactivating_a_former_admin_leaves_them_without_admin(env: &Env) {
    env.with_team(&["ana"]).await;
    env.apply(
        &host(),
        Op::GrantAdmin {
            handle: "ana".into(),
            entra: AdminEntra::Oid(key(OID_ANA)),
        },
    )
    .await
    .unwrap();
    env.apply(
        &host(),
        Op::Deactivate {
            handle: "ana".into(),
        },
    )
    .await
    .unwrap();
    let ana = env.dir().await.get("ana").cloned().unwrap();
    assert_eq!(ana.status, Status::Deactivated);
    assert!(!ana.has(UserRole::Admin), "deactivation revokes admin");
    assert!(ana.has(UserRole::Pm), "other roles are kept");
    let revoke = env
        .store
        .role_changes(Some("ana"), 10)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.change == ChangeKind::Revoke)
        .unwrap();
    assert_eq!(revoke.role, Some(UserRole::Admin));
    assert_eq!(revoke.detail.as_deref(), Some("on deactivation"));

    env.apply(
        &host(),
        Op::Reactivate {
            handle: "ana".into(),
        },
    )
    .await
    .unwrap();
    let ana = env.dir().await.get("ana").cloned().unwrap();
    assert!(ana.active() && ana.deactivated_at.is_none());
    assert!(
        !ana.has(UserRole::Admin),
        "reactivation never restores admin"
    );
    assert_eq!(ana.role_set(), roles(&[UserRole::Pm]));
}

async fn pilot_exception_lifecycle(env: &Env) {
    env.with_team(&["ana"]).await;
    let now = Utc::now();
    let add_auditor = || Op::SetRoles {
        handle: "raj".into(),
        roles: roles(&[UserRole::Admin, UserRole::Pm, UserRole::Auditor]),
    };
    // No exception: refused.
    assert_eq!(
        refusal(env.apply(&host(), add_auditor()).await),
        Refusal::DualRole
    );
    // An admin can't hold auditor through grant-admin either.
    env.apply(
        &host(),
        Op::SetRoles {
            handle: "ana".into(),
            roles: roles(&[UserRole::Pm, UserRole::Auditor]),
        },
    )
    .await
    .unwrap();
    let grant = env
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ana".into(),
                entra: AdminEntra::Oid(key(OID_ANA)),
            },
        )
        .await;
    assert_eq!(refusal(grant), Refusal::DualRole);
    // --until more than 90 days ahead, or in the past: refused.
    let long = env
        .apply(
            &host(),
            Op::ExceptionOn {
                until: now + Duration::days(91),
            },
        )
        .await;
    assert_eq!(refusal(long), Refusal::ExceptionTooLong);
    let past = env
        .apply(
            &host(),
            Op::ExceptionOn {
                until: now - Duration::days(1),
            },
        )
        .await;
    assert_eq!(refusal(past), Refusal::Invalid);
    // The web can't switch it.
    let web = env
        .apply(
            &Actor::web("raj"),
            Op::ExceptionOn {
                until: now + Duration::days(30),
            },
        )
        .await;
    assert_eq!(refusal(web), Refusal::HostOnly);
    // On: allowed while open.
    env.apply(
        &host(),
        Op::ExceptionOn {
            until: now + Duration::days(30),
        },
    )
    .await
    .unwrap();
    env.apply(&host(), add_auditor()).await.unwrap();
    assert!(env.dir().await.get("raj").unwrap().dual_role());
    let twice = env
        .apply(
            &host(),
            Op::ExceptionOn {
                until: now + Duration::days(10),
            },
        )
        .await;
    assert_eq!(refusal(twice), Refusal::ExceptionOpen);
    // Off: refused while someone holds both ...
    let off = env
        .apply(
            &host(),
            Op::ExceptionOff {
                revoke_auditor: false,
            },
        )
        .await;
    assert_eq!(refusal(off), Refusal::DualRole);
    // ... and with --revoke-auditor, auditor goes, admin stays.
    env.apply(
        &host(),
        Op::ExceptionOff {
            revoke_auditor: true,
        },
    )
    .await
    .unwrap();
    let dir = env.dir().await;
    assert!(dir.exception.is_none());
    let raj = dir.get("raj").unwrap();
    assert!(raj.has(UserRole::Admin) && !raj.has(UserRole::Auditor));
    let star = env.store.role_changes(Some("*"), 10).await.unwrap();
    assert_eq!(star[0].change, ChangeKind::ExceptionOff);
    assert_eq!(star[1].change, ChangeKind::ExceptionOn);
    assert!(star[1]
        .detail
        .as_deref()
        .unwrap()
        .starts_with("admin_and_auditor until "));
    // Off again: nothing to switch off.
    let none = env
        .apply(
            &host(),
            Op::ExceptionOff {
                revoke_auditor: false,
            },
        )
        .await;
    assert_eq!(refusal(none), Refusal::NoException);
}

/// raj and ana are admins; raj also holds auditor under an exception that has lapsed.
async fn lapsed_setup(env: &Env) -> DateTime<Utc> {
    env.with_team(&["ana", "ben"]).await;
    env.apply(
        &host(),
        Op::GrantAdmin {
            handle: "ana".into(),
            entra: AdminEntra::Oid(key(OID_ANA)),
        },
    )
    .await
    .unwrap();
    let on = Utc::now();
    env.apply_at(
        &host(),
        Op::ExceptionOn {
            until: on + Duration::days(30),
        },
        on,
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::SetRoles {
            handle: "raj".into(),
            roles: roles(&[UserRole::Admin, UserRole::Pm, UserRole::Auditor]),
        },
        on,
    )
    .await
    .unwrap();
    env.lapse_exception(on).await
}

/// Reviewer point 1: a lapsed exception blocks other changes to the person holding both,
/// but never the changes that resolve it.
async fn lapsed_exception_resolves_and_keeps_an_admin(env: &Env) {
    let later = lapsed_setup(env).await;
    // A change that touches raj and doesn't resolve it: refused.
    let touch = env
        .apply_at(
            &host(),
            Op::SetField {
                handle: "raj".into(),
                field: Field::SignInName,
                value: "raj.k@contoso.com".into(),
            },
            later,
        )
        .await;
    assert_eq!(refusal(touch), Refusal::DualRole);
    // A change to someone else: allowed.
    env.apply_at(
        &host(),
        Op::SetField {
            handle: "ben".into(),
            field: Field::SignInName,
            value: "ben.b@contoso.com".into(),
        },
        later,
    )
    .await
    .unwrap();
    // Off without --revoke-auditor: refused.
    let off = env
        .apply_at(
            &host(),
            Op::ExceptionOff {
                revoke_auditor: false,
            },
            later,
        )
        .await;
    assert_eq!(refusal(off), Refusal::DualRole);
    // Each resolving change works on its own (each on a copy of the state would need a fresh
    // store; here they run in an order where each is still a resolution).
    // 1. Revoking auditor (web, by the other admin).
    env.apply_at(
        &Actor::web("ana"),
        Op::SetRoles {
            handle: "raj".into(),
            roles: roles(&[UserRole::Admin, UserRole::Pm]),
        },
        later,
    )
    .await
    .unwrap();
    assert!(!env.dir().await.get("raj").unwrap().dual_role());
    // Put raj back under a fresh exception, let it lapse again, then:
    let on = Utc::now();
    env.apply_at(
        &host(),
        Op::ExceptionOff {
            revoke_auditor: false,
        },
        later,
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::ExceptionOn {
            until: on + Duration::days(30),
        },
        on,
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::SetRoles {
            handle: "raj".into(),
            roles: roles(&[UserRole::Admin, UserRole::Pm, UserRole::Auditor]),
        },
        on,
    )
    .await
    .unwrap();
    let later = env.lapse_exception(on).await;
    // 2. Revoking admin (host): raj keeps auditor only; ana is still an admin.
    env.apply_at(
        &host(),
        Op::RevokeAdmin {
            handle: "raj".into(),
        },
        later,
    )
    .await
    .unwrap();
    let dir = env.dir().await;
    assert!(!dir.get("raj").unwrap().dual_role());
    assert!(dir.has_active_admin());
    // 3. off --revoke-auditor works while lapsed, with nobody holding both now too.
    env.apply_at(
        &host(),
        Op::ExceptionOff {
            revoke_auditor: true,
        },
        later,
    )
    .await
    .unwrap();
    assert!(env.dir().await.exception.is_none());
}

/// Reviewer point 1, the other half: when the only admin is the one holding both, a lapsed
/// exception can't leave the stack with no working admin: deactivating them or revoking
/// admin is refused (last admin), and revoking auditor or `off --revoke-auditor` resolve it
/// with admin kept.
async fn lapsed_exception_only_admin_keeps_admin(env: &Env) {
    env.with_team(&["ben"]).await;
    let on = Utc::now();
    env.apply_at(
        &host(),
        Op::ExceptionOn {
            until: on + Duration::days(30),
        },
        on,
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::SetRoles {
            handle: "raj".into(),
            roles: roles(&[UserRole::Admin, UserRole::Pm, UserRole::Auditor]),
        },
        on,
    )
    .await
    .unwrap();
    let later = env.lapse_exception(on).await;
    let revoke = env
        .apply_at(
            &host(),
            Op::RevokeAdmin {
                handle: "raj".into(),
            },
            later,
        )
        .await;
    assert_eq!(refusal(revoke), Refusal::LastAdmin);
    let deactivate = env
        .apply_at(
            &host(),
            Op::Deactivate {
                handle: "raj".into(),
            },
            later,
        )
        .await;
    assert_eq!(refusal(deactivate), Refusal::LastAdmin);
    // Deactivating someone else still works: the lapse blocks only changes to raj.
    env.apply_at(
        &host(),
        Op::Deactivate {
            handle: "ben".into(),
        },
        later,
    )
    .await
    .unwrap();
    let dir = env.dir().await;
    assert!(
        dir.get("raj").unwrap().has(UserRole::Admin),
        "admin powers stay"
    );
    // off --revoke-auditor resolves it.
    env.apply_at(
        &host(),
        Op::ExceptionOff {
            revoke_auditor: true,
        },
        later,
    )
    .await
    .unwrap();
    let raj = env.dir().await.get("raj").cloned().unwrap();
    assert!(raj.has(UserRole::Admin) && !raj.has(UserRole::Auditor));
}

/// Reviewer point 4: a refused fourth deactivation doesn't count toward the hour.
async fn deactivation_cap_refused_attempt_does_not_count(env: &Env) {
    env.with_team(&["p1", "p2", "p3", "p4", "p5"]).await;
    let t0 = Utc::now() - Duration::hours(3);
    let web = Actor::web("raj");
    let at = |m: i64| t0 + Duration::minutes(m);
    for (h, m) in [("p1", 0), ("p2", 10), ("p3", 20)] {
        env.apply_at(&web, Op::Deactivate { handle: h.into() }, at(m))
            .await
            .unwrap();
    }
    assert_eq!(env.store.deactivations_in_window(at(30)).await.unwrap(), 3);
    let fourth = env
        .apply_at(
            &web,
            Op::Deactivate {
                handle: "p4".into(),
            },
            at(30),
        )
        .await;
    assert_eq!(refusal(fourth), Refusal::DeactivationLimit);
    // The Slack client shares the cap.
    let bot = env
        .apply_at(
            &Actor::bot("raj"),
            Op::Deactivate {
                handle: "p4".into(),
            },
            at(31),
        )
        .await;
    assert_eq!(refusal(bot), Refusal::DeactivationLimit);
    assert_eq!(env.store.deactivations_in_window(at(31)).await.unwrap(), 3);
    // The host isn't capped, and doesn't count.
    env.apply_at(
        &host(),
        Op::Deactivate {
            handle: "p5".into(),
        },
        at(32),
    )
    .await
    .unwrap();
    assert_eq!(env.store.deactivations_in_window(at(32)).await.unwrap(), 3);
    // At +61 min p1 is out of the window: two left in it (p2, p3). Had the refused attempts
    // at +30 and +31 counted, this would still be refused.
    env.apply_at(
        &web,
        Op::Deactivate {
            handle: "p4".into(),
        },
        at(61),
    )
    .await
    .unwrap();
    let denied = env
        .events()
        .await
        .into_iter()
        .filter(|e| e.tool == "users.deactivate" && e.outcome == Outcome::Denied)
        .count();
    assert_eq!(denied, 2, "each refusal is audited");
}

/// Reviewer point 4: a reactivation doesn't reset the window.
async fn deactivation_cap_reactivation_does_not_reset(env: &Env) {
    env.with_team(&["p1", "p2", "p3", "p4"]).await;
    let t0 = Utc::now() - Duration::hours(3);
    let web = Actor::web("raj");
    let at = |m: i64| t0 + Duration::minutes(m);
    for (h, m) in [("p1", 0), ("p2", 5), ("p3", 10)] {
        env.apply_at(&web, Op::Deactivate { handle: h.into() }, at(m))
            .await
            .unwrap();
    }
    env.apply_at(
        &web,
        Op::Reactivate {
            handle: "p1".into(),
        },
        at(15),
    )
    .await
    .unwrap();
    assert_eq!(env.store.deactivations_in_window(at(20)).await.unwrap(), 3);
    let fourth = env
        .apply_at(
            &web,
            Op::Deactivate {
                handle: "p4".into(),
            },
            at(20),
        )
        .await;
    assert_eq!(refusal(fourth), Refusal::DeactivationLimit);
    // Deactivating p1 again also counts against the same window.
    let again = env
        .apply_at(
            &web,
            Op::Deactivate {
                handle: "p1".into(),
            },
            at(21),
        )
        .await;
    assert_eq!(refusal(again), Refusal::DeactivationLimit);
}

/// The client rules the store already holds for later slices.
async fn client_rules(env: &Env) {
    env.with_team(&["ana", "ben"]).await;
    let bot = Actor::bot("raj");
    // Slack: pm-only joins, unconfirmed sign-in name; never admin or other roles.
    let admin = env
        .apply(
            &bot,
            Op::CreateUser {
                user: person("cy", &[UserRole::Pm, UserRole::Admin]),
            },
        )
        .await;
    assert_eq!(refusal(admin), Refusal::AdminNotFromSlack);
    let reviewer = env
        .apply(
            &bot,
            Op::CreateUser {
                user: person("cy", &[UserRole::Pm, UserRole::Reviewer]),
            },
        )
        .await;
    assert_eq!(refusal(reviewer), Refusal::RoleNotFromSlack);
    env.apply(
        &bot,
        Op::CreateUser {
            user: person("cy", &[UserRole::Pm]),
        },
    )
    .await
    .unwrap();
    let cy = env.dir().await.get("cy").cloned().unwrap();
    assert!(cy.sign_in_name.is_some() && cy.sign_in_name_confirmed_at.is_none());
    assert_eq!(cy.created_via, Via::Bot);
    // Slack never deactivates an Admin or Auditor, never touches addresses.
    let protected = env
        .apply(
            &Actor::bot("ana"),
            Op::Deactivate {
                handle: "raj".into(),
            },
        )
        .await;
    assert_eq!(refusal(protected), Refusal::Forbidden, "ana isn't an admin");
    env.apply(
        &host(),
        Op::SetRoles {
            handle: "ben".into(),
            roles: roles(&[UserRole::Pm, UserRole::Auditor]),
        },
    )
    .await
    .unwrap();
    let auditor = env
        .apply(
            &bot,
            Op::Deactivate {
                handle: "ben".into(),
            },
        )
        .await;
    assert_eq!(refusal(auditor), Refusal::ProtectedNotFromSlack);
    let field = env
        .apply(
            &bot,
            Op::SetField {
                handle: "ana".into(),
                field: Field::SignInName,
                value: "x@contoso.com".into(),
            },
        )
        .await;
    assert_eq!(refusal(field), Refusal::NotFromSlack);
    // Web: never grants admin; an Auditor's addresses are host only.
    let web = Actor::web("raj");
    let grant = env
        .apply(
            &web,
            Op::SetRoles {
                handle: "ana".into(),
                roles: roles(&[UserRole::Pm, UserRole::Admin]),
            },
        )
        .await;
    assert_eq!(refusal(grant), Refusal::HostOnly);
    let grant = env
        .apply(
            &web,
            Op::GrantAdmin {
                handle: "ana".into(),
                entra: AdminEntra::Oid(key(OID_ANA)),
            },
        )
        .await;
    assert_eq!(refusal(grant), Refusal::HostOnly);
    let protected = env
        .apply(
            &web,
            Op::SetField {
                handle: "ben".into(),
                field: Field::AtlassianEmail,
                value: "b@contoso.com".into(),
            },
        )
        .await;
    assert_eq!(refusal(protected), Refusal::HostOnly);
    // A web actor who isn't an active admin can't write.
    let pm = env
        .apply(
            &Actor::web("ana"),
            Op::Deactivate {
                handle: "cy".into(),
            },
        )
        .await;
    assert_eq!(refusal(pm), Refusal::Forbidden);
    // The import is the import's own path.
    let req = ImportRequest::parse(b"members: []\n", None, ImportMode::Members).unwrap();
    let import = env.apply(&host(), Op::Import(Box::new(req))).await;
    assert_eq!(refusal(import), Refusal::HostOnly);
}

async fn evals_account_rule(env: &Env) {
    env.with_team(&[]).await;
    // With a Slack id: refused (no keys, ever, in Postgres or a default memory store).
    let keyed = env
        .apply(
            &host(),
            Op::CreateUser {
                user: person("evals", &[UserRole::Pm]),
            },
        )
        .await;
    assert_eq!(refusal(keyed), Refusal::EvalsAccount);
    let mut evals = person("evals", &[UserRole::Reviewer]);
    evals.slack_id = None;
    let reviewer = env
        .apply(
            &host(),
            Op::CreateUser {
                user: evals.clone(),
            },
        )
        .await;
    assert_eq!(refusal(reviewer), Refusal::EvalsAccount);
    evals.roles = roles(&[UserRole::Pm]);
    env.apply(&host(), Op::CreateUser { user: evals })
        .await
        .unwrap();
    let more = env
        .apply(
            &host(),
            Op::SetRoles {
                handle: "evals".into(),
                roles: roles(&[UserRole::Pm, UserRole::Reviewer]),
            },
        )
        .await;
    assert_eq!(refusal(more), Refusal::EvalsAccount);
    let entra = env
        .apply(
            &host(),
            Op::SetEntra {
                handle: "evals".into(),
                key: key(OID_OTHER),
            },
        )
        .await;
    assert_eq!(refusal(entra), Refusal::EvalsAccount);
    let dir = env.dir().await;
    let evals = dir.get("evals").unwrap();
    assert!(evals.identities.is_empty() && evals.role_set() == roles(&[UserRole::Pm]));
}

async fn unique_keys_and_addresses_never_reused(env: &Env) {
    env.with_team(&["ana"]).await;
    let mut ben = person("ben", &[UserRole::Pm]);
    for (field, value) in [
        ("sign_in", "ana@contoso.com"),
        ("atlassian", "ANA@contoso.com"),
        ("github", "ANA-GH"),
        ("slack", "U0ANA"),
    ] {
        let mut b = ben.clone();
        match field {
            "sign_in" => b.sign_in_name = Some(value.into()),
            "atlassian" => b.atlassian_email = Some(value.into()),
            "github" => b.github_login = Some(value.into()),
            _ => b.slack_id = Some(value.into()),
        }
        let r = env.apply(&host(), Op::CreateUser { user: b }).await;
        assert_eq!(refusal(r), Refusal::Taken, "{field}");
    }
    // A deactivated person's addresses stay theirs.
    env.apply(
        &host(),
        Op::Deactivate {
            handle: "ana".into(),
        },
    )
    .await
    .unwrap();
    ben.sign_in_name = Some("ana@contoso.com".into());
    let r = env
        .apply(&host(), Op::CreateUser { user: ben.clone() })
        .await;
    assert_eq!(refusal(r), Refusal::Taken);
    // Handles are never reused.
    let r = env
        .apply(
            &host(),
            Op::CreateUser {
                user: person("ana", &[UserRole::Pm]),
            },
        )
        .await;
    assert_eq!(refusal(r), Refusal::Exists);
    // Nor is a handle spelled as another's (the Low on colliding handles, review at 6bb806f):
    // `ana.`, `ana_` and `ana-` are all `ana` in a bot's name and to the model gateway, which
    // keeps each person's calls under that spelling.
    for h in ["ana.", "ana_", "ana-", "ana.."] {
        let r = env
            .apply(
                &host(),
                Op::CreateUser {
                    user: person(h, &[UserRole::Pm]),
                },
            )
            .await;
        let Err(e) = r else {
            panic!("{h} was created beside ana")
        };
        assert!(e.to_string().contains("as `ana` already is"), "{h}: {e}");
    }
    assert!(env.dir().await.get("ana.").is_none());
    // Sign-in name and Atlassian email are separate columns.
    env.apply(
        &host(),
        Op::SetField {
            handle: "raj".into(),
            field: Field::SignInName,
            value: "Raj.Kumar@Contoso.com".into(),
        },
    )
    .await
    .unwrap();
    let raj = env.dir().await.get("raj").cloned().unwrap();
    assert_eq!(raj.sign_in_name.as_deref(), Some("raj.kumar@contoso.com"));
    assert_eq!(raj.atlassian_email.as_deref(), Some("raj@contoso.com"));
    // L8: the sign-in name raj had is still his, never anyone else's: not by a change, not
    // for a new user. Raj may have it back.
    let r = env
        .apply(
            &host(),
            Op::SetField {
                handle: "ben-not-yet".into(),
                field: Field::SignInName,
                value: "raj@contoso.com".into(),
            },
        )
        .await;
    assert!(r.is_err());
    let mut carl = person("carl", &[UserRole::Pm]);
    carl.sign_in_name = Some("Raj@Contoso.com".into());
    let r = env.apply(&host(), Op::CreateUser { user: carl }).await;
    assert_eq!(refusal(r), Refusal::Taken, "a retired sign-in name");
    let r = env
        .apply(
            &host(),
            Op::SetField {
                handle: "ana".into(),
                field: Field::SignInName,
                value: "raj@contoso.com".into(),
            },
        )
        .await;
    assert_eq!(
        refusal(r),
        Refusal::Taken,
        "a retired sign-in name, by a change"
    );
    // A retired GitHub login likewise, in any case.
    env.apply(
        &host(),
        Op::SetField {
            handle: "raj".into(),
            field: Field::GithubLogin,
            value: "raj-new".into(),
        },
    )
    .await
    .unwrap();
    let mut dan = person("dan", &[UserRole::Pm]);
    dan.github_login = Some("RAJ-GH".into());
    let r = env.apply(&host(), Op::CreateUser { user: dan }).await;
    assert_eq!(refusal(r), Refusal::Taken, "a retired GitHub login");
    env.apply(
        &host(),
        Op::SetField {
            handle: "raj".into(),
            field: Field::SignInName,
            value: "raj@contoso.com".into(),
        },
    )
    .await
    .unwrap();
    env.apply(
        &host(),
        Op::SetField {
            handle: "raj".into(),
            field: Field::SignInName,
            value: "raj.kumar@contoso.com".into(),
        },
    )
    .await
    .unwrap();
    // One Entra key per person; one person per key.
    let r = env
        .apply(
            &host(),
            Op::SetEntra {
                handle: "ana".into(),
                key: key(OID_RAJ),
            },
        )
        .await;
    assert_eq!(refusal(r), Refusal::Taken);
    for bad in ["bots.docs", "host", "Ana", "-x"] {
        let r = env
            .apply(
                &host(),
                Op::CreateUser {
                    user: NewUser {
                        handle: bad.into(),
                        ..Default::default()
                    },
                },
            )
            .await;
        assert_eq!(refusal(r), Refusal::Invalid, "{bad}");
    }
}

async fn grant_admin_entra_rules(env: &Env) {
    env.with_team(&["ana", "ben"]).await;
    let keep = env
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ana".into(),
                entra: AdminEntra::Keep,
            },
        )
        .await;
    assert_eq!(refusal(keep), Refusal::EntraNeedsHost);
    env.apply(
        &host(),
        Op::SetEntra {
            handle: "ana".into(),
            key: key(OID_ANA),
        },
    )
    .await
    .unwrap();
    let other = env
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ana".into(),
                entra: AdminEntra::Oid(key(OID_OTHER)),
            },
        )
        .await;
    assert_eq!(refusal(other), Refusal::EntraMismatch);
    env.apply(
        &host(),
        Op::GrantAdmin {
            handle: "ana".into(),
            entra: AdminEntra::Keep,
        },
    )
    .await
    .unwrap();
    env.apply(
        &host(),
        Op::Deactivate {
            handle: "ben".into(),
        },
    )
    .await
    .unwrap();
    let inactive = env
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ben".into(),
                entra: AdminEntra::Oid(key(OID_OTHER)),
            },
        )
        .await;
    assert_eq!(refusal(inactive), Refusal::NotActive);
    env.apply(
        &host(),
        Op::UnsetEntra {
            handle: "ana".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(env.dir().await.get("ana").unwrap().entra_state(), "none");
}

const TEAM: &str = r#"
# The team.
members:
  - handle: raj
    slack-id: U0RAJ
    email: Raj@contoso.com
    github: raj-gh
    roles: [pm, admin]
  - handle: ana
    slack-id: U0ANA
    email: ana@contoso.com
    roles: [pm, reviewer]
  - handle: ben
    slack-id: U0BEN
    email: ben@contoso.com
    entra-oid: aaaaaaaa-0000-0000-0000-000000000003
    roles: [skill-author]
  - handle: evals
    slack-id: U0EVALS
    roles: [pm]
"#;

fn import(text: &str, mode: ImportMode) -> Op {
    Op::Import(Box::new(
        ImportRequest::parse(text.as_bytes(), Some("1a2b3c4d"), mode).unwrap(),
    ))
}

fn importer() -> Actor {
    Actor::import(Some(HostUser::parse("opsuser").unwrap()))
}

async fn import_bootstrap_idempotent_and_conflicts(env: &Env) {
    // The dry run changes nothing.
    let preview = env
        .store
        .preview(
            &importer(),
            &import(TEAM, ImportMode::Bootstrap),
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(
        preview.lines.iter().any(|l| l.contains("4 to create")),
        "{preview:?}"
    );
    assert!(env.dir().await.users.is_empty());

    env.apply(&importer(), import(TEAM, ImportMode::Bootstrap))
        .await
        .unwrap();
    let dir = env.dir().await;
    assert_eq!(dir.users.len(), 4);
    let raj = dir.get("raj").unwrap();
    assert_eq!(raj.role_set(), roles(&[UserRole::Admin, UserRole::Pm]));
    assert_eq!(raj.sign_in_name.as_deref(), Some("raj@contoso.com"));
    assert_eq!(raj.atlassian_email.as_deref(), Some("raj@contoso.com"));
    assert!(
        raj.sign_in_name_confirmed_at.is_some(),
        "imported names are confirmed"
    );
    assert!(
        raj.identity(IdentityKind::Entra).is_none(),
        "admins get no Entra link from a file"
    );
    assert_eq!(
        dir.get("ben").unwrap().role_set(),
        roles(&[UserRole::Pm]),
        "skill-author dropped, keeps pm"
    );
    assert!(dir
        .get("ben")
        .unwrap()
        .identity(IdentityKind::Entra)
        .is_none());
    assert!(dir.get("evals").unwrap().identities.is_empty());
    let star = env.store.role_changes(Some("*"), 5).await.unwrap();
    let d = star[0].detail.as_deref().unwrap();
    assert!(d.starts_with("bootstrap sha256 ") && d.len() == "bootstrap sha256 ".len() + 64);
    assert_eq!(star[0].change, ChangeKind::Import);
    assert_eq!(star[0].host_user.as_deref(), Some("opsuser"));

    // --bootstrap again: the table isn't empty.
    let again = env
        .apply(&importer(), import(TEAM, ImportMode::Bootstrap))
        .await;
    assert_eq!(refusal(again), Refusal::NotEmpty);

    // Without --bootstrap, the same file: idempotent, everything unchanged, recorded again.
    let version = env.dir().await.version;
    for _ in 0..2 {
        let preview = env
            .store
            .preview(&importer(), &import(TEAM, ImportMode::Members), Utc::now())
            .await
            .unwrap();
        assert!(
            preview
                .lines
                .iter()
                .any(|l| l.contains("0 to create, 4 unchanged, 0 conflicts")),
            "{preview:?}"
        );
        env.apply(&importer(), import(TEAM, ImportMode::Members))
            .await
            .unwrap();
    }
    assert_eq!(
        env.dir().await.version,
        version,
        "nothing in the directory moved"
    );
    let star = env.store.role_changes(Some("*"), 5).await.unwrap();
    assert!(star[0]
        .detail
        .as_deref()
        .unwrap()
        .ends_with(" commit 1a2b3c4d"));

    // A conflict is reported and never overwritten; a role the file drops is never revoked.
    let changed = TEAM
        .replace("ana@contoso.com", "ana.new@contoso.com")
        .replace("roles: [pm, reviewer]", "roles: [pm]");
    let preview = env
        .store
        .preview(
            &importer(),
            &import(&changed, ImportMode::Members),
            Utc::now(),
        )
        .await
        .unwrap();
    let line = preview.lines.iter().find(|l| l.contains("`ana`")).unwrap();
    assert!(
        line.contains("conflict") && line.contains("sign_in_name") && line.contains("roles"),
        "{line}"
    );
    env.apply(&importer(), import(&changed, ImportMode::Members))
        .await
        .unwrap();
    let ana = env.dir().await.get("ana").cloned().unwrap();
    assert_eq!(ana.sign_in_name.as_deref(), Some("ana@contoso.com"));
    assert!(ana.has(UserRole::Reviewer));

    // A new handle spelled as an existing one (`ana_`), or as one created earlier in the same
    // file (`zed_q` after `zed.q`), is a conflict and not created; the rest are (the Low on
    // colliding handles, review at 6bb806f).
    let clashing = format!(
        "{TEAM}  - handle: ana_\n    slack-id: U0ANAU\n    roles: [pm]\n  \
         - handle: zed.q\n    slack-id: U0ZEDQ\n    roles: [pm]\n  \
         - handle: zed_q\n    slack-id: U0ZEDQU\n    roles: [pm]\n"
    );
    let preview = env
        .store
        .preview(
            &importer(),
            &import(&clashing, ImportMode::Members),
            Utc::now(),
        )
        .await
        .unwrap();
    for (h, other) in [("ana_", "ana"), ("zed_q", "zed.q")] {
        let line = preview
            .lines
            .iter()
            .find(|l| l.contains(&format!("`{h}`")))
            .unwrap();
        assert!(
            line.contains("conflict") && line.contains(&format!("as `{other}` is")),
            "{line}"
        );
    }
    assert!(
        preview
            .lines
            .iter()
            .any(|l| l.contains("create ") && l.contains("`zed.q`")),
        "{preview:?}"
    );
    let import_events = env
        .events()
        .await
        .into_iter()
        .filter(|e| e.tool == "users.import" && e.outcome == Outcome::Ok)
        .count();
    assert_eq!(import_events, 4);
}

async fn import_refusals_write_nothing(env: &Env) {
    // No admin in the file and none in the database: refused with what to do.
    let members_only = "members:\n  - handle: ana\n    slack-id: U0ANA\n    roles: [pm]\n";
    let err = env
        .apply(&importer(), import(members_only, ImportMode::Members))
        .await
        .unwrap_err();
    assert_eq!(err.refusal(), Some(Refusal::LastAdmin));
    assert!(err.to_string().contains("bootstrap-admin"), "{err}");
    for (file, why) in [
        (
            TEAM.replace("roles: [pm, reviewer]", "roles: [pm, owner]"),
            "unknown role",
        ),
        (TEAM.replace("U0ANA", "U0RAJ"), "listed for two people"),
        (
            TEAM.replace("handle: ana", "handle: bots.ana"),
            "usable handle",
        ),
        (TEAM.replace("handle: ana", "handle: raj"), "listed twice"),
        (
            TEAM.replace("roles: [pm, admin]", "roles: [pm, admin, auditor]"),
            "admin and auditor",
        ),
        (
            TEAM.replace(
                "U0EVALS\n    roles: [pm]",
                "U0EVALS\n    roles: [pm, reviewer]",
            ),
            "evals",
        ),
    ] {
        let err = env
            .apply(&importer(), import(&file, ImportMode::Bootstrap))
            .await
            .unwrap_err();
        assert_eq!(err.refusal(), Some(Refusal::ImportRefused), "{why}: {err}");
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    let dir = env.dir().await;
    assert!(dir.users.is_empty());
    assert!(env.store.role_changes(None, 10).await.unwrap().is_empty());
    let denied = env
        .events()
        .await
        .into_iter()
        .filter(|e| e.tool == "users.import" && e.outcome == Outcome::Denied)
        .count();
    assert_eq!(denied, 7);
}

async fn import_without_allow_admins(env: &Env) {
    // The documented order: bootstrap-admin, then import without --allow-admins.
    env.with_team(&[]).await;
    let file = TEAM.replace("roles: [pm, reviewer]", "roles: [pm, admin]");
    let preview = env
        .store
        .preview(&importer(), &import(&file, ImportMode::Members), Utc::now())
        .await
        .unwrap();
    assert!(preview
        .lines
        .iter()
        .any(|l| l.contains("admin not imported")));
    env.apply(&importer(), import(&file, ImportMode::Members))
        .await
        .unwrap();
    let dir = env.dir().await;
    assert_eq!(dir.get("ana").unwrap().role_set(), roles(&[UserRole::Pm]));
    // raj (bootstrapped as admin, an admin in the file too) is unchanged, not a conflict.
    let raj_line = preview.lines.iter().find(|l| l.contains("`raj`")).unwrap();
    assert!(raj_line.contains("unchanged"), "{raj_line}");
    // With --allow-admins, a later import grants nothing either: the database wins.
    env.apply(&importer(), import(&file, ImportMode::AllowAdmins))
        .await
        .unwrap();
    assert!(!env.dir().await.get("ana").unwrap().has(UserRole::Admin));
}

async fn role_changes_retention(env: &Env) {
    let now = Utc::now();
    let then = now - Duration::days(100);
    env.apply_at(
        &host(),
        Op::BootstrapAdmin {
            user: person("raj", &[]),
            entra: None,
        },
        then,
    )
    .await
    .unwrap();
    for h in ["ana", "ben", "cy"] {
        env.apply_at(
            &host(),
            Op::CreateUser {
                user: person(h, &[UserRole::Pm]),
            },
            then,
        )
        .await
        .unwrap();
    }
    env.apply_at(
        &host(),
        Op::ExceptionOn {
            until: then + Duration::days(10),
        },
        then,
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::Deactivate {
            handle: "ana".into(),
        },
        now - Duration::days(91),
    )
    .await
    .unwrap();
    env.apply_at(
        &host(),
        Op::Deactivate {
            handle: "ben".into(),
        },
        now - Duration::days(89),
    )
    .await
    .unwrap();
    let count = |changes: &[pmos_toolserver::users::RoleChange], t: &str| {
        changes.iter().filter(|c| c.target == t).count()
    };
    let before = env.store.role_changes(None, 1000).await.unwrap();
    assert!(count(&before, "ana") > 0 && count(&before, "*") == 1);
    let deleted = env.store.purge_role_changes(now).await.unwrap();
    let after = env.store.role_changes(None, 1000).await.unwrap();
    assert_eq!(deleted as usize, count(&before, "ana"));
    assert_eq!(count(&after, "ana"), 0, "deactivated 91 days ago: gone");
    for t in ["raj", "ben", "cy", "*"] {
        assert_eq!(count(&after, t), count(&before, t), "{t} kept");
    }
    // The user row itself stays (no erasure in the MVP).
    assert_eq!(
        env.dir().await.get("ana").unwrap().status,
        Status::Deactivated
    );
    // The job records itself.
    assert!(env
        .events()
        .await
        .iter()
        .any(|e| e.tool == "users.role_changes_purge" && e.client == Client::System));
}

// --- The rules on their own --------------------------------------------------------------

/// Reviewer point 5: `import --bootstrap` refuses any non-empty table, even one whose only
/// rows are deactivated users. (The database's last-admin trigger keeps that state from
/// ever being committed; the bootstrap's own check doesn't rely on it: `trigger_bootstrap_*`
/// below proves the same in Postgres.)
#[test]
fn bootstrap_refuses_a_table_of_deactivated_users() {
    let mut dir = Directory::default();
    let now = Utc::now();
    let gone = pmos_toolserver::users::User {
        handle: "old".into(),
        display_name: String::new(),
        sign_in_name: None,
        sign_in_name_confirmed_at: None,
        sign_in_name_confirmed_by: None,
        atlassian_email: None,
        github_login: None,
        status: Status::Deactivated,
        created_at: now,
        created_by: "host".into(),
        created_via: Via::Host,
        deactivated_at: Some(now),
        deactivated_by: Some("host".into()),
        roles: Default::default(),
        identities: Default::default(),
    };
    dir.users.insert("old".into(), gone);
    let r = rules::plan(
        &dir,
        &importer(),
        &import(TEAM, ImportMode::Bootstrap),
        now,
        &Rules::default(),
    );
    assert_eq!(refusal(r), Refusal::NotEmpty);
}

#[tokio::test]
async fn the_seed_loads_the_eval_stack_with_its_fake_slack_id() {
    let seed = "members:\n  - handle: ops\n    slack-id: U0OPS\n    roles: [pm, admin]\n  - handle: evals\n    slack-id: U0EVALS\n    roles: [pm]\n";
    let req = ImportRequest::parse(seed.as_bytes(), None, ImportMode::Bootstrap).unwrap();
    let fake = Rules {
        evals_slack_allowed: true,
        ..Default::default()
    };
    let store = MemoryUsers::seeded(req.clone(), fake, None).await.unwrap();
    let dir = store.directory(Utc::now()).await.unwrap();
    let evals = dir.get("evals").unwrap();
    assert_eq!(
        evals.identity(IdentityKind::Slack).unwrap().subject,
        "U0EVALS"
    );
    // Without fake connectors, the evals account gets no key.
    let store = MemoryUsers::seeded(req, Rules::default(), None)
        .await
        .unwrap();
    assert!(store
        .directory(Utc::now())
        .await
        .unwrap()
        .get("evals")
        .unwrap()
        .identities
        .is_empty());
    // A seed is checked like any import: no admin, or an unknown role, is refused.
    let no_admin = "members:\n  - handle: evals\n    slack-id: U0EVALS\n    roles: [pm]\n";
    let req = ImportRequest::parse(no_admin.as_bytes(), None, ImportMode::Bootstrap).unwrap();
    let err = MemoryUsers::seeded(req, fake, None).await.err().unwrap();
    assert!(err.contains("no active admin"), "{err}");
    let unknown = "members:\n  - handle: ops\n    slack-id: U0OPS\n    roles: [admin, owner]\n";
    let req = ImportRequest::parse(unknown.as_bytes(), None, ImportMode::Bootstrap).unwrap();
    assert!(MemoryUsers::seeded(req, fake, None).await.is_err());
}

// --- The triggers, in Postgres, with raw SQL ---------------------------------------------

async fn pg() -> Option<TestPg> {
    TestPg::new().await
}

/// raj and ana, both active admins, written through the store.
async fn two_admins(pg: &TestPg) {
    let store: &dyn UserStore = &pg.store;
    store
        .apply(
            &host(),
            Op::BootstrapAdmin {
                user: person("raj", &[]),
                entra: Some(key(OID_RAJ)),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    store
        .apply(
            &host(),
            Op::CreateUser {
                user: person("ana", &[UserRole::Pm]),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    store
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ana".into(),
                entra: AdminEntra::Oid(key(OID_ANA)),
            },
            Utc::now(),
        )
        .await
        .unwrap();
}

/// One raw statement in its own transaction; the error's message on failure.
async fn raw(pg: &TestPg, sql: &str) -> Result<(), String> {
    let mut tx = pg.store.pool().begin().await.unwrap();
    if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&mut *tx)
        .await
    {
        return Err(e.to_string());
    }
    tx.commit().await.map_err(|e| e.to_string())
}

async fn admins(pg: &TestPg) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT u.handle FROM users u JOIN user_roles r USING (handle)
         WHERE r.role = 'admin' AND u.status = 'active' ORDER BY 1",
    )
    .fetch_all(pg.store.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn trigger_concurrent_last_admin_demotions_without_any_application_lock() {
    for round in 0..5 {
        let Some(pg) = pg().await else { return };
        two_admins(&pg).await;
        // Two transactions, raw SQL, no advisory lock taken by anyone but the trigger. Each
        // demotes a different admin; both statements run before either commits.
        let mut a = pg.store.pool().begin().await.unwrap();
        let mut b = pg.store.pool().begin().await.unwrap();
        sqlx::query("DELETE FROM user_roles WHERE handle = 'raj' AND role = 'admin'")
            .execute(&mut *a)
            .await
            .unwrap();
        sqlx::query("DELETE FROM user_roles WHERE handle = 'ana' AND role = 'admin'")
            .execute(&mut *b)
            .await
            .unwrap();
        let (ra, rb) = tokio::join!(a.commit(), b.commit());
        let errors: Vec<String> = [ra, rb]
            .into_iter()
            .filter_map(|r| r.err().map(|e| e.to_string()))
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "round {round}: exactly one commits: {errors:?}"
        );
        assert!(errors[0].contains("last_admin"), "{}", errors[0]);
        assert_eq!(admins(&pg).await.len(), 1, "round {round}");
        pg.drop_schema().await;
    }
}

#[tokio::test]
async fn trigger_concurrent_deactivation_and_demotion() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    let mut a = pg.store.pool().begin().await.unwrap();
    let mut b = pg.store.pool().begin().await.unwrap();
    sqlx::query(
        "UPDATE users SET status = 'deactivated', deactivated_at = now(), deactivated_by = 'x'
         WHERE handle = 'raj'",
    )
    .execute(&mut *a)
    .await
    .unwrap();
    sqlx::query("DELETE FROM user_roles WHERE handle = 'ana' AND role = 'admin'")
        .execute(&mut *b)
        .await
        .unwrap();
    let (ra, rb) = tokio::join!(a.commit(), b.commit());
    assert!(ra.is_ok() != rb.is_ok(), "{ra:?} {rb:?}");
    assert_eq!(admins(&pg).await.len(), 1);
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_refuses_any_isolation_but_read_committed() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    for level in ["REPEATABLE READ", "SERIALIZABLE"] {
        let mut tx = pg.store.pool().begin().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SET TRANSACTION ISOLATION LEVEL {level}"
        )))
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO users (handle, status, created_at, created_by, created_client)
             VALUES ('zed', 'active', now(), 'host', 'host')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        let err = tx.commit().await.unwrap_err().to_string();
        assert!(err.contains("users_isolation"), "{level}: {err}");
    }
    // The same write in READ COMMITTED commits.
    raw(
        &pg,
        "INSERT INTO users (handle, status, created_at, created_by, created_client)
         VALUES ('zed', 'active', now(), 'host', 'host')",
    )
    .await
    .unwrap();
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_last_admin_and_no_deletes() {
    let Some(pg) = pg().await else { return };
    // An empty table needs no admin; the first user without one is refused.
    let err = raw(
        &pg,
        "INSERT INTO users (handle, status, created_at, created_by, created_client)
         VALUES ('ana', 'active', now(), 'host', 'host')",
    )
    .await
    .unwrap_err();
    assert!(err.contains("last_admin"), "{err}");
    two_admins(&pg).await;
    raw(
        &pg,
        "DELETE FROM user_roles WHERE handle = 'raj' AND role = 'admin'",
    )
    .await
    .unwrap();
    for sql in [
        "DELETE FROM user_roles WHERE handle = 'ana' AND role = 'admin'",
        "UPDATE users SET status = 'deactivated', deactivated_at = now(), deactivated_by = 'x' WHERE handle = 'ana'",
    ] {
        let err = raw(&pg, sql).await.unwrap_err();
        assert!(err.contains("last_admin"), "{sql}: {err}");
    }
    let err = raw(&pg, "DELETE FROM users WHERE handle = 'raj'")
        .await
        .unwrap_err();
    assert!(err.contains("never deleted"), "{err}");
    for table in [
        "users",
        "user_roles",
        "user_identities",
        "pilot_exceptions",
        "role_changes",
    ] {
        let err = raw(&pg, &format!("TRUNCATE {table} CASCADE"))
            .await
            .unwrap_err();
        assert!(
            err.contains("truncated")
                || err.contains("append-only")
                || err.contains("never deleted"),
            "{table}: {err}"
        );
    }
    assert_eq!(admins(&pg).await, ["ana"]);
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_admin_and_auditor_and_the_pilot_exception() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    let grant = "INSERT INTO user_roles (handle, role, granted_at, granted_by)
                 VALUES ('raj', 'auditor', now(), 'host')";
    // No exception: refused.
    let err = raw(&pg, grant).await.unwrap_err();
    assert!(err.contains("dual_role"), "{err}");
    // Past its expires_at: refused.
    raw(
        &pg,
        "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
         VALUES ('admin_and_auditor', now() - interval '40 days', 'opsuser', now() - interval '1 day')",
    )
    .await
    .unwrap();
    let err = raw(&pg, grant).await.unwrap_err();
    assert!(err.contains("dual_role"), "{err}");
    raw(
        &pg,
        "UPDATE pilot_exceptions SET ended_at = now(), ended_by = 'opsuser' WHERE ended_at IS NULL",
    )
    .await
    .unwrap();
    // More than 90 days: the table refuses it.
    let err = raw(
        &pg,
        "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
         VALUES ('admin_and_auditor', now(), 'opsuser', now() + interval '91 days')",
    )
    .await
    .unwrap_err();
    assert!(err.contains("check"), "{err}");
    // Open: allowed.
    raw(
        &pg,
        "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
         VALUES ('admin_and_auditor', now(), 'opsuser', now() + interval '30 days')",
    )
    .await
    .unwrap();
    raw(&pg, grant).await.unwrap();
    // A second open exception: refused.
    let err = raw(
        &pg,
        "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
         VALUES ('admin_and_auditor', now(), 'opsuser', now() + interval '5 days')",
    )
    .await
    .unwrap_err();
    assert!(err.contains("one_open_exception"), "{err}");
    // Ending it while raj holds both: refused; with auditor revoked in the same
    // transaction: allowed.
    let end =
        "UPDATE pilot_exceptions SET ended_at = now(), ended_by = 'opsuser' WHERE ended_at IS NULL";
    let err = raw(&pg, end).await.unwrap_err();
    assert!(err.contains("dual_role"), "{err}");
    let mut tx = pg.store.pool().begin().await.unwrap();
    sqlx::query("DELETE FROM user_roles WHERE handle = 'raj' AND role = 'auditor'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(end).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_lapsed_exception_allows_only_the_resolving_changes() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    raw(
        &pg,
        "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
         VALUES ('admin_and_auditor', now(), 'opsuser', now() + interval '30 days')",
    )
    .await
    .unwrap();
    raw(
        &pg,
        "INSERT INTO user_roles (handle, role, granted_at, granted_by)
         VALUES ('raj', 'auditor', now(), 'host')",
    )
    .await
    .unwrap();
    // A month goes by (see Env::lapse_exception).
    let mut tx = pg.store.pool().begin().await.unwrap();
    for sql in [
        "ALTER TABLE pilot_exceptions DISABLE TRIGGER users_invariants",
        "UPDATE pilot_exceptions SET enabled_at = now() - interval '31 days', expires_at = now() - interval '1 day'",
        "ALTER TABLE pilot_exceptions ENABLE TRIGGER users_invariants",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    // Touching raj otherwise: refused. Touching someone else: allowed.
    let err = raw(
        &pg,
        "UPDATE users SET display_name = 'Raj' WHERE handle = 'raj'",
    )
    .await
    .unwrap_err();
    assert!(err.contains("dual_role"), "{err}");
    raw(
        &pg,
        "UPDATE users SET display_name = 'Ana' WHERE handle = 'ana'",
    )
    .await
    .unwrap();
    // Each resolving change, rolled back after the check so the next starts from the same
    // state: revoke auditor; revoke admin; deactivate.
    for sql in [
        "DELETE FROM user_roles WHERE handle = 'raj' AND role = 'auditor'",
        "DELETE FROM user_roles WHERE handle = 'raj' AND role = 'admin'",
        "UPDATE users SET status = 'deactivated', deactivated_at = now(), deactivated_by = 'x' WHERE handle = 'raj'",
    ] {
        let mut tx = pg.store.pool().begin().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await.unwrap();
        // Run the deferred trigger now, without committing.
        sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        tx.rollback().await.unwrap();
    }
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_evals_account_and_unique_keys() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    raw(
        &pg,
        "INSERT INTO users (handle, status, created_at, created_by, created_client)
         VALUES ('evals', 'active', now(), 'host', 'host')",
    )
    .await
    .unwrap();
    for sql in [
        "INSERT INTO user_roles VALUES ('evals', 'reviewer', now(), 'host')",
        "INSERT INTO user_identities (handle, kind, subject, linked_at, linked_by, linked_client) VALUES ('evals', 'slack', 'U0EVALS', now(), 'host', 'host')",
        "INSERT INTO user_identities (handle, kind, issuer, subject, linked_at, linked_by, linked_client) VALUES ('evals', 'entra', 't', 'o', now(), 'host', 'host')",
    ] {
        let err = raw(&pg, sql).await.unwrap_err();
        assert!(err.contains("evals_account"), "{sql}: {err}");
    }
    raw(
        &pg,
        "INSERT INTO user_roles VALUES ('evals', 'pm', now(), 'host')",
    )
    .await
    .unwrap();
    for (sql, key) in [
        ("UPDATE users SET sign_in_name = 'raj@contoso.com' WHERE handle = 'ana'", "users_sign_in_name"),
        ("UPDATE users SET atlassian_email = 'raj@contoso.com' WHERE handle = 'ana'", "users_atlassian_email"),
        ("UPDATE users SET github_login = 'RAJ-GH' WHERE handle = 'ana'", "users_github_login"),
        (
            "INSERT INTO user_identities (handle, kind, issuer, subject, linked_at, linked_by, linked_client) VALUES ('ana', 'slack', '', 'U0RAJ', now(), 'host', 'host')",
            "user_identities_kind_issuer_subject_key",
        ),
        (
            "INSERT INTO user_identities (handle, kind, issuer, subject, linked_at, linked_by, linked_client) VALUES ('ana', 'entra', 'other-tenant', 'x', now(), 'host', 'host')",
            "user_identities_handle_kind_key",
        ),
        ("UPDATE users SET sign_in_name = 'Ana@contoso.com' WHERE handle = 'ana'", "check"),
        ("INSERT INTO users (handle, status, created_at, created_by, created_client) VALUES ('bots.x', 'active', now(), 'host', 'host')", "check"),
    ] {
        let err = raw(&pg, sql).await.unwrap_err();
        assert!(err.contains(key), "{sql}: {err}");
    }
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_role_changes_append_only_and_retention() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    for (h, days) in [("gone91", 91), ("gone89", 89)] {
        raw(&pg, &format!(
            "INSERT INTO users (handle, status, created_at, created_by, created_client, deactivated_at, deactivated_by)
             VALUES ('{h}', 'deactivated', now() - interval '200 days', 'host', 'host', now() - interval '{days} days', 'host')"
        ))
        .await
        .unwrap();
    }
    // Old rows, about each of them, an active user, and `*`.
    for t in ["gone91", "gone89", "ana", "*"] {
        raw(
            &pg,
            &format!(
                "INSERT INTO role_changes (at, actor, client, host_user, target, change)
             VALUES (now() - interval '300 days', 'host', 'host', 'opsuser', '{t}', 'set_field')"
            ),
        )
        .await
        .unwrap();
    }
    let n = |t: &'static str| {
        let pool = pg.store.pool().clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM role_changes WHERE target = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    // UPDATE: always refused.
    let err = raw(&pg, "UPDATE role_changes SET detail = 'x'")
        .await
        .unwrap_err();
    assert!(err.contains("append-only"), "{err}");
    // DELETE without the setting, or with it but outside the retention function: refused, even
    // for the 91-day user.
    for sql in [
        "DELETE FROM role_changes WHERE target = 'gone91'",
        "SELECT set_config('pmos.role_changes_retention', 'on', true); DELETE FROM role_changes WHERE target = 'gone91'",
    ] {
        let mut tx = pg.store.pool().begin().await.unwrap();
        let mut failed = None;
        for stmt in sql.split("; ") {
            if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(stmt.to_string()))
                .execute(&mut *tx)
                .await
            {
                failed = Some(e.to_string());
                break;
            }
        }
        assert!(failed.unwrap().contains("append-only"), "{sql}");
    }
    // The audit purge's exception doesn't reach role_changes: its setting, a DELETE ...
    let mut tx = pg.store.pool().begin().await.unwrap();
    sqlx::query("SELECT set_config('pmos.audit_purge', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = sqlx::query("DELETE FROM role_changes")
        .execute(&mut *tx)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("append-only"), "{err}");
    drop(tx);
    // ... and its real delete path, run while the 300-day-old rows are there.
    let mut before = Vec::new();
    for t in ["gone91", "gone89", "ana", "*"] {
        before.push(n(t).await);
    }
    let purged: i64 =
        sqlx::query_scalar("SELECT deleted FROM purge_audit_events(interval '90 days', 1000)")
            .fetch_one(pg.store.pool())
            .await
            .unwrap();
    assert!(purged >= 0);
    for (t, was) in ["gone91", "gone89", "ana", "*"].into_iter().zip(before) {
        assert_eq!(
            n(t).await,
            was,
            "{t}: the audit purge never touches role_changes"
        );
    }
    // The retention function: only the 91-day user's rows go.
    let row = sqlx::query("SELECT deleted, more FROM purge_role_changes(1000)")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("deleted"), 1);
    assert!(!row.get::<bool, _>("more"));
    assert_eq!(n("gone91").await, 0);
    assert_eq!(n("gone89").await, 1, "89 days: kept");
    assert!(n("ana").await >= 1, "active: kept");
    assert_eq!(n("*").await, 1, "`*` rows: never deleted");
    // The setting doesn't open `*` rows or an active user's, even from inside a function
    // named the same way: the trigger checks each row.
    let err = raw(&pg, "DELETE FROM role_changes WHERE target = '*'")
        .await
        .unwrap_err();
    assert!(err.contains("append-only"), "{err}");
    let recorded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE tool = 'users.role_changes_purge' AND client = 'system'",
    )
    .fetch_one(pg.store.pool())
    .await
    .unwrap();
    assert_eq!(recorded, 1);
    pg.drop_schema().await;
}

/// UM5 (review of `b469d5b`, migration 0021): a function with the purge's own name and
/// signature in another schema, put first on the search path and called by its bare name,
/// defeats neither append-only exception, even when it deletes only rows the real purge
/// would (an old audit event; the role history of someone deactivated 91 days ago). Both
/// triggers accept only the real function's frame, with its schema; the real purges still
/// work.
#[tokio::test]
async fn a_same_named_function_on_the_search_path_defeats_neither_purge() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    raw(
        &pg,
        "INSERT INTO users (handle, status, created_at, created_by, created_client, deactivated_at, deactivated_by)
         VALUES ('gone91', 'deactivated', now() - interval '200 days', 'host', 'host', now() - interval '91 days', 'host')",
    )
    .await
    .unwrap();
    raw(
        &pg,
        "INSERT INTO role_changes (at, actor, client, host_user, target, change)
         VALUES (now() - interval '300 days', 'host', 'host', 'opsuser', 'gone91', 'set_field')",
    )
    .await
    .unwrap();
    raw(
        &pg,
        "INSERT INTO audit_events (at, run_id, user_handle, skill, tool, outcome, sources, client, detail)
         VALUES (now() - interval '200 days', 'old', 'ana', NULL, 'docs.search', 'ok', '{}', 'mcp', NULL)",
    )
    .await
    .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    let count = |sql: &'static str| {
        let pool = pg.store.pool().clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let old_events = "SELECT count(*) FROM audit_events WHERE run_id = 'old'";
    let gone_rows = "SELECT count(*) FROM role_changes WHERE target = 'gone91'";
    assert_eq!((count(old_events).await, count(gone_rows).await), (1, 1));

    let evil = format!("{schema}_evil");
    raw(&pg, &format!("CREATE SCHEMA {evil}")).await.unwrap();
    for f in [
        format!(
            "CREATE FUNCTION {evil}.purge_audit_events(retention interval, batch_size integer)
             RETURNS bigint LANGUAGE plpgsql AS $$
             DECLARE n bigint;
             BEGIN
                 PERFORM set_config('pmos.audit_purge', 'on', true);
                 DELETE FROM {schema}.audit_events WHERE at < now() - interval '100 days';
                 GET DIAGNOSTICS n = ROW_COUNT;
                 RETURN n;
             END $$"
        ),
        format!(
            "CREATE FUNCTION {evil}.purge_role_changes(batch_size integer)
             RETURNS bigint LANGUAGE plpgsql AS $$
             DECLARE n bigint;
             BEGIN
                 PERFORM set_config('pmos.role_changes_retention', 'on', true);
                 DELETE FROM {schema}.role_changes WHERE target = 'gone91';
                 GET DIAGNOSTICS n = ROW_COUNT;
                 RETURN n;
             END $$"
        ),
    ] {
        raw(&pg, &f).await.unwrap();
    }
    for call in [
        "SELECT purge_audit_events(interval '90 days', 10)",
        "SELECT purge_role_changes(10)",
    ] {
        let mut tx = pg.store.pool().begin().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SET LOCAL search_path = {evil}, {schema}"
        )))
        .execute(&mut *tx)
        .await
        .unwrap();
        // The look-alike is what the bare name finds.
        let found: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT n.nspname FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE p.oid = '{}'::regprocedure",
            if call.contains("audit") {
                "purge_audit_events(interval,integer)"
            } else {
                "purge_role_changes(integer)"
            }
        )))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(found, evil);
        let err = sqlx::query(sqlx::AssertSqlSafe(call.to_string()))
            .execute(&mut *tx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("append-only"), "{call}: {err}");
        tx.rollback().await.unwrap();
    }
    assert_eq!(
        (count(old_events).await, count(gone_rows).await),
        (1, 1),
        "nothing went"
    );
    raw(&pg, &format!("DROP SCHEMA {evil} CASCADE"))
        .await
        .unwrap();
    // The real purges, by their bare names on the normal search path, still do their work.
    let purged: i64 =
        sqlx::query_scalar("SELECT deleted FROM purge_audit_events(interval '90 days', 1000)")
            .fetch_one(pg.store.pool())
            .await
            .unwrap();
    assert!(purged >= 1);
    assert_eq!(count(old_events).await, 0);
    let row = sqlx::query("SELECT deleted FROM purge_role_changes(1000)")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("deleted"), 1);
    assert_eq!(count(gone_rows).await, 0);
    // Both functions and both triggers run with the fixed path.
    let paths: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT p.proname || ' ' || array_to_string(p.proconfig, ',')
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = '{schema}' AND p.proname IN ('purge_audit_events', 'audit_events_append_only',
               'purge_role_changes', 'role_changes_append_only')
         ORDER BY 1"
    )))
    .fetch_all(pg.store.pool())
    .await
    .unwrap();
    assert_eq!(paths.len(), 4, "{paths:?}");
    for p in &paths {
        assert!(p.ends_with("search_path=pg_catalog, pg_temp"), "{p}");
    }
    pg.drop_schema().await;
}

/// L8 (review of `b469d5b`): an address a user had is never anyone else's, even after it
/// changed, in the database itself (raw SQL, no application check); the same user may have
/// it back.
#[tokio::test]
async fn trigger_an_old_address_is_never_reused() {
    let Some(pg) = pg().await else { return };
    two_admins(&pg).await;
    let sign_in: String = sqlx::query_scalar("SELECT sign_in_name FROM users WHERE handle = 'ana'")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    raw(
        &pg,
        "UPDATE users SET sign_in_name = 'ana.new@contoso.com', github_login = 'Ana-New' WHERE handle = 'ana'",
    )
    .await
    .unwrap();
    for sql in [
        format!("UPDATE users SET sign_in_name = '{sign_in}' WHERE handle = 'raj'"),
        format!(
            "INSERT INTO users (handle, sign_in_name, status, created_at, created_by, created_client)
             VALUES ('newbie', '{sign_in}', 'active', now(), 'host', 'host')"
        ),
    ] {
        let err = raw(&pg, &sql).await.unwrap_err();
        assert!(
            err.contains("taken") && err.contains("never reused"),
            "{sql}: {err}"
        );
    }
    // The retired values can't be edited away.
    for sql in [
        "DELETE FROM retired_addresses",
        "UPDATE retired_addresses SET handle = 'raj'",
        "TRUNCATE retired_addresses",
    ] {
        let err = raw(&pg, sql).await.unwrap_err();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
    // Ana may have her own old name back.
    raw(
        &pg,
        &format!("UPDATE users SET sign_in_name = '{sign_in}' WHERE handle = 'ana'"),
    )
    .await
    .unwrap();
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_bootstrap_refuses_a_table_of_only_deactivated_users() {
    let Some(pg) = pg().await else { return };
    // A state the last-admin trigger never lets commit, built with it off, to prove the
    // bootstrap's own "any row" check stands alone.
    let mut tx = pg.store.pool().begin().await.unwrap();
    for sql in [
        "ALTER TABLE users DISABLE TRIGGER users_invariants",
        "INSERT INTO users (handle, status, created_at, created_by, created_client, deactivated_at, deactivated_by)
         VALUES ('old', 'deactivated', now(), 'host', 'host', now(), 'host')",
        "ALTER TABLE users ENABLE TRIGGER users_invariants",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let store: &dyn UserStore = &pg.store;
    let r = store
        .apply(&importer(), import(TEAM, ImportMode::Bootstrap), Utc::now())
        .await;
    assert_eq!(refusal(r), Refusal::NotEmpty);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(users, 1);
    pg.drop_schema().await;
}

#[tokio::test]
async fn trigger_directory_version_moves_with_every_change() {
    let Some(pg) = pg().await else { return };
    let v = || async {
        sqlx::query_scalar::<_, i64>("SELECT v FROM directory_version")
            .fetch_one(pg.store.pool())
            .await
            .unwrap()
    };
    let v0 = v().await;
    two_admins(&pg).await;
    let v1 = v().await;
    assert!(v1 > v0);
    raw(
        &pg,
        "UPDATE users SET display_name = 'Ana' WHERE handle = 'ana'",
    )
    .await
    .unwrap();
    assert!(v().await > v1);
    // A refused change doesn't move it.
    let v2 = v().await;
    let _ = raw(&pg, "DELETE FROM user_roles WHERE role = 'admin'").await;
    assert_eq!(v().await, v2);
    pg.drop_schema().await;
}
