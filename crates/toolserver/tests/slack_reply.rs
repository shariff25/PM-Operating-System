//! The Slack reply assistant's tool server rules (`docs/slack-reply-spec.md` §3), through
//! `ToolService` on both stores, and the decision answers over HTTP: one draft per thread,
//! what a draft was built from, the audience check, and the 24-hour expiry.
//!
//! The Slack fixtures are the fake connector's (`connectors/fake.rs`): #pm-team (`C0PMTEAM`,
//! all team), #field-questions (`C0FIELD`, one outsider), #globex-shared (`C0GLOBEXCX`, Slack
//! Connect). Postgres cases need `PMOS_TEST_DATABASE_URL`, as in `tests/stores.rs`.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::{now, Fixture};
use pmos_contract::Sensitivity;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{
    args_hash, ApprovalItem, DecideError, Decision, NewItem, Status, Verdict, EXPIRED_TEXT,
};
use pmos_toolserver::audit::{AuditEvent, Client, Outcome};
use pmos_toolserver::connectors::fake::{
    SLACK_ALL_TEAM, SLACK_CONNECT, SLACK_PM, SLACK_WITH_OUTSIDER,
};
use pmos_toolserver::ledger::{LedgerOutcome, LedgerQuery};
use pmos_toolserver::service::{AudienceSummary, DecisionError, Swept};
use pmos_toolserver::session::Session;
use pmos_toolserver::{router, AppConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Generate a `memory` and a `postgres` test for each case.
macro_rules! both {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $case() {
                    let (f, _) = crate::common::reply_fixture(false).await.unwrap();
                    super::$case(&f).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $case() {
                    if let Some((f, pg)) = crate::common::reply_fixture(true).await {
                        super::$case(&f).await;
                        drop(f);
                        if let Some(pg) = pg {
                            pg.drop_schema().await;
                        }
                    }
                }
            )*
        }
    };
}

both!(
    one_draft_per_thread,
    duplicate_drafts_race_to_one,
    sources_sensitivity_is_recorded_from_reads,
    restricted_drafts_never_reach_outsiders,
    external_channels_need_a_second_confirmation,
    failed_channel_calls_fail_closed,
    undecided_drafts_expire_and_lose_their_text,
    decided_drafts_lose_their_text_a_day_later,
    an_overdue_draft_expires_when_decided,
    a_tagged_reply_is_one_draft_per_message,
    a_tagged_message_id_is_kept_and_never_sent,
    the_pending_list_is_oldest_first_and_capped,
    a_restricted_note_keeps_later_drafts_from_outsiders,
    a_follow_up_starts_at_its_historys_level,
);

