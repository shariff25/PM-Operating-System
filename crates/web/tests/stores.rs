//! The same contract tests against the in-memory stores and Postgres. The Postgres cases need
//! `PMOS_TEST_DATABASE_URL`; without it they skip (and fail in CI, where `CI` is set, so they
//! can't be skipped there by accident).

use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use pmos_web::followups::{
    followup_id, Changed, FollowUpStore, Kind, MemoryFollowUpStore, NewFollowUp,
};
use pmos_web::pg::PgStore;
use pmos_web::session::{hash_id, random_token, MemorySessionStore, Session, SessionStore};
use pmos_web::settings::{
    DraftEdit, MemorySettingsStore, PracticeDraft, SettingsStore, StoredSuggestion, UserState,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::Connection;

struct Stores {
    sessions: Arc<dyn SessionStore>,
    settings: Arc<dyn SettingsStore>,
    followups: Arc<dyn FollowUpStore>,
}

fn memory() -> Stores {
    Stores {
        sessions: Arc::new(MemorySessionStore::new()),
        settings: Arc::new(MemorySettingsStore::new()),
        followups: Arc::new(MemoryFollowUpStore::new()),
    }
}

fn of(pg: PgStore) -> Stores {
    let pg = Arc::new(pg);
    Stores {
        sessions: pg.clone(),
        settings: pg.clone(),
        followups: pg,
    }
}

/// A Postgres store in a fresh schema.
struct TestPg {
    stores: Stores,
    base: PgConnectOptions,
    schema: String,
    pg: PgStore,
}

impl TestPg {
    async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("PMOS_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "PMOS_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: PMOS_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("PMOS_TEST_DATABASE_URL parses");
        let schema = format!(
            "w_{}",
            random_token().to_lowercase().replace(['-', '_'], "x")
        );
        let mut admin = sqlx::PgConnection::connect_with(&base)
            .await
            .expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pg = Self::open(&base, &schema).await;
        Some(Self {
            stores: of(pg.clone()),
            base,
            schema,
            pg,
        })
    }

    async fn open(base: &PgConnectOptions, schema: &str) -> PgStore {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(base.clone().options([("search_path", schema)]))
            .await
            .expect("connect to test schema");
        let pg = PgStore::from_pool(pool);
        pg.migrate().await.expect("migrations apply");
        pg
    }

    /// A new pool on the same schema, as a restarted process would open.
    async fn reconnect(&self) -> Stores {
        of(Self::open(&self.base, &self.schema).await)
    }

    async fn drop_schema(self) {
        self.pg.pool().close().await;
        if let Ok(mut admin) = sqlx::PgConnection::connect_with(&self.base).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&mut admin)
            .await;
        }
    }
}

fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-16T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::minutes(minutes)
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
}

fn session(handle: &str, minutes: i64) -> Session {
    Session {
        handle: handle.into(),
        csrf_token: random_token(),
        created_at: at(minutes),
        last_seen: at(minutes),
        entra_oid: None,
        entra_tid: None,
        password_epoch: None,
    }
}

// Sessions.

async fn sessions_expire_idle_and_absolutely(s: &Stores) {
    let (idle, max) = (Duration::hours(8), Duration::hours(24));
    let id = hash_id("one");
    let mine = session("ana", 0);
    s.sessions.insert(id, mine.clone()).await.unwrap();
    // Kept alive by use every 7 hours, and `last_seen` moves with it.
    for step in 1..=3 {
        let got = s
            .sessions
            .touch(&id, at(step * 7 * 60), idle, max)
            .await
            .unwrap()
            .expect("live");
        assert_eq!(got.handle, "ana");
        assert_eq!(got.csrf_token, mine.csrf_token);
        assert_eq!(got.created_at, at(0));
        assert_eq!(got.last_seen, at(step * 7 * 60));
    }
    // Never past 24 hours from sign-in, even though it was used 3 hours ago.
    assert!(s
        .sessions
        .touch(&id, at(24 * 60), idle, max)
        .await
        .unwrap()
        .is_none());
    // And it stays gone.
    assert!(s
        .sessions
        .touch(&id, at(24 * 60 + 1), idle, max)
        .await
        .unwrap()
        .is_none());

    // Idle: 8 hours unused is over, 7h59m isn't.
    let id = hash_id("two");
    s.sessions.insert(id, session("ana", 0)).await.unwrap();
    assert!(s
        .sessions
        .touch(&id, at(8 * 60 - 1), idle, max)
        .await
        .unwrap()
        .is_some());
    let id = hash_id("three");
    s.sessions.insert(id, session("ana", 0)).await.unwrap();
    assert!(s
        .sessions
        .touch(&id, at(8 * 60), idle, max)
        .await
        .unwrap()
        .is_none());
    // An id never inserted.
    assert!(s
        .sessions
        .touch(&hash_id("nope"), at(1), idle, max)
        .await
        .unwrap()
        .is_none());
}

