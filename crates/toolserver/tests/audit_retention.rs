//! Audit retention (`docs/audit-retention.md`, migration 0013): events older than the
//! retention are deleted by the one controlled purge, which records its own run; nothing newer
//! than 90 days can be deleted by anyone, whatever they try (a direct DELETE, a forged
//! setting, a look-alike function, a short retention, UPDATE, TRUNCATE); runs are idempotent,
//! bounded, and safe to run twice at once. In Postgres (with `PMOS_TEST_DATABASE_URL`) and
//! in memory.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::TestPg;
use pmos_toolserver::audit::{
    parse_purge_detail, AuditEvent, AuditQuery, AuditSink, Client, MemoryAudit, Outcome, Purged,
    PURGE_ACTOR, PURGE_TOOL,
};
use pmos_toolserver::retention;
use sqlx::Row;

fn event(at: DateTime<Utc>, tool: &str) -> AuditEvent {
    AuditEvent {
        at,
        client: Client::Mcp,
        run_id: "tsk_1".into(),
        user: "ana".into(),
        skill: Some("ask".into()),
        bot: None,
        tool: tool.into(),
        outcome: Outcome::Ok,
        sources: vec![],
        source_titles: BTreeMap::new(),
        args_hash: None,
        output_ref: None,
        detail: None,
    }
}