/// A follow-up in the same conversation replays earlier answers into a fresh session (W6).
/// The runtime reads the level the first task's sessions reached, and starts the follow-up's
/// session at it: a draft that reads nothing new is still checked as restricted.
async fn a_follow_up_starts_at_its_historys_level(f: &Fixture) {
    // Task 1 reads a restricted source.
    let s1 = session(f, "tsk_hist_1").await;
    f.svc
        .call(
            &s1,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.svc.run_sensitivity("tsk_hist_1").await.unwrap(),
        Some(Sensitivity::Restricted)
    );
    assert_eq!(f.svc.run_sensitivity("tsk_never").await.unwrap(), None);
    // Task 2 starts at that level and drafts, reading nothing itself, to a channel with an
    // outsider: blocked.
    let s2 = f
        .svc
        .create_session_from(
            SLACK_PM,
            "reply-test",
            "tsk_hist_2",
            None,
            Sensitivity::Restricted,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(s2.sources_sensitivity, Sensitivity::Restricted);
    let res = draft_at(
        f,
        &s2,
        reply(
            SLACK_WITH_OUTSIDER,
            "1731601200.000600",
            "As I said above: ...",
        ),
        now(),
    )
    .await;
    let id = res["approval_id"].as_str().unwrap().to_string();
    assert_eq!(
        item(f, &id).await.sources_sensitivity,
        Some(Sensitivity::Restricted)
    );
    assert!(matches!(
        decide(f, &id, Decision::Approve, None, true, now()).await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    assert!(f.slack.writes().is_empty());
    // A public read later never lowers it.
    f.svc
        .call(&s2, "context_search", json!({"query": "pricing"}), now())
        .await
        .unwrap();
    assert_eq!(
        f.svc.run_sensitivity("tsk_hist_2").await.unwrap(),
        Some(Sensitivity::Restricted)
    );
    // Without a floor, the same draft is not blocked for its history (control).
    let s3 = session(f, "tsk_hist_3").await;
    let res = draft_at(
        f,
        &s3,
        reply(SLACK_WITH_OUTSIDER, "1731601300.000700", "Fresh answer."),
        now(),
    )
    .await;
    let id = res["approval_id"].as_str().unwrap().to_string();
    let done = decide(f, &id, Decision::Approve, None, false, now())
        .await
        .unwrap();
    assert_eq!(done.status, Status::Approved);
}

async fn session(f: &Fixture, run: &str) -> Session {
    f.svc
        .create_session(SLACK_PM, "reply-test", run, None, now())
        .await
        .unwrap()
}

fn reply(channel: &str, thread_ts: &str, text: &str) -> Value {
    json!({"channel": channel, "thread_ts": thread_ts, "text": text})
}

/// Draft a reply in `s`; returns the tool's answer.
async fn draft_at(f: &Fixture, s: &Session, args: Value, at: DateTime<Utc>) -> Value {
    f.svc.call(s, "slack_post_reply", args, at).await.unwrap()
}

/// Draft a reply in a fresh session that read only `read` (a tool and its arguments), and
/// return the approval id.
async fn draft_after(f: &Fixture, run: &str, read: Option<(&str, Value)>, args: Value) -> String {
    let s = session(f, run).await;
    if let Some((tool, read_args)) = read {
        f.svc.call(&s, tool, read_args, now()).await.unwrap();
    }
    let res = draft_at(f, &s, args, now()).await;
    assert_eq!(res["status"], "pending_approval", "{res}");
    res["approval_id"].as_str().unwrap().to_string()
}

/// A draft read from a restricted context file.
async fn restricted_draft(f: &Fixture, run: &str, args: Value) -> String {
    draft_after(
        f,
        run,
        Some((
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
        )),
        args,
    )
    .await
}

/// A draft read from Jira: internal.
async fn internal_draft(f: &Fixture, run: &str, args: Value) -> String {
    draft_after(f, run, Some(("jira_read", json!({"key": "PM-101"}))), args).await
}

async fn item(f: &Fixture, id: &str) -> ApprovalItem {
    f.svc.approval(id).await.unwrap().unwrap()
}

async fn decide(
    f: &Fixture,
    id: &str,
    decision: Decision,
    args: Option<Value>,
    confirm_external: bool,
    at: DateTime<Utc>,
) -> Result<ApprovalItem, DecisionError> {
    let verdict = Verdict {
        confirm_external,
        ..Verdict::new(decision, args)
    };
    f.svc
        .decide_verdict(Client::Bot, id, SLACK_PM, verdict, at)
        .await
}

fn last_event(f: &Fixture) -> AuditEvent {
    f.audit.events().last().cloned().unwrap()
}

fn events_for(f: &Fixture, id: &str) -> Vec<AuditEvent> {
    f.audit
        .events()
        .into_iter()
        .filter(|e| e.output_ref.as_deref() == Some(id))
        .collect()
}

// ----- one draft per thread -----------------------------------------------------------------

async fn one_draft_per_thread(f: &Fixture) {
    let s = session(f, "tsk_dup").await;
    let thread = reply(SLACK_ALL_TEAM, "1731600000.000100", "Yes: SAML 2.0.");
    let first = draft_at(f, &s, thread.clone(), now()).await;
    assert_eq!(first["status"], "pending_approval");
    let id = first["approval_id"].as_str().unwrap().to_string();

    // A second draft for the thread, even with other words, from another run: the first
    // stands, nothing new is queued.
    let other = session(f, "tsk_dup2").await;
    let again = draft_at(
        f,
        &other,
        reply(SLACK_ALL_TEAM, "1731600000.000100", "Different words."),
        now() + Duration::minutes(5),
    )
    .await;
    assert_eq!(
        again,
        json!({
            "status": "already_drafted",
            "approval_id": id,
            "message": "A reply to this thread is already waiting for evals (or was posted); don't draft another.",
        })
    );
    let e = last_event(f);
    assert_eq!(
        (e.tool.as_str(), e.outcome, e.detail.as_deref()),
        ("slack.post_reply", Outcome::Ok, Some("already drafted"))
    );
    assert_eq!(e.output_ref.as_deref(), Some(id.as_str()));
    assert_eq!(f.svc.approvals(SLACK_PM, None).await.unwrap().len(), 1);

    // Another thread, or a message that isn't a reply, is drafted as usual.
    let elsewhere = draft_at(
        f,
        &s,
        reply(SLACK_ALL_TEAM, "1731600300.000200", "Not yet."),
        now(),
    )
    .await;
    assert_eq!(elsewhere["status"], "pending_approval");

    // Approved (posted) still blocks a new draft.
    decide(f, &id, Decision::Approve, None, false, now())
        .await
        .unwrap();
    assert_eq!(item(f, &id).await.status, Status::Approved);
    let after_post = draft_at(f, &s, thread.clone(), now() + Duration::hours(1)).await;
    assert_eq!(after_post["status"], "already_drafted");
    assert_eq!(after_post["approval_id"], json!(id));
    // ...for 7 days.
    let next_week = draft_at(
        f,
        &s,
        thread.clone(),
        now() + Duration::days(7) + Duration::minutes(1),
    )
    .await;
    assert_eq!(next_week["status"], "pending_approval", "{next_week}");

    // A rejected draft doesn't block a new one.
    let other_thread = reply(SLACK_ALL_TEAM, "1731600600.000400", "Thanks!");
    let rejected = draft_at(f, &s, other_thread.clone(), now()).await;
    let rejected_id = rejected["approval_id"].as_str().unwrap();
    decide(f, rejected_id, Decision::Reject, None, false, now())
        .await
        .unwrap();
    let redraft = draft_at(f, &s, other_thread, now()).await;
    assert_eq!(redraft["status"], "pending_approval");
    assert_ne!(redraft["approval_id"], json!(rejected_id));

    // Someone else's draft for the same thread is theirs: it doesn't block this PM.
    let ben = f
        .svc
        .create_session("ben", "reply-test", "tsk_ben", None, now())
        .await
        .unwrap();
    let bens = draft_at(
        f,
        &ben,
        reply(SLACK_ALL_TEAM, "1731600300.000200", "I'll check."),
        now(),
    )
    .await;
    assert_eq!(bens["status"], "pending_approval");
}

async fn duplicate_drafts_race_to_one(f: &Fixture) {
    let s = session(f, "tsk_race").await;
    let mut handles = Vec::new();
    for i in 0..8 {
        let svc = Arc::clone(&f.svc);
        let s = s.clone();
        handles.push(tokio::spawn(async move {
            svc.call(
                &s,
                "slack_post_reply",
                reply(SLACK_ALL_TEAM, "1731600000.000100", &format!("draft {i}")),
                now(),
            )
            .await
            .unwrap()
        }));
    }
    let mut created = 0;
    for h in handles {
        if h.await.unwrap()["status"] == "pending_approval" {
            created += 1;
        }
    }
    assert_eq!(created, 1, "exactly one concurrent draft per thread");
    assert_eq!(f.svc.approvals(SLACK_PM, None).await.unwrap().len(), 1);
}

// ----- what a draft was built from ----------------------------------------------------------

/// A case name, what the session reads first (a tool and its arguments), and the data class
/// its draft should record.
type ReadCase = (&'static str, Option<(&'static str, Value)>, Sensitivity);

async fn sources_sensitivity_is_recorded_from_reads(f: &Fixture) {
    let args = |ts: &str| reply(SLACK_ALL_TEAM, ts, "Answer.");
    let cases: Vec<ReadCase> = vec![
        ("nothing", None, Sensitivity::Public),
        (
            "docs",
            Some(("docs_search", json!({"query": "SAML SSO"}))),
            Sensitivity::Public,
        ),
        (
            "jira",
            Some(("jira_read", json!({"key": "PM-101"}))),
            Sensitivity::Internal,
        ),
        (
            "slack",
            Some(("slack_search", json!({"query": "<@U0EVALS>"}))),
            Sensitivity::Internal,
        ),
        (
            "context-internal",
            Some((
                "context_read",
                json!({"path": "context/products/key-management.md"}),
            )),
            Sensitivity::Internal,
        ),
        (
            "context-restricted",
            Some((
                "context_read",
                json!({"path": "context/partners/globex-nda.md"}),
            )),
            Sensitivity::Restricted,
        ),
        (
            "context-search",
            Some(("context_search", json!({"query": "NDA roadmap"}))),
            Sensitivity::Restricted,
        ),
    ];
    for (i, (name, read, want)) in cases.into_iter().enumerate() {
        let id = draft_after(
            f,
            &format!("tsk_{name}"),
            read,
            args(&format!("17316{i:05}.000100")),
        )
        .await;
        assert_eq!(item(f, &id).await.sources_sensitivity, Some(want), "{name}");
    }

    // The highest so far, and it never goes down: restricted, then public docs.
    let s = session(f, "tsk_mixed").await;
    for (tool, read) in [
        (
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
        ),
        ("docs_search", json!({"query": "rotation"})),
    ] {
        f.svc.call(&s, tool, read, now()).await.unwrap();
    }
    let reloaded = f.svc.session(&s.token, now()).await.unwrap().unwrap();
    assert_eq!(reloaded.sources_sensitivity, Sensitivity::Restricted);
    let res = draft_at(f, &s, args("1731699999.000100"), now()).await;
    let id = res["approval_id"].as_str().unwrap();
    assert_eq!(
        item(f, id).await.sources_sensitivity,
        Some(Sensitivity::Restricted)
    );
    // The item says so in its JSON (the Slack card and the console read it).
    let json = serde_json::to_value(item(f, id).await).unwrap();
    assert_eq!(json["sources_sensitivity"], "restricted");

    // A search that finds nothing reads nothing.
    let empty = session(f, "tsk_empty").await;
    f.svc
        .call(&empty, "jira_search", json!({"jql": "zzzz-nothing"}), now())
        .await
        .unwrap();
    let res = draft_at(f, &empty, args("1731688888.000100"), now()).await;
    let id = res["approval_id"].as_str().unwrap();
    assert_eq!(
        item(f, id).await.sources_sensitivity,
        Some(Sensitivity::Public)
    );
}

// ----- the audience check -------------------------------------------------------------------

async fn restricted_drafts_never_reach_outsiders(f: &Fixture) {
    // Restricted, in a channel with one outsider (U0SE): blocked, still pending, not posted.
    let id = restricted_draft(
        f,
        "tsk_r1",
        reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "Under NDA: ..."),
    )
    .await;
    let err = decide(f, &id, Decision::Approve, None, false, now())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        DecisionError::AudienceBlocked(AudienceSummary {
            members: 3,
            outsiders: 1,
            external: false
        })
    );
    assert!(err
        .to_string()
        .contains("1 person in this channel isn't on the product team"));
    assert_eq!(item(f, &id).await.status, Status::Pending);
    assert!(f.slack.writes().is_empty());
    let e = last_event(f);
    assert_eq!(
        (e.tool.as_str(), e.outcome, e.client),
        ("slack.post_reply", Outcome::Denied, Client::Bot)
    );
    assert!(e.detail.unwrap().starts_with("audience_blocked"));
    // Confirming doesn't help, and neither does an edit: the sources are what they were.
    assert!(matches!(
        decide(f, &id, Decision::Approve, None, true, now()).await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    assert!(matches!(
        decide(
            f,
            &id,
            Decision::Edit,
            Some(reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "Shorter.")),
            false,
            now()
        )
        .await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    // Rejecting is never checked.
    let rejected = decide(f, &id, Decision::Reject, None, false, now())
        .await
        .unwrap();
    assert_eq!(rejected.status, Status::Rejected);

    // Restricted, in an all-team channel: posted.
    let ok = restricted_draft(
        f,
        "tsk_r2",
        reply(SLACK_ALL_TEAM, "1731600000.000100", "Yes."),
    )
    .await;
    let posted = decide(f, &ok, Decision::Approve, None, false, now())
        .await
        .unwrap();
    assert_eq!(posted.status, Status::Approved);
    assert_eq!(f.slack.writes().len(), 1);
    assert_eq!(f.slack.writes()[0].user, SLACK_PM, "as the PM");

    // An edit from Slack can't move the reply to another channel or thread at all (S2).
    let moved = restricted_draft(
        f,
        "tsk_r3",
        reply(SLACK_ALL_TEAM, "1731600300.000200", "Yes."),
    )
    .await;
    for (field, edited) in [
        (
            "channel",
            reply(SLACK_WITH_OUTSIDER, "1731600300.000200", "Yes."),
        ),
        (
            "thread_ts",
            reply(SLACK_ALL_TEAM, "1731601200.000600", "Yes."),
        ),
        (
            "message_ts",
            tagged(
                SLACK_ALL_TEAM,
                "1731600300.000200",
                "1731600300.000300",
                "Yes.",
            ),
        ),
    ] {
        assert_eq!(
            decide(f, &moved, Decision::Edit, Some(edited), false, now()).await,
            Err(DecisionError::TargetChanged(field.into())),
            "{field}"
        );
    }
    assert_eq!(item(f, &moved).await.status, Status::Pending);
    // The orchestrator may move it, and the audience check runs on the new channel.
    assert!(matches!(
        f.svc
            .decide_verdict(
                Client::Orchestrator,
                &moved,
                SLACK_PM,
                Verdict::new(
                    Decision::Edit,
                    Some(reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "Yes."))
                ),
                now()
            )
            .await,
        Err(DecisionError::AudienceBlocked(_))
    ));

    // Restricted, in a Slack Connect channel: blocked as external.
    let ext = restricted_draft(
        f,
        "tsk_r4",
        reply(SLACK_CONNECT, "1731601500.000700", "Yes."),
    )
    .await;
    assert_eq!(
        decide(f, &ext, Decision::Approve, None, true, now())
            .await
            .unwrap_err(),
        DecisionError::AudienceBlocked(AudienceSummary {
            members: 3,
            outsiders: 1,
            external: true
        })
    );

    // Internal sources may reach a channel with an outsider.
    let internal = internal_draft(
        f,
        "tsk_r5",
        reply(
            SLACK_WITH_OUTSIDER,
            "1731601200.000600",
            "PM-101 tracks it.",
        ),
    )
    .await;
    assert_eq!(
        decide(f, &internal, Decision::Approve, None, false, now())
            .await
            .unwrap()
            .status,
        Status::Approved
    );

    // `slack.send_message` is checked the same way.
    let s = session(f, "tsk_r6").await;
    f.svc
        .call(
            &s,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    let sent = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": SLACK_WITH_OUTSIDER, "text": "NDA details"}),
            now(),
        )
        .await
        .unwrap();
    let sent = sent["approval_id"].as_str().unwrap();
    assert!(matches!(
        decide(f, sent, Decision::Approve, None, false, now()).await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    assert_eq!(f.slack.writes().len(), 2, "only the two allowed posts");
}

/// Once a PM's memory holds a restricted note, a reply drafted in a session that read it is
/// blocked where people outside the team can see it (S6), on each memory backend.
async fn a_restricted_note_keeps_later_drafts_from_outsiders(f: &Fixture) {
    // A session reads restricted context, then writes a note: stored restricted.
    let writer = session(f, "tsk_m1").await;
    f.svc
        .call(
            &writer,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    let written = f
        .svc
        .call(
            &writer,
            "memory_update",
            json!({"key": "partners", "text": "Globex: joint key-management roadmap"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(written["note"]["sensitivity"], "restricted", "{written}");

    // A later session reads memory, and nothing else: raised to restricted.
    let reader = session(f, "tsk_m2").await;
    let read = f
        .svc
        .call(&reader, "memory_read", json!({}), now())
        .await
        .unwrap();
    assert_eq!(read["notes"][0]["sensitivity"], "restricted");
    let res = draft_at(
        f,
        &reader,
        reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "On partners: ..."),
        now(),
    )
    .await;
    let id = res["approval_id"].as_str().unwrap().to_string();
    assert_eq!(
        item(f, &id).await.sources_sensitivity,
        Some(Sensitivity::Restricted)
    );
    assert!(matches!(
        decide(f, &id, Decision::Approve, None, false, now()).await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    assert!(f.slack.writes().is_empty());

    // A note written by a session that read only internal data reads as internal, and its
    // reader may still reach the outsider's channel.
    let other = session(f, "tsk_m3").await;
    f.svc
        .call(&other, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .unwrap();
    f.svc
        .call(
            &other,
            "memory_update",
            json!({"key": "partners", "text": "Tracked in PM-101"}),
            now(),
        )
        .await
        .unwrap();
    let fresh = session(f, "tsk_m4").await;
    f.svc
        .call(&fresh, "memory_read", json!({}), now())
        .await
        .unwrap();
    let res = draft_at(
        f,
        &fresh,
        reply(
            SLACK_WITH_OUTSIDER,
            "1731601300.000900",
            "PM-101 tracks it.",
        ),
        now(),
    )
    .await;
    assert_eq!(res["status"], "pending_approval", "{res}");
    let ok = res["approval_id"].as_str().unwrap().to_string();
    assert_eq!(
        item(f, &ok).await.sources_sensitivity,
        Some(Sensitivity::Internal)
    );
    assert_eq!(
        decide(f, &ok, Decision::Approve, None, false, now())
            .await
            .unwrap()
            .status,
        Status::Approved
    );
}

async fn external_channels_need_a_second_confirmation(f: &Fixture) {
    let id = internal_draft(
        f,
        "tsk_x1",
        reply(
            SLACK_CONNECT,
            "1731601500.000700",
            "Yes, a new key version.",
        ),
    )
    .await;
    let err = decide(f, &id, Decision::Approve, None, false, now())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        DecisionError::NeedsExternalConfirm(AudienceSummary {
            members: 3,
            outsiders: 1,
            external: true
        })
    );
    assert_eq!(item(f, &id).await.status, Status::Pending);
    assert!(f.slack.writes().is_empty());
    let e = last_event(f);
    assert_eq!(
        (e.outcome, e.detail.as_deref()),
        (Outcome::Denied, Some("needs_external_confirm"))
    );
    // Confirmed: posted.
    let posted = decide(f, &id, Decision::Approve, None, true, now())
        .await
        .unwrap();
    assert_eq!(posted.status, Status::Approved);
    assert_eq!(f.slack.writes().len(), 1);
    // An edit needs it too.
    let s = session(f, "tsk_x2").await;
    let sent = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": SLACK_CONNECT, "text": "Hello Globex"}),
            now(),
        )
        .await
        .unwrap();
    let sent = sent["approval_id"].as_str().unwrap();
    let edit = json!({"channel": SLACK_CONNECT, "text": "Hello, Globex team"});
    assert!(matches!(
        decide(f, sent, Decision::Edit, Some(edit.clone()), false, now()).await,
        Err(DecisionError::NeedsExternalConfirm(_))
    ));
    assert_eq!(
        decide(f, sent, Decision::Edit, Some(edit), true, now())
            .await
            .unwrap()
            .status,
        Status::Approved
    );
}

async fn failed_channel_calls_fail_closed(f: &Fixture) {
    // The fake has no audience for this channel, as when Slack's calls fail.
    let id = internal_draft(
        f,
        "tsk_f1",
        reply("C0NOWHERE", "1731600000.000100", "Hello"),
    )
    .await;
    let err = decide(f, &id, Decision::Approve, None, true, now())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DecisionError::AudienceUnavailable(_)),
        "{err:?}"
    );
    assert_eq!(item(f, &id).await.status, Status::Pending);
    assert!(f.slack.writes().is_empty());
    let e = last_event(f);
    assert_eq!(e.outcome, Outcome::Error);
    assert!(e.detail.unwrap().starts_with("audience_unavailable"));
}

// ----- the 24-hour expiry -------------------------------------------------------------------

fn hours(n: i64) -> DateTime<Utc> {
    now() + Duration::hours(n)
}

async fn undecided_drafts_expire_and_lose_their_text(f: &Fixture) {
    let args = reply(
        SLACK_ALL_TEAM,
        "1731600000.000100",
        "Yes, SAML 2.0 is supported.",
    );
    let id = internal_draft(f, "tsk_e1", args.clone()).await;
    let s = session(f, "tsk_e2").await;
    let sent = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": SLACK_ALL_TEAM, "text": "Standup moved"}),
            now(),
        )
        .await
        .unwrap();
    let sent = sent["approval_id"].as_str().unwrap().to_string();
    // Other tools never expire this way.
    let jira = f
        .svc
        .call(
            &s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "SCIM"}),
            now(),
        )
        .await
        .unwrap();
    let jira = jira["approval_id"].as_str().unwrap().to_string();

    assert_eq!(
        f.svc.sweep_slack_drafts(hours(23)).await.unwrap(),
        Swept::default()
    );
    assert_eq!(item(f, &id).await.status, Status::Pending);

    let swept = f.svc.sweep_slack_drafts(hours(24)).await.unwrap();
    assert_eq!(
        swept,
        Swept {
            expired: 2,
            redacted: 0
        }
    );
    let expired = item(f, &id).await;
    assert_eq!(expired.status, Status::Expired);
    assert_eq!(
        expired.args,
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": EXPIRED_TEXT})
    );
    assert_eq!(expired.args_hash, args_hash(&args), "the hash is kept");
    assert_eq!(expired.redacted_at, Some(hours(24)));
    assert!(expired.decided_at.is_none());
    assert_eq!(item(f, &sent).await.args["text"], EXPIRED_TEXT);
    assert_eq!(item(f, &jira).await.status, Status::Pending);
    assert_eq!(
        f.svc.sweep_slack_drafts(hours(25)).await.unwrap(),
        Swept::default()
    );

    // Audited, with no text.
    let audited: Vec<_> = events_for(f, &id)
        .into_iter()
        .filter(|e| e.outcome == Outcome::Expired)
        .collect();
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].tool, "slack.post_reply");
    assert!(!format!("{:?}", audited[0]).contains("SAML"));

    // The ledger stores `expired`, whatever its own expiry.
    let rows = f
        .stores
        .approvals
        .ledger(&LedgerQuery::all(now() - Duration::days(365)))
        .await
        .unwrap();
    let outcome = |id: &str| rows.iter().find(|r| r.id == id).unwrap().outcome;
    assert_eq!(outcome(&id), LedgerOutcome::Expired);
    assert_eq!(outcome(&sent), LedgerOutcome::Expired);
    assert_eq!(outcome(&jira), LedgerOutcome::Pending);
    let summary = f.svc.ledger_summary(None, None, hours(25)).await.unwrap();
    let group = summary
        .groups
        .iter()
        .find(|g| g.tool == "slack.post_reply")
        .unwrap();
    assert_eq!(group.counts["expired"], 1);

    // Deciding it now: 409 expired, nothing posted.
    for decision in [Decision::Approve, Decision::Reject] {
        assert_eq!(
            decide(f, &id, decision, None, false, hours(25))
                .await
                .unwrap_err(),
            DecisionError::Decide(DecideError::Expired)
        );
    }
    assert!(f.slack.writes().is_empty());

    // An expired draft doesn't block a new one for the thread.
    let again = draft_at(f, &s, args, hours(25)).await;
    assert_eq!(again["status"], "pending_approval");
}

async fn decided_drafts_lose_their_text_a_day_later(f: &Fixture) {
    let posted = internal_draft(
        f,
        "tsk_d1",
        reply(SLACK_ALL_TEAM, "1731600000.000100", "Posted text."),
    )
    .await;
    decide(f, &posted, Decision::Approve, None, false, hours(2))
        .await
        .unwrap();
    let rejected = internal_draft(
        f,
        "tsk_d2",
        reply(SLACK_ALL_TEAM, "1731600300.000200", "Rejected text."),
    )
    .await;
    decide(f, &rejected, Decision::Reject, None, false, hours(3))
        .await
        .unwrap();

    // 24 hours after creation, but not after the decisions: kept.
    assert_eq!(
        f.svc.sweep_slack_drafts(hours(25)).await.unwrap(),
        Swept::default()
    );
    assert_eq!(item(f, &posted).await.args["text"], "Posted text.");

    assert_eq!(
        f.svc.sweep_slack_drafts(hours(26)).await.unwrap(),
        Swept {
            expired: 0,
            redacted: 1
        }
    );
    let gone = item(f, &posted).await;
    assert_eq!(gone.status, Status::Approved);
    assert_eq!(gone.args["text"], EXPIRED_TEXT);
    assert_eq!(gone.args["thread_ts"], "1731600000.000100");
    assert_eq!(gone.redacted_at, Some(hours(26)));
    assert_eq!(
        gone.args_hash,
        args_hash(&reply(SLACK_ALL_TEAM, "1731600000.000100", "Posted text."))
    );
    assert!(gone.result.is_some(), "the posted copy's link stays");
    assert_eq!(item(f, &rejected).await.args["text"], "Rejected text.");

    assert_eq!(
        f.svc.sweep_slack_drafts(hours(27)).await.unwrap(),
        Swept {
            expired: 0,
            redacted: 1
        }
    );
    assert_eq!(item(f, &rejected).await.args["text"], EXPIRED_TEXT);
    assert_eq!(
        f.svc.sweep_slack_drafts(hours(48)).await.unwrap(),
        Swept::default(),
        "once"
    );
    // The ledger keeps the decisions.
    let rows = f
        .svc
        .ledger(LedgerQuery::all(now()), hours(48))
        .await
        .unwrap();
    let outcome = |id: &str| rows.iter().find(|r| r.id == id).unwrap().outcome;
    assert_eq!(outcome(&posted), LedgerOutcome::Approved);
    assert_eq!(outcome(&rejected), LedgerOutcome::Rejected);
}

async fn an_overdue_draft_expires_when_decided(f: &Fixture) {
    // The sweeper hasn't run: deciding finds it past its time and expires it.
    let id = internal_draft(
        f,
        "tsk_o1",
        reply(SLACK_ALL_TEAM, "1731600000.000100", "Late."),
    )
    .await;
    assert_eq!(
        decide(f, &id, Decision::Approve, None, false, hours(24))
            .await
            .unwrap_err(),
        DecisionError::Decide(DecideError::Expired)
    );
    let expired = item(f, &id).await;
    assert_eq!(expired.status, Status::Expired);
    assert_eq!(expired.args["text"], EXPIRED_TEXT);
    assert!(f.slack.writes().is_empty());
    // Just before its time it is decided as usual.
    let fresh = internal_draft(
        f,
        "tsk_o2",
        reply(SLACK_ALL_TEAM, "1731600300.000200", "On time."),
    )
    .await;
    assert_eq!(
        decide(
            f,
            &fresh,
            Decision::Approve,
            None,
            false,
            hours(24) - Duration::seconds(1)
        )
        .await
        .unwrap()
        .status,
        Status::Approved
    );
}

// ----- the store, directly ------------------------------------------------------------------

/// An item made before `sources_sensitivity` was recorded (null in Postgres) counts as
/// restricted.
#[tokio::test]
async fn unrecorded_sources_count_as_restricted() {
    let Some((f, Some(pg))) = common::reply_fixture(true).await else {
        return;
    };
    let args = reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "Hello");
    let new = |run| NewItem {
        user: SLACK_PM,
        skill: Some("reply-test"),
        bot: None,
        run_id: run,
        tool: "slack.post_reply",
        args: &args,
        content_sha: None,
        model_alias: None,
        sources_sensitivity: Sensitivity::Public,
    };
    let public = f
        .stores
        .approvals
        .create(new("tsk_new"), now())
        .await
        .unwrap();
    let legacy = f
        .stores
        .approvals
        .create(new("tsk_old"), now())
        .await
        .unwrap();
    sqlx::query("UPDATE approvals SET sources_sensitivity = NULL WHERE id = $1")
        .bind(&legacy.id)
        .execute(pg.store.pool())
        .await
        .unwrap();
    let legacy = item(&f, &legacy.id).await;
    assert_eq!(legacy.sources_sensitivity, None);
    assert!(serde_json::to_value(&legacy)
        .unwrap()
        .get("sources_sensitivity")
        .is_none());
    assert!(matches!(
        decide(&f, &legacy.id, Decision::Approve, None, false, now()).await,
        Err(DecisionError::AudienceBlocked(_))
    ));
    // Public sources may reach the outsider.
    assert_eq!(
        decide(&f, &public.id, Decision::Approve, None, false, now())
            .await
            .unwrap()
            .status,
        Status::Approved
    );
    drop(f);
    pg.drop_schema().await;
}