async fn sessions_end_by_id_and_by_user_and_purge(s: &Stores) {
    let (idle, max) = (Duration::hours(8), Duration::hours(24));
    for (name, handle) in [("a1", "ana"), ("a2", "ana"), ("b1", "ben")] {
        s.sessions
            .insert(hash_id(name), session(handle, 0))
            .await
            .unwrap();
    }
    s.sessions.remove(&hash_id("a1")).await.unwrap();
    s.sessions.remove(&hash_id("a1")).await.unwrap();
    assert!(s
        .sessions
        .touch(&hash_id("a1"), at(1), idle, max)
        .await
        .unwrap()
        .is_none());
    s.sessions.remove_user("ana").await.unwrap();
    assert!(s
        .sessions
        .touch(&hash_id("a2"), at(1), idle, max)
        .await
        .unwrap()
        .is_none());
    // Ben's is untouched by any of it.
    assert!(s
        .sessions
        .touch(&hash_id("b1"), at(2), idle, max)
        .await
        .unwrap()
        .is_some());

    // Purge removes the expired and only them.
    s.sessions
        .insert(hash_id("old"), session("cy", 0))
        .await
        .unwrap();
    s.sessions
        .insert(hash_id("new"), session("cy", 600))
        .await
        .unwrap();
    s.sessions.purge(at(9 * 60), idle, max).await.unwrap();
    assert!(s
        .sessions
        .touch(&hash_id("old"), at(9 * 60), idle, max)
        .await
        .unwrap()
        .is_none());
    assert!(s
        .sessions
        .touch(&hash_id("new"), at(9 * 60), idle, max)
        .await
        .unwrap()
        .is_some());
}

// Settings.

fn busy_state() -> impl FnOnce(&mut UserState) + Send {
    |st| {
        st.nudges = false;
        st.mascot = false;
        st.opened_a_draft = true;
        st.hints_seen.insert("ask".into());
        st.hints_seen.insert("sources".into());
        st.edits.insert(
            "apr_1".into(),
            DraftEdit {
                title: Some("Title".into()),
                body: "Edited body\nwith lines".into(),
            },
        );
        st.edits.insert(
            "apr_2".into(),
            DraftEdit {
                title: None,
                body: "No title".into(),
            },
        );
        st.suggestions.insert(
            "apr_1".into(),
            StoredSuggestion {
                task_id: Some("tsk_1".into()),
                instruction: "shorter".into(),
                status: "ready".into(),
                body: Some("Short".into()),
                error: None,
            },
        );
        st.practice = Some(PracticeDraft {
            id: "prc_1".into(),
            status: "pending".into(),
            created_at: "2026-11-16T09:00:00Z".into(),
            decided_at: None,
        });
        st.external.insert("apr_2".into());
        st.revision_tasks.insert("tsk_1".into());
        st.ask_persona = Some("chief".into());
    }
}