async fn tools(audit: &dyn AuditSink) -> Vec<String> {
    let mut found = audit
        .search(&AuditQuery {
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    found.sort_by_key(|s| s.id);
    found.into_iter().map(|s| s.event.tool).collect()
}

/// An old event (120 days), one just inside the window (89 days), and a new one.
async fn seed(audit: &dyn AuditSink, now: DateTime<Utc>) {
    for (days, tool) in [(120, "old.read"), (89, "recent.read"), (0, "new.read")] {
        audit
            .record(event(now - Duration::days(days), tool))
            .await
            .unwrap();
    }
}

async fn purge_deletes_only_old_events_and_records_itself(audit: &dyn AuditSink) {
    let now = Utc::now();
    seed(audit, now).await;
    let Some(Purged::Ran(done)) = audit.purge(90, 1000, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!(done.deleted, 1);
    assert!(!done.more);
    assert!((now - Duration::days(90) - done.cutoff).num_seconds().abs() < 120);
    assert_eq!(
        tools(audit).await,
        ["recent.read", "new.read", PURGE_TOOL],
        "only the 120-day-old event went, and the run is recorded"
    );
    let runs = audit
        .search(&AuditQuery {
            limit: 10,
            action: Some(PURGE_TOOL.into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    let run = &runs[0].event;
    assert_eq!(run.client, Client::System);
    assert_eq!(run.user, PURGE_ACTOR);
    assert_eq!(run.outcome, Outcome::Ok);
    assert!(run.sources.is_empty() && run.args_hash.is_none());
    let (n, cutoff) = parse_purge_detail(run.detail.as_deref().unwrap()).unwrap();
    assert_eq!(n, 1);
    assert!((cutoff - done.cutoff).num_seconds().abs() <= 1);

    // Idempotent: again, nothing more goes, and that run is recorded too.
    let Some(Purged::Ran(again)) = audit.purge(90, 1000, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!(again.deleted, 0);
    assert_eq!(
        tools(audit).await,
        ["recent.read", "new.read", PURGE_TOOL, PURGE_TOOL]
    );
    let facts = audit.retention().await.unwrap().unwrap();
    assert_eq!(facts.last_purge.unwrap().deleted, 0);
    let oldest = facts.oldest_event_at.unwrap();
    assert!((oldest - (now - Duration::days(89))).num_seconds().abs() < 2);

    // A longer retention keeps longer: nothing under 365 days goes.
    audit
        .record(event(now - Duration::days(200), "older.read"))
        .await
        .unwrap();
    let Some(Purged::Ran(long)) = audit.purge(365, 1000, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!(long.deleted, 0);
    assert!(tools(audit).await.contains(&"older.read".to_string()));
}

async fn short_retentions_are_refused(audit: &dyn AuditSink) {
    let now = Utc::now();
    seed(audit, now).await;
    for days in [0, 1, 30, 89] {
        let err = audit.purge(days, 1000, now).await.unwrap_err();
        assert!(err.0.contains("at least 90 days"), "{days}: {err}");
    }
    for batch in [0, 100_001] {
        assert!(audit.purge(90, batch, now).await.is_err(), "{batch}");
    }
    assert_eq!(
        tools(audit).await,
        ["old.read", "recent.read", "new.read"],
        "nothing deleted and nothing recorded"
    );
}

async fn runs_are_bounded(audit: &dyn AuditSink) {
    let now = Utc::now();
    for i in 0..5 {
        audit
            .record(event(now - Duration::days(100 + i), "old.read"))
            .await
            .unwrap();
    }
    audit.record(event(now, "new.read")).await.unwrap();
    let Some(Purged::Ran(first)) = audit.purge(90, 2, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!((first.deleted, first.more), (2, true));
    // The oldest go first.
    let left = audit
        .search(&AuditQuery {
            limit: 100,
            action: Some("old.read".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    let oldest_left = left.iter().map(|s| s.event.at).min().unwrap();
    assert!(oldest_left > now - Duration::days(103));
    // A run goes on batch by batch until nothing is left.
    let report = retention::run(audit, 90, 2, now).await.unwrap().unwrap();
    assert_eq!(report.deleted, 3);
    assert_eq!(report.batches, 2);
    assert!(!report.busy && !report.more);
    let all = tools(audit).await;
    assert!(!all.contains(&"old.read".to_string()));
    assert!(all.contains(&"new.read".to_string()));
    assert_eq!(all.iter().filter(|t| *t == PURGE_TOOL).count(), 3);
}

#[tokio::test]
async fn memory_purges_like_postgres() {
    purge_deletes_only_old_events_and_records_itself(&MemoryAudit::default()).await;
    short_retentions_are_refused(&MemoryAudit::default()).await;
    runs_are_bounded(&MemoryAudit::default()).await;
}

#[tokio::test]
async fn a_log_that_cant_be_purged_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let file = pmos_toolserver::audit::JsonlAudit::open(&dir.path().join("a.jsonl")).unwrap();
    assert_eq!(file.purge(90, 10, Utc::now()).await.unwrap(), None);
    assert_eq!(
        retention::run(&file, 90, 10, Utc::now()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn a_tee_records_the_purge_in_its_other_sinks() {
    let first = Arc::new(MemoryAudit::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.jsonl");
    let file = Arc::new(pmos_toolserver::audit::JsonlAudit::open(&path).unwrap());
    let tee = pmos_toolserver::audit::TeeAudit(vec![first.clone(), file]);
    let now = Utc::now();
    tee.record(event(now - Duration::days(100), "old.read"))
        .await
        .unwrap();
    tee.purge(90, 10, now).await.unwrap().unwrap();
    let lines = std::fs::read_to_string(&path).unwrap();
    let last: serde_json::Value = serde_json::from_str(lines.lines().last().unwrap()).unwrap();
    assert_eq!(last["tool"], PURGE_TOOL);
    assert_eq!(last["client"], "system");
    assert_eq!(
        last["detail"].as_str().unwrap().split(' ').nth(1),
        Some("1")
    );
    // The file keeps the old event: PM OS never deletes from it.
    assert!(lines.contains("old.read"));
}

// Postgres.

#[tokio::test]
async fn postgres_purges_only_old_events_and_records_itself() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    purge_deletes_only_old_events_and_records_itself(&pg.store).await;
    pg.drop_schema().await;
}

#[tokio::test]
async fn postgres_refuses_short_retentions() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    short_retentions_are_refused(&pg.store).await;
    // Straight to the function, past the tool server's own check: the database refuses.
    for retention in ["30 days", "89 days 23 hours", "1 month"] {
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT * FROM purge_audit_events(interval '{retention}', 100)"
        )))
        .execute(pg.store.pool())
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("at least 90 days"),
            "{retention}: {err}"
        );
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(n, 3);
    pg.drop_schema().await;
}

#[tokio::test]
async fn postgres_runs_are_bounded() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    runs_are_bounded(&pg.store).await;
    pg.drop_schema().await;
}

/// Every other way to remove or change an event fails, and a newer event can't be deleted
/// even by someone who forges the purge's setting or writes a function with its name.
#[tokio::test]
async fn postgres_nothing_else_deletes_and_nothing_new_is_ever_deleted() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let now = Utc::now();
    seed(&pg.store, now).await;
    let pool = pg.store.pool();
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
            .fetch_one(pool)
            .await
            .unwrap()
    };
    let refused = |sql: &'static str| async move {
        let mut tx = pool.begin().await.unwrap();
        let mut last = Ok(());
        for statement in sql.split(";;") {
            last = sqlx::query(sqlx::AssertSqlSafe(statement.to_string()))
                .execute(&mut *tx)
                .await
                .map(|_| ());
            if last.is_err() {
                break;
            }
        }
        let err = last.expect_err(sql);
        tx.rollback().await.ok();
        assert!(
            err.to_string().contains("append-only"),
            "{sql}: refused for the wrong reason: {err}"
        );
    };
    // Plain statements, even of the old row only.
    refused("DELETE FROM audit_events").await;
    refused("DELETE FROM audit_events WHERE at < now() - interval '100 days'").await;
    refused("UPDATE audit_events SET outcome = 'denied'").await;
    refused("UPDATE audit_events SET at = now() - interval '1000 days'").await;
    refused("TRUNCATE audit_events").await;
    // The setting forged: still refused, old row or new.
    refused(
        "SET LOCAL pmos.audit_purge = 'on';; DELETE FROM audit_events WHERE at < now() - interval '100 days'",
    )
    .await;
    refused("SET LOCAL pmos.audit_purge = 'on';; DELETE FROM audit_events").await;
    refused(
        "SELECT set_config('pmos.audit_purge', 'on', false);; DELETE FROM audit_events WHERE at > now() - interval '1 day'",
    )
    .await;
    refused("SET LOCAL pmos.audit_purge = 'on';; TRUNCATE audit_events").await;
    refused("SET LOCAL pmos.audit_purge = 'on';; UPDATE audit_events SET outcome = 'denied'").await;
    assert_eq!(count().await, 3);

    // A look-alike: a function with the purge's own name and signature, in another schema,
    // that sets the setting and deletes everything. The trigger's own age check still stops
    // it at the first event newer than 90 days, and the whole statement fails.
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await
        .unwrap();
    let evil = format!("{schema}_evil");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {evil}")))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION {evil}.purge_audit_events(retention interval, batch_size integer)
         RETURNS bigint LANGUAGE plpgsql AS $$
         DECLARE n bigint;
         BEGIN
             PERFORM set_config('pmos.audit_purge', 'on', true);
             DELETE FROM {schema}.audit_events;
             GET DIAGNOSTICS n = ROW_COUNT;
             RETURN n;
         END $$"
    )))
    .execute(pool)
    .await
    .unwrap();
    let err = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {evil}.purge_audit_events(interval '1 day', 10)"
    )))
    .execute(pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");
    assert_eq!(count().await, 3, "not even the old event went");
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {evil} CASCADE")))
        .execute(pool)
        .await
        .unwrap();

    // The function with a forged session setting and a short retention: refused.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL pmos.audit_purge = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = sqlx::query("SELECT * FROM purge_audit_events(interval '1 day', 10)")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("at least 90 days"), "{err}");
    tx.rollback().await.unwrap();

    // The function clears its setting before it returns: a DELETE after it, in the same
    // transaction, is refused.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT * FROM purge_audit_events(interval '90 days', 10)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let setting: Option<String> =
        sqlx::query_scalar("SELECT current_setting('pmos.audit_purge', true)")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(setting.as_deref(), Some("off"));
    let err = sqlx::query("DELETE FROM audit_events")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");
    tx.rollback().await.unwrap();

    // The tool server can't write a `system` event of its own.
    let mut fake = event(now, PURGE_TOOL);
    fake.client = Client::System;
    fake.detail = Some("purged 999 events older than 2026-01-01T00:00:00Z".into());
    assert!(pg.store.record(fake).await.is_err());

    // Only the purge itself, which takes the old one.
    let Some(Purged::Ran(done)) = pg.store.purge(90, 100, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!(done.deleted, 1);
    let rows = sqlx::query("SELECT tool FROM audit_events ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();
    let left: Vec<String> = rows.iter().map(|r| r.get("tool")).collect();
    assert_eq!(left, ["recent.read", "new.read", PURGE_TOOL]);

    // Only the tool server's role may call it.
    let public: bool = sqlx::query_scalar(
        "SELECT has_function_privilege('public', 'purge_audit_events(interval, integer)', 'EXECUTE')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(!public, "EXECUTE is revoked from PUBLIC");
    pg.drop_schema().await;
}

/// LG (the re-check of eb24d28): a function whose quoted name is a newline and the text of the
/// real purge's call-stack frame, so PG_CONTEXT holds that text, sets the purge's setting and
/// deletes only old rows, from the audit log and from role history. Identifiers are cut at 63
/// bytes, so the name fits only beside a short schema name: role history's frame fits with a
/// schema of up to 10 characters (`public` among them), the audit log's with one character;
/// the case runs in a one-letter schema so both fit. Before migration 0023 both deletes went
/// through (checked); now both are refused: the triggers no longer read a name from the call
/// stack, they look for a mark only the purge itself writes. The real purges still work.
#[tokio::test]
async fn postgres_a_crafted_function_name_cannot_pass_as_the_purge() {
    let Some(pg) = TestPg::new_short_schema().await else {
        return;
    };
    let now = Utc::now();
    seed(&pg.store, now).await;
    let pool = pg.store.pool();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await
        .unwrap();
    let evil = format!("{schema}_lg");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {evil}")))
        .execute(pool)
        .await
        .unwrap();
    // The name: a newline, then what the real frame prints, so PG_CONTEXT holds that text.
    let audit_name =
        format!("\nPL/pgSQL function {schema}.purge_audit_events(interval,integer) line ");
    let roles_name = format!("\nPL/pgSQL function {schema}.purge_role_changes(integer) line ");
    assert!(
        audit_name.len() <= 63 && roles_name.len() <= 63,
        "the names fit"
    );
    for (name, setting, body) in [
        (
            &audit_name,
            "pmos.audit_purge",
            format!("DELETE FROM {schema}.audit_events WHERE at < now() - interval '100 days'"),
        ),
        (
            &roles_name,
            "pmos.role_changes_retention",
            format!(
                "DELETE FROM {schema}.role_changes c USING {schema}.users u
                 WHERE u.handle = c.target AND u.status = 'deactivated'
                   AND u.deactivated_at < now() - interval '90 days'"
            ),
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE FUNCTION {evil}.\"{name}\"() RETURNS bigint LANGUAGE plpgsql AS $$
             DECLARE n bigint;
             BEGIN
                 PERFORM set_config('{setting}', 'on', true);
                 {body};
                 GET DIAGNOSTICS n = ROW_COUNT;
                 RETURN n;
             END $$"
        )))
        .execute(pool)
        .await
        .unwrap();
    }
    // A user deactivated long ago, with role history the role purge may take (and the active
    // admin the user database always has).
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "INSERT INTO users (handle, status, created_at, created_by, created_client)
         VALUES ('lg-admin', 'active', now(), 'host', 'host')",
        "INSERT INTO user_roles (handle, role, granted_at, granted_by)
         VALUES ('lg-admin', 'admin', now(), 'host')",
        "INSERT INTO users (handle, status, created_at, created_by, created_client,
                            deactivated_at, deactivated_by)
         VALUES ('lg-gone', 'deactivated', now() - interval '400 days', 'host', 'host',
                 now() - interval '200 days', 'host')",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    sqlx::query(
        "INSERT INTO role_changes (at, actor, client, host_user, target, change)
         VALUES (now() - interval '300 days', 'host', 'host', 'opsuser', 'lg-gone', 'set_field')",
    )
    .execute(pool)
    .await
    .unwrap();
    let count = |sql: &'static str| async move {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    };
    let events = count("SELECT count(*) FROM audit_events").await;
    let history = count("SELECT count(*) FROM role_changes WHERE target = 'lg-gone'").await;
    for name in [&audit_name, &roles_name] {
        let err = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {evil}.\"{name}\"()")))
            .execute(pool)
            .await
            .expect_err("a crafted name passed as the purge");
        assert!(err.to_string().contains("append-only"), "{err}");
    }
    assert_eq!(count("SELECT count(*) FROM audit_events").await, events);
    assert_eq!(
        count("SELECT count(*) FROM role_changes WHERE target = 'lg-gone'").await,
        history
    );
    // Nobody but the purge's owner can write the mark (nothing on it is granted to PUBLIC).
    let public: bool =
        sqlx::query_scalar("SELECT has_table_privilege('public', 'purge_marks', 'INSERT')")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(!public);
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {evil} CASCADE")))
        .execute(pool)
        .await
        .unwrap();
    // The real purges still take what they may, and leave no mark behind.
    let Some(Purged::Ran(done)) = pg.store.purge(90, 100, now).await.unwrap() else {
        panic!("the purge ran");
    };
    assert_eq!(done.deleted, 1);
    let (deleted, _): (i64, bool) =
        sqlx::query_as("SELECT deleted, more FROM purge_role_changes(100)")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(deleted >= 1, "{deleted}");
    assert_eq!(count("SELECT count(*) FROM purge_marks").await, 0);
    pg.drop_schema().await;
}