// ----- tagging: one draft per message (docs/slack-mentions-and-home-spec.md §3) -----------

fn tagged(channel: &str, thread_ts: &str, message_ts: &str, text: &str) -> Value {
    json!({"channel": channel, "thread_ts": thread_ts, "message_ts": message_ts, "text": text})
}

async fn a_tagged_reply_is_one_draft_per_message(f: &Fixture) {
    let thread = "1731600000.000100";
    let s = session(f, "tsk_tag1").await;
    let first = draft_at(
        f,
        &s,
        tagged(
            SLACK_ALL_TEAM,
            thread,
            "1731600300.000200",
            "Yes: SAML 2.0.",
        ),
        now(),
    )
    .await;
    assert_eq!(first["status"], "pending_approval", "{first}");
    let id = first["approval_id"].as_str().unwrap().to_string();

    // The same tagged message again, from another run: the first draft stands.
    let other = session(f, "tsk_tag2").await;
    let again = draft_at(
        f,
        &other,
        tagged(SLACK_ALL_TEAM, thread, "1731600300.000200", "Other words."),
        now() + Duration::minutes(5),
    )
    .await;
    assert_eq!(
        again,
        json!({
            "status": "already_drafted",
            "approval_id": id,
            "message": "A reply to this message is already waiting for evals (or was posted); don't draft another.",
        })
    );

    // A second tag later in the same thread is a new draft.
    let second = draft_at(
        f,
        &s,
        tagged(
            SLACK_ALL_TEAM,
            thread,
            "1731600600.000400",
            "And SCIM in 4.3.",
        ),
        now(),
    )
    .await;
    assert_eq!(second["status"], "pending_approval", "{second}");
    assert_ne!(second["approval_id"], json!(id));

    // Without `message_ts` the rule is per thread, as before: the tagged drafts count.
    let untagged = draft_at(f, &s, reply(SLACK_ALL_TEAM, thread, "Plain reply."), now()).await;
    assert_eq!(untagged["status"], "already_drafted");
    assert_eq!(
        untagged["message"],
        "A reply to this thread is already waiting for evals (or was posted); don't draft another."
    );
    // ...and a reply-assistant draft doesn't stop a tag in its thread.
    let plain_thread = "1731601500.000700";
    let plain = draft_at(f, &s, reply(SLACK_ALL_TEAM, plain_thread, "Plain."), now()).await;
    assert_eq!(plain["status"], "pending_approval");
    let tag = draft_at(
        f,
        &s,
        tagged(SLACK_ALL_TEAM, plain_thread, "1731601600.000800", "Tagged."),
        now(),
    )
    .await;
    assert_eq!(tag["status"], "pending_approval", "{tag}");

    // A bad id is refused before anything is queued.
    let bad = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            tagged(SLACK_ALL_TEAM, thread, "not-a-ts", "x"),
            now(),
        )
        .await;
    assert!(bad.unwrap_err().to_string().contains("timestamp"));

    // A rejected tag draft doesn't block a new one for the same message.
    decide(f, &id, Decision::Reject, None, false, now())
        .await
        .unwrap();
    let redraft = draft_at(
        f,
        &s,
        tagged(SLACK_ALL_TEAM, thread, "1731600300.000200", "Try again."),
        now(),
    )
    .await;
    assert_eq!(redraft["status"], "pending_approval", "{redraft}");
}