async fn settings_keep_each_pms_state_apart(s: &Stores) {
    assert_eq!(s.settings.get("ana").await.unwrap(), UserState::default());
    let ana = s
        .settings
        .update("ana", Box::new(busy_state()))
        .await
        .unwrap();
    assert!(!ana.nudges && !ana.mascot && ana.opened_a_draft && ana.edits.len() == 2);
    assert_eq!(ana.ask_persona.as_deref(), Some("chief"));
    assert_eq!(s.settings.get("ana").await.unwrap(), ana);
    // Ben has none of it.
    assert_eq!(s.settings.get("ben").await.unwrap(), UserState::default());
    s.settings
        .update(
            "ben",
            Box::new(|st| {
                st.edits.insert(
                    "apr_1".into(),
                    DraftEdit {
                        title: None,
                        body: "Ben's".into(),
                    },
                );
            }),
        )
        .await
        .unwrap();
    assert_eq!(s.settings.get("ana").await.unwrap(), ana);
    assert_eq!(
        s.settings.get("ben").await.unwrap().edits["apr_1"].body,
        "Ben's"
    );
}

async fn settings_changes_and_removals_are_kept(s: &Stores) {
    s.settings
        .update("ana", Box::new(busy_state()))
        .await
        .unwrap();
    let after = s
        .settings
        .update(
            "ana",
            Box::new(|st| {
                st.forget_draft("apr_1");
                st.edits.get_mut("apr_2").unwrap().body = "Changed".into();
                st.hints_seen.remove("ask");
                st.practice.as_mut().unwrap().status = "approved".into();
                st.external.clear();
                st.revision_tasks.insert("tsk_2".into());
            }),
        )
        .await
        .unwrap();
    assert_eq!(s.settings.get("ana").await.unwrap(), after);
    assert_eq!(after.edits.keys().collect::<Vec<_>>(), ["apr_2"]);
    assert_eq!(after.edits["apr_2"].body, "Changed");
    assert!(after.suggestions.is_empty() && after.external.is_empty());
    assert_eq!(after.revision_tasks.len(), 2);
    assert_eq!(after.practice.unwrap().status, "approved");
    // Dropping the practice draft.
    let st = s
        .settings
        .update("ana", Box::new(|st| st.practice = None))
        .await
        .unwrap();
    assert!(st.practice.is_none() && s.settings.get("ana").await.unwrap().practice.is_none());
}