/// Two purges at once (two replicas): between them every old event goes exactly once, none
/// fails, none waits on the other, and nothing new goes.
#[tokio::test]
async fn postgres_concurrent_purges_dont_double_delete_or_deadlock() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let now = Utc::now();
    for i in 0..60 {
        pg.store
            .record(event(now - Duration::days(100 + i), "old.read"))
            .await
            .unwrap();
    }
    for _ in 0..5 {
        pg.store.record(event(now, "new.read")).await.unwrap();
    }
    let a = pg.store.clone();
    let b = pg.store.clone();
    let (ra, rb) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(
            tokio::spawn(async move { retention::run(&a, 90, 5, now).await }),
            tokio::spawn(async move { retention::run(&b, 90, 5, now).await }),
        )
    })
    .await
    .expect("no deadlock");
    let (ra, rb) = (ra.unwrap().unwrap().unwrap(), rb.unwrap().unwrap().unwrap());
    // A run that found the lock held stopped; finish whatever is left.
    let rest = retention::run(&pg.store, 90, 5, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ra.deleted + rb.deleted + rest.deleted,
        60,
        "{ra:?} {rb:?} {rest:?}"
    );
    let pool = pg.store.pool();
    let old: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE tool = 'old.read'")
        .fetch_one(pool)
        .await
        .unwrap();
    let new: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE tool = 'new.read'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!((old, new), (0, 5));
    // Every batch that ran is recorded, and their counts add up.
    let details: Vec<String> =
        sqlx::query_scalar("SELECT detail FROM audit_events WHERE tool = $1")
            .bind(PURGE_TOOL)
            .fetch_all(pool)
            .await
            .unwrap();
    let recorded: u64 = details
        .iter()
        .map(|d| parse_purge_detail(d).unwrap().0)
        .sum();
    assert_eq!(recorded, 60);
    assert_eq!(details.len(), ra.batches + rb.batches + rest.batches);

    // Held by another transaction: a purge doesn't wait, it says so.
    let mut tx = pool.begin().await.unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pmos.audit_purge'), hashtext($1))")
        .bind(&schema)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        pg.store.purge(90, 5, now).await.unwrap(),
        Some(Purged::Busy)
    );
    tx.rollback().await.unwrap();
    pg.drop_schema().await;
}