async fn a_tagged_message_id_is_kept_and_never_sent(f: &Fixture) {
    let posted_args = tagged(
        SLACK_ALL_TEAM,
        "1731600000.000100",
        "1731600300.000200",
        "Posted text.",
    );
    let posted = internal_draft(f, "tsk_tk1", posted_args.clone()).await;
    let undecided = internal_draft(
        f,
        "tsk_tk2",
        tagged(
            SLACK_ALL_TEAM,
            "1731600000.000100",
            "1731600600.000400",
            "Never decided.",
        ),
    )
    .await;

    // Approved: what reaches Slack has no `message_ts`.
    decide(f, &posted, Decision::Approve, None, false, hours(2))
        .await
        .unwrap();
    let writes = f.slack.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].args,
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Posted text."})
    );
    // The ledger's edit distance measures the text, not the id: unchanged is 0.
    let rows = f
        .stores
        .approvals
        .ledger(&LedgerQuery::all(now() - Duration::days(365)))
        .await
        .unwrap();
    let row = rows.iter().find(|r| r.id == posted).unwrap();
    assert_eq!(row.edit_distance, Some(0.0));

    // Expiry keeps it.
    let swept = f.svc.sweep_slack_drafts(hours(24)).await.unwrap();
    assert_eq!(swept.expired, 1);
    let expired = item(f, &undecided).await;
    assert_eq!(expired.status, Status::Expired);
    assert_eq!(
        expired.args,
        json!({
            "channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100",
            "message_ts": "1731600600.000400", "text": EXPIRED_TEXT,
        })
    );

    // So does the purge of a decided draft's text.
    let swept = f.svc.sweep_slack_drafts(hours(26)).await.unwrap();
    assert_eq!(swept.redacted, 1);
    let gone = item(f, &posted).await;
    assert_eq!(
        gone.args,
        json!({
            "channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100",
            "message_ts": "1731600300.000200", "text": EXPIRED_TEXT,
        })
    );
    assert_eq!(gone.args_hash, args_hash(&posted_args));

    // An approved tag draft still blocks a second one for that message, after the purge.
    let s = session(f, "tsk_tk3").await;
    let again = draft_at(f, &s, posted_args, hours(27)).await;
    assert_eq!(again["status"], "already_drafted");
    assert_eq!(again["approval_id"], json!(posted));
}