async fn settings_updates_take_turns(s: &Stores) {
    // Twenty "tabs" each add their own hint: none may be lost.
    let mut tasks = Vec::new();
    for n in 0..20 {
        let settings = s.settings.clone();
        tasks.push(tokio::spawn(async move {
            settings
                .update(
                    "ana",
                    Box::new(move |st| {
                        st.hints_seen.insert(format!("h{n}"));
                        st.edits.insert(
                            format!("d{n}"),
                            DraftEdit {
                                title: None,
                                body: n.to_string(),
                            },
                        );
                    }),
                )
                .await
                .unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let st = s.settings.get("ana").await.unwrap();
    assert_eq!(st.hints_seen.len(), 20);
    assert_eq!(st.edits.len(), 20);
}

// Follow-ups.

fn new_followup(kind: Kind, system: &str, who: &str, n: u32) -> NewFollowUp {
    NewFollowUp {
        kind,
        system: system.into(),
        who: who.into(),
        summary: format!("about {n}"),
        url: format!("https://example.com/{system}/{n}"),
        created: None,
    }
}

fn changed(title: &str) -> Changed {
    Changed {
        system: "jira".into(),
        title: title.into(),
        summary: "by Ben".into(),
        url: "https://jira.example.com/PAY-1".into(),
    }
}

async fn followups_are_listed_per_pm_and_expire(s: &Stores) {
    let f = &s.followups;
    assert!(f.list("ana", at(0)).await.unwrap().is_empty());
    assert!(f.day("ana", at(0)).await.unwrap().is_none());
    f.replace(
        "ana",
        day(),
        vec![changed("PAY-1 blocked")],
        vec![
            new_followup(Kind::WaitingOnYou, "slack", "Ben", 1),
            new_followup(Kind::YouWaitOn, "jira", "Legal", 2),
        ],
        None,
        at(0),
    )
    .await
    .unwrap();
    let list = f.list("ana", at(1)).await.unwrap();
    assert_eq!(list.len(), 2);
    let slack = list.iter().find(|x| x.system == "slack").unwrap();
    assert_eq!(slack.kind, Kind::WaitingOnYou);
    assert_eq!(
        slack.id,
        followup_id("ana", Kind::WaitingOnYou, "slack", &slack.url)
    );
    assert_eq!((slack.created, slack.expires), (at(0), at(24 * 60)));
    let jira = list.iter().find(|x| x.system == "jira").unwrap();
    assert_eq!(jira.expires, at(7 * 24 * 60));
    assert!(!jira.dismissed);
    let day_back = f.day("ana", at(1)).await.unwrap().unwrap();
    assert_eq!(
        (day_back.date, day_back.changed),
        (day(), vec![changed("PAY-1 blocked")])
    );

    // Ben sees nothing of it, whatever id he asks for.
    assert!(f.list("ben", at(1)).await.unwrap().is_empty());
    assert!(f.day("ben", at(1)).await.unwrap().is_none());
    assert!(f.open("ben", &slack.id, at(1)).await.unwrap().is_none());
    assert!(!f.dismiss("ben", &slack.id, at(1)).await.unwrap());
    assert!(f.open("ana", &slack.id, at(1)).await.unwrap().is_some());

    // The Slack one expires after a day (and the changed items), the other after a week.
    assert_eq!(f.list("ana", at(24 * 60 - 1)).await.unwrap().len(), 2);
    let later = f.list("ana", at(24 * 60)).await.unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].system, "jira");
    assert!(f.day("ana", at(24 * 60)).await.unwrap().is_none());
    assert!(f
        .open("ana", &slack.id, at(24 * 60))
        .await
        .unwrap()
        .is_none());
    assert!(!f.dismiss("ana", &slack.id, at(24 * 60)).await.unwrap());
    assert!(f.list("ana", at(7 * 24 * 60)).await.unwrap().is_empty());

    // Purge deletes the expired and leaves the rest.
    assert!(f.purge(at(24 * 60)).await.unwrap() >= 1);
    assert_eq!(f.list("ana", at(24 * 60)).await.unwrap().len(), 1);
    assert!(f.purge(at(7 * 24 * 60)).await.unwrap() >= 1);
    assert_eq!(f.purge(at(7 * 24 * 60)).await.unwrap(), 0);
}

async fn followups_dismiss_and_replace(s: &Stores) {
    let f = &s.followups;
    let a = new_followup(Kind::WaitingOnYou, "jira", "Ben", 1);
    let b = new_followup(Kind::WaitingOnYou, "jira", "Cy", 2);
    f.replace(
        "ana",
        day(),
        vec![],
        vec![a.clone(), b.clone()],
        None,
        at(0),
    )
    .await
    .unwrap();
    let id_a = a.id("ana");
    // Dismiss twice: both say it was there; it is not listed or open either way.
    assert!(f.dismiss("ana", &id_a, at(1)).await.unwrap());
    assert!(f.dismiss("ana", &id_a, at(2)).await.unwrap());
    assert!(!f.dismiss("ana", "fu_unknown", at(2)).await.unwrap());
    let list = f.list("ana", at(3)).await.unwrap();
    assert_eq!(
        list.iter().map(|x| x.id.clone()).collect::<Vec<_>>(),
        [b.id("ana")]
    );
    assert!(f.open("ana", &id_a, at(3)).await.unwrap().is_none());

    // The producer sends the same ones again, a day later, plus a new one: the dismissed one
    // stays dismissed, `created` is kept, the expiry moves.
    let mut a2 = a.clone();
    a2.summary = "updated".into();
    let c = new_followup(Kind::YouWaitOn, "jira", "Di", 3);
    f.replace(
        "ana",
        day(),
        vec![],
        vec![a2, b.clone(), b.clone(), c.clone()],
        None,
        at(60),
    )
    .await
    .unwrap();
    let list = f.list("ana", at(61)).await.unwrap();
    assert_eq!(list.len(), 2, "{list:?}");
    let kept = list.iter().find(|x| x.id == b.id("ana")).unwrap();
    assert_eq!(kept.created, at(0));
    assert_eq!(kept.expires, at(60 + 7 * 24 * 60));
    assert!(list.iter().any(|x| x.id == c.id("ana")));
    assert!(f.open("ana", &id_a, at(61)).await.unwrap().is_none());
    // Dropped by the producer: gone.
    f.replace("ana", day(), vec![], vec![c.clone()], None, at(120))
        .await
        .unwrap();
    assert_eq!(f.list("ana", at(121)).await.unwrap().len(), 1);
    assert!(!f.dismiss("ana", &b.id("ana"), at(121)).await.unwrap());
    // A producer-supplied `created` is used for a new one.
    let mut old = new_followup(Kind::YouWaitOn, "jira", "Ed", 4);
    old.created = Some(at(-3 * 24 * 60));
    f.replace("ana", day(), vec![], vec![old.clone()], None, at(130))
        .await
        .unwrap();
    assert_eq!(
        f.list("ana", at(131)).await.unwrap()[0].created,
        at(-3 * 24 * 60)
    );
    // Replacing Ana's leaves Ben's alone.
    f.replace(
        "ben",
        day(),
        vec![changed("x")],
        vec![c.clone()],
        None,
        at(140),
    )
    .await
    .unwrap();
    f.replace("ana", day(), vec![], vec![], None, at(150))
        .await
        .unwrap();
    assert!(f.list("ana", at(151)).await.unwrap().is_empty());
    assert_eq!(f.list("ben", at(151)).await.unwrap().len(), 1);
    assert_ne!(c.id("ana"), c.id("ben"));
}

async fn sessions_keep_the_entra_account(s: &Stores) {
    let (idle, max) = (Duration::hours(8), Duration::hours(24));
    let id = hash_id(&random_token());
    let mut entra = session("ana", 0);
    entra.entra_oid = Some("00000000-0000-0000-0000-00000000a0a0".into());
    entra.entra_tid = Some("11111111-2222-3333-4444-555555555555".into());
    s.sessions.insert(id, entra.clone()).await.unwrap();
    let back = s
        .sessions
        .touch(&id, at(1), idle, max)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.entra_oid, entra.entra_oid);
    assert_eq!(back.entra_tid, entra.entra_tid);
    let dev = hash_id(&random_token());
    s.sessions.insert(dev, session("ben", 0)).await.unwrap();
    let back = s
        .sessions
        .touch(&dev, at(1), idle, max)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((back.entra_oid, back.entra_tid), (None, None));
    assert_eq!(back.password_epoch, None);
    // A password sign-in keeps the credential's epoch it signed in with.
    let pw = hash_id(&random_token());
    let mut with_epoch = session("cy", 0);
    with_epoch.password_epoch = Some(7);
    s.sessions.insert(pw, with_epoch).await.unwrap();
    let back = s
        .sessions
        .touch(&pw, at(1), idle, max)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.password_epoch, Some(7));
    s.sessions.ping().await.unwrap();
}