// ----- the pending list for the App Home ---------------------------------------------------

async fn the_pending_list_is_oldest_first_and_capped(f: &Fixture) {
    let mut ids = Vec::new();
    for i in 0..5 {
        let s = session(f, &format!("tsk_pl{i}")).await;
        let res = draft_at(
            f,
            &s,
            reply(SLACK_ALL_TEAM, &format!("173160000{i}.000100"), "x"),
            now() + Duration::minutes(i),
        )
        .await;
        ids.push(res["approval_id"].as_str().unwrap().to_string());
    }
    // Decided items, and someone else's, are left out.
    decide(f, &ids[1], Decision::Reject, None, false, now())
        .await
        .unwrap();
    let ben = f
        .svc
        .create_session("ben", "reply-test", "tsk_plben", None, now())
        .await
        .unwrap();
    draft_at(
        f,
        &ben,
        reply(SLACK_ALL_TEAM, "1731609999.000100", "x"),
        now(),
    )
    .await;

    let (items, total) = f.svc.pending_approvals(SLACK_PM, 50).await.unwrap();
    let got: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(got, vec![&ids[0], &ids[2], &ids[3], &ids[4]]);
    assert_eq!(total, 4);
    assert!(items.iter().all(|i| i.status == Status::Pending));
    let (items, total) = f.svc.pending_approvals(SLACK_PM, 2).await.unwrap();
    let got: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!((got, total), (vec![ids[0].as_str(), ids[2].as_str()], 4));
}

// ----- over HTTP ----------------------------------------------------------------------------

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";

struct Server {
    base: String,
    cancel: CancellationToken,
    f: Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn start() -> Server {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: None,
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
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
    Server { base, cancel, f }
}

impl Server {
    async fn decide(&self, id: &str, body: Value) -> (u16, Value) {
        let res = reqwest::Client::new()
            .post(format!("{}/internal/approvals/{id}/decision", self.base))
            .bearer_auth(BOT_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// A draft made `hours_ago` hours before the real now, so the HTTP layer's clock sees it
    /// at that age.
    async fn draft(
        &self,
        run: &str,
        read: Option<(&str, Value)>,
        args: Value,
        hours_ago: i64,
    ) -> String {
        let at = Utc::now() - Duration::hours(hours_ago);
        let s = self
            .f
            .svc
            .create_session(SLACK_PM, "reply-test", run, Some(Duration::hours(48)), at)
            .await
            .unwrap();
        if let Some((tool, read_args)) = read {
            self.f.svc.call(&s, tool, read_args, at).await.unwrap();
        }
        let tool = if args.get("thread_ts").is_some() {
            "slack_post_reply"
        } else {
            "slack_send_message"
        };
        let res = self.f.svc.call(&s, tool, args, at).await.unwrap();
        res["approval_id"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn decision_answers_over_http() {
    let s = start().await;
    let nda = Some((
        "context_read",
        json!({"path": "context/partners/globex-nda.md"}),
    ));
    let jira = Some(("jira_read", json!({"key": "PM-101"})));
    let approve = json!({"decided_by": SLACK_PM, "decision": "approve"});

    // 409 audience_blocked, with the count.
    let blocked = s
        .draft(
            "tsk_h1",
            nda.clone(),
            reply(SLACK_WITH_OUTSIDER, "1731601200.000600", "x"),
            0,
        )
        .await;
    let (status, body) = s.decide(&blocked, approve.clone()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "audience_blocked");
    assert_eq!(body["outsiders"], 1);
    assert_eq!(body["members"], 3);
    assert_eq!(body["external"], false);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("restricted sources"));

    // 409 needs_external_confirm, then 200 with `confirm_external: true`.
    let ext = s
        .draft(
            "tsk_h2",
            jira.clone(),
            reply(SLACK_CONNECT, "1731601500.000700", "x"),
            0,
        )
        .await;
    let (status, body) = s.decide(&ext, approve.clone()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "needs_external_confirm");
    assert_eq!(body["external"], true);
    assert!(body["error"].is_string());
    let (status, body) = s
        .decide(
            &ext,
            json!({"decided_by": SLACK_PM, "decision": "approve", "confirm_external": true}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "approved");

    // 503 audience_unavailable.
    let nowhere = s
        .draft(
            "tsk_h3",
            jira.clone(),
            reply("C0NOWHERE", "1731600000.000100", "x"),
            0,
        )
        .await;
    let (status, body) = s.decide(&nowhere, approve.clone()).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "audience_unavailable");

    // 409 expired: a draft from 25 hours ago, which the sweeper hasn't reached.
    let old = s
        .draft(
            "tsk_h4",
            jira,
            reply(SLACK_ALL_TEAM, "1731600000.000100", "x"),
            25,
        )
        .await;
    let (status, body) = s.decide(&old, approve.clone()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "expired");
    let (status, body) = s
        .decide(&old, json!({"decided_by": SLACK_PM, "decision": "reject"}))
        .await;
    assert_eq!((status, body["code"].as_str()), (409, Some("expired")));

    // The item reads `expired`, without its text.
    let res = reqwest::Client::new()
        .get(format!(
            "{}/internal/approvals/{old}?user={SLACK_PM}",
            s.base
        ))
        .bearer_auth(BOT_TOKEN)
        .send()
        .await
        .unwrap();
    let item: Value = res.json().await.unwrap();
    assert_eq!(item["status"], "expired");
    assert_eq!(item["args"]["text"], EXPIRED_TEXT);
    assert_eq!(item["sources_sensitivity"], "internal");
    assert!(item["redacted_at"].is_string());

    // Listing by the new status works.
    let res = reqwest::Client::new()
        .get(format!(
            "{}/internal/approvals?user={SLACK_PM}&status=expired",
            s.base
        ))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap();
    let listed: Value = res.json().await.unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);

    // The ledger reports it as a stored outcome.
    let res = reqwest::Client::new()
        .get(format!("{}/internal/ledger?outcome=expired", s.base))
        .bearer_auth(BOT_TOKEN)
        .send()
        .await
        .unwrap();
    let ledger: Value = res.json().await.unwrap();
    let rows = ledger["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    // No approval ids for the bot: the digest goes by `run_id`.
    assert!(rows[0].get("id").is_none());
    assert_eq!(rows[0]["run_id"], "tsk_h4");
    assert!(
        !ledger.to_string().contains("[expired]"),
        "no args in the ledger"
    );
}

#[tokio::test]
async fn the_bot_lists_a_members_pending_approvals_over_http() {
    let s = start().await;
    let jira = Some(("jira_read", json!({"key": "PM-101"})));
    // 52 pending drafts, oldest first by creation, and one decided.
    let mut ids = Vec::new();
    for i in 0..52i64 {
        let at = Utc::now() - Duration::minutes(120 - i);
        let session =
            s.f.svc
                .create_session(
                    SLACK_PM,
                    "reply-test",
                    &format!("tsk_hp{i}"),
                    Some(Duration::hours(48)),
                    at,
                )
                .await
                .unwrap();
        let res =
            s.f.svc
                .call(
                    &session,
                    "slack_post_reply",
                    reply(SLACK_ALL_TEAM, &format!("17316{i:05}.000100"), "x"),
                    at,
                )
                .await
                .unwrap();
        ids.push(res["approval_id"].as_str().unwrap().to_string());
    }
    let decided = s
        .draft(
            "tsk_hpd",
            jira,
            reply(SLACK_ALL_TEAM, "1731699999.000100", "x"),
            0,
        )
        .await;
    s.decide(
        &decided,
        json!({"decided_by": SLACK_PM, "decision": "reject"}),
    )
    .await;

    let get = |token: &'static str, query: String| {
        let base = s.base.clone();
        async move {
            let res = reqwest::Client::new()
                .get(format!("{base}/internal/approvals{query}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            let status = res.status().as_u16();
            (status, res.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    // The bot: pending only, oldest first, at most 50, with the total. `status` is ignored.
    for query in [
        format!("?user={SLACK_PM}"),
        format!("?user={SLACK_PM}&status=rejected"),
        format!("?user={SLACK_PM}&status=nonsense"),
    ] {
        let (status, body) = get(BOT_TOKEN, query.clone()).await;
        assert_eq!(status, 200, "{query}: {body}");
        assert_eq!(body["total"], 52, "{query}");
        let listed: Vec<&str> = body["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        let want: Vec<&str> = ids[..50].iter().map(String::as_str).collect();
        assert_eq!(listed, want, "{query}");
        assert!(body["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "pending"));
        assert!(body.get("items").is_none());
    }
    // Each item reads as `GET /internal/approvals/{id}` returns it.
    let (_, one) = get(BOT_TOKEN, format!("/{}?user={SLACK_PM}", ids[0])).await;
    let (_, body) = get(BOT_TOKEN, format!("?user={SLACK_PM}")).await;
    assert_eq!(body["approvals"][0], one);

    // A non-member, a team bot's identity, or no user: refused.
    for query in ["?user=eve", "?user=bots.docs"] {
        let (status, body) = get(BOT_TOKEN, query.into()).await;
        assert_eq!(status, 403, "{query}");
        assert_eq!(
            body["error"],
            "this token may list approvals for team members only"
        );
    }
    let (status, body) = get(BOT_TOKEN, String::new()).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("`user` is required"))
    );

    // The orchestrator: unchanged. Every item, newest first, `status` filters, no cap.
    let (status, body) = get(SERVICE_TOKEN, format!("?user={SLACK_PM}")).await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 53);
    assert_eq!(items[0]["id"], json!(decided));
    assert!(body.get("total").is_none());
    let (_, body) = get(SERVICE_TOKEN, format!("?user={SLACK_PM}&status=rejected")).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    let (_, body) = get(SERVICE_TOKEN, format!("?user={SLACK_PM}&status=pending")).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 52);
    assert_eq!(body["items"][0]["id"], json!(ids[51]));
    let (status, _) = get(SERVICE_TOKEN, format!("?user={SLACK_PM}&status=nonsense")).await;
    assert_eq!(status, 400);
    // ...and may list anyone's.
    let (status, _) = get(SERVICE_TOKEN, "?user=eve".into()).await;
    assert_eq!(status, 200);
}