async fn a_delivery_is_remembered_after_its_items_go(s: &Stores) {
    assert_eq!(s.followups.delivered("ana").await.unwrap(), None);
    s.followups
        .replace(
            "ana",
            day(),
            vec![changed("t")],
            vec![],
            Some("chief"),
            at(0),
        )
        .await
        .unwrap();
    assert_eq!(s.followups.delivered("ana").await.unwrap(), Some(at(0)));
    // Who wrote it (slice R2) is kept with the day.
    let d = s.followups.day("ana", at(1)).await.unwrap().unwrap();
    assert_eq!(d.persona.as_deref(), Some("chief"));
    // A day and more later the items are purged; when it was delivered is not.
    let later = at(25 * 60);
    s.followups.purge(later).await.unwrap();
    assert!(s.followups.day("ana", later).await.unwrap().is_none());
    assert_eq!(s.followups.delivered("ana").await.unwrap(), Some(at(0)));
    assert_eq!(s.followups.delivered("ben").await.unwrap(), None);
    // A new delivery moves it on.
    s.followups
        .replace("ana", day(), vec![], vec![], None, later)
        .await
        .unwrap();
    assert_eq!(s.followups.delivered("ana").await.unwrap(), Some(later));
    let d = s.followups.day("ana", later).await.unwrap().unwrap();
    assert_eq!(
        d.persona, None,
        "Nudge's brief replaces Chief's attribution"
    );
}

// Running the contract.

macro_rules! contract {
    ($($name:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[tokio::test]
                async fn $name() {
                    super::$name(&memory()).await;
                }
            )*
        }
        mod postgres {
            use super::*;
            $(
                #[tokio::test]
                async fn $name() {
                    let Some(pg) = TestPg::new().await else { return };
                    super::$name(&pg.stores).await;
                    pg.drop_schema().await;
                }
            )*
        }
    };
}

contract!(
    sessions_expire_idle_and_absolutely,
    sessions_end_by_id_and_by_user_and_purge,
    settings_keep_each_pms_state_apart,
    settings_changes_and_removals_are_kept,
    settings_updates_take_turns,
    followups_are_listed_per_pm_and_expire,
    followups_dismiss_and_replace,
    sessions_keep_the_entra_account,
    a_delivery_is_remembered_after_its_items_go,
);

/// A restart: what one store handle wrote, a new connection pool reads back.
#[tokio::test]
async fn postgres_data_survives_a_restart() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let (idle, max) = (Duration::hours(8), Duration::hours(24));
    let id = hash_id("cookie");
    let mine = session("ana", 0);
    pg.stores.sessions.insert(id, mine.clone()).await.unwrap();
    pg.stores
        .settings
        .update("ana", Box::new(busy_state()))
        .await
        .unwrap();
    let written = pg.stores.settings.get("ana").await.unwrap();
    let a = new_followup(Kind::YouWaitOn, "jira", "Legal", 1);
    pg.stores
        .followups
        .replace(
            "ana",
            day(),
            vec![changed("t")],
            vec![a.clone()],
            None,
            at(0),
        )
        .await
        .unwrap();
    pg.stores
        .followups
        .dismiss("ana", &a.id("ana"), at(1))
        .await
        .unwrap();
    pg.stores
        .followups
        .replace("ben", day(), vec![], vec![a.clone()], None, at(1))
        .await
        .unwrap();

    let again = pg.reconnect().await;
    let back = again
        .sessions
        .touch(&id, at(60), idle, max)
        .await
        .unwrap()
        .expect("the session survived");
    assert_eq!(back.csrf_token, mine.csrf_token);
    assert_eq!(again.settings.get("ana").await.unwrap(), written);
    assert!(again.followups.list("ana", at(2)).await.unwrap().is_empty());
    assert_eq!(again.followups.list("ben", at(2)).await.unwrap().len(), 1);
    assert_eq!(
        again
            .followups
            .day("ana", at(2))
            .await
            .unwrap()
            .unwrap()
            .changed,
        vec![changed("t")]
    );
    pg.drop_schema().await;
}

/// The store keeps only the hash of a session id and no follow-up text beyond its one line.
#[tokio::test]
async fn postgres_stores_the_hash_not_the_cookie() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let cookie = random_token();
    pg.stores
        .sessions
        .insert(hash_id(&cookie), session("ana", 0))
        .await
        .unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM web_sessions WHERE id_hash = $1")
        .bind(&hash_id(&cookie)[..])
        .fetch_one(pg.pg.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM web_sessions WHERE id_hash = $1 OR csrf_token = $2 OR handle = $2",
    )
    .bind(cookie.as_bytes())
    .bind(&cookie)
    .fetch_one(pg.pg.pool())
    .await
    .unwrap();
    assert_eq!(n, 0);
    pg.drop_schema().await;
}
