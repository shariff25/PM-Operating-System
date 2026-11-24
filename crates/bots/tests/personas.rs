//! Marketplace bots in the runtime (`docs/bot-marketplace-spec.md` §6, slice R1): a task,
//! routine or conversation names a persona; the runtime checks it against the catalog, the
//! tool server decides the team at claim, and the claim runs as the persona's role. Every
//! case runs on both stores, against a mock tool server.

#[macro_use]
mod common;

use common::{at, persona_catalog, Env};
use pmos_bots::model::{Draft, Output, Schedule, SlackRef, Source, Task, TaskStatus};
use pmos_bots::store::TaskFilter;
use serde_json::{json, Value};

macro_rules! persona_envs {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                let env = super::common::Env::memory_with(super::persona_catalog()).await;
                super::$case(&env).await;
                env.finish().await;
            } )*
        }
        mod postgres {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                if let Some(env) = super::common::Env::postgres_with(super::persona_catalog()).await {
                    super::$case(&env).await;
                    env.finish().await;
                }
            } )*
        }
    };
}

persona_envs!(
    a_named_persona_is_checked_against_the_catalog,
    nudge_is_stored_as_null,
    the_claim_runs_as_the_personas_role_with_the_sessions_tools,
    a_claim_without_a_persona_is_as_before,
    a_refused_session_fails_the_task_with_the_sentence,
    a_tool_server_that_drops_the_persona_fails_the_task,
    a_withdrawn_persona_fails_a_queued_task_at_claim,
    history_keeps_to_one_persona_and_a_reply_inherits_it,
    a_reply_inherits_only_the_requesters_own_interactive_task,
    routines_carry_a_persona_and_pause_when_it_is_refused,
    a_withdrawn_personas_routine_pauses_at_its_next_run,
    personas_are_listed_for_the_clients,
    tasks_list_by_persona,
);

async fn ask_as(env: &Env, persona: Option<&str>, thread: &str, text: &str) -> (u16, Value) {
    let mut body = json!({"bot": "pm-ana", "requested_by": "ana", "text": text, "source": "slack",
                          "slack": {"channel": "D0ANA", "thread_ts": thread}});
    if let Some(p) = persona {
        body["persona"] = json!(p);
    }
    env.post("/v1/tasks", "slack", Some("ana"), body).await
}

async fn complete(env: &Env, claim: &Value, result: &str) {
    let tid = claim["task"]["id"].as_str().unwrap();
    let (status, body) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": result, "approvals": []}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
}

async fn a_named_persona_is_checked_against_the_catalog(env: &Env) {
    let (status, body) = ask_as(env, Some("quill"), "1.1", "x").await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "unknown_persona");
    // Withdrawn: defined, but not offered.
    let (status, body) = ask_as(env, Some("ghost"), "1.1", "x").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("unknown_persona"))
    );
    let (status, body) = ask_as(env, Some("Chief!"), "1.1", "x").await;
    assert_eq!((status, body["code"].as_str()), (400, Some("bad_request")));
    // A team bot never runs as a persona.
    let (status, body) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "docs", "requested_by": "ana", "text": "x", "source": "slack",
                   "persona": "chief"}),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("persona_needs_personal_bot")),
        "{body}"
    );
    // Nothing was queued, and the team is not checked here (the tool server does, at claim).
    assert_eq!(env.claim().await.0, 204);
    let (status, task) = ask_as(env, Some("chief"), "1.1", "Prep me for Monday").await;
    assert_eq!(status, 201, "{task}");
    assert_eq!(task["persona"], "chief");
}

async fn nudge_is_stored_as_null(env: &Env) {
    let (status, task) = ask_as(env, Some("nudge"), "2.1", "hello").await;
    assert_eq!(status, 201, "{task}");
    assert_eq!(task["persona"], Value::Null);
    let stored = env.rt.task(task["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(stored.persona, None);
    let (status, task) = ask_as(env, None, "2.2", "hello").await;
    assert_eq!((status, &task["persona"]), (201, &Value::Null));
}

async fn the_claim_runs_as_the_personas_role_with_the_sessions_tools(env: &Env) {
    ask_as(env, Some("chief"), "3.1", "Prep me for Monday").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    let sent = env.ts.sessions.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent["persona"], "chief");
    assert_eq!(sent["bot"], "pm-ana");
    assert_eq!(claim["bot"]["name"], "pm-ana");
    assert_eq!(claim["role"]["name"], "chief-of-staff");
    assert_eq!(claim["role"]["skills"], json!(["meet"]));
    assert_eq!(
        claim["role"]["instructions"],
        "Lead with what needs the PM."
    );
    assert_eq!(claim["gateway"]["model"], "pmos/reasoning");
    assert_eq!(
        claim["persona"],
        json!({"key": "chief", "display_name": "Chief", "title": "Chief title"})
    );
    // The session's tools (harness names), by registry name.
    assert_eq!(claim["tools"], json!(["jira.search"]));
    assert_eq!(claim["task"]["persona"], "chief");
}

/// A Nudge claim has no `persona` field at all, so an older worker reads it as before.
async fn a_claim_without_a_persona_is_as_before(env: &Env) {
    ask_as(env, None, "4.1", "hello").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert!(claim.get("persona").is_none(), "{claim}");
    assert_eq!(claim["role"]["name"], "pm-assistant");
    let sent = env.ts.sessions.lock().unwrap().last().cloned().unwrap();
    assert!(sent.get("persona").is_none(), "{sent}");
}

const NOT_ON_TEAM: &str =
    "Chief isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge.";

async fn a_refused_session_fails_the_task_with_the_sentence(env: &Env) {
    *env.ts.persona_refusal.lock().unwrap() =
        Some(("persona_not_on_team".into(), NOT_ON_TEAM.into()));
    let (_, task) = ask_as(env, Some("chief"), "5.1", "Prep me").await;
    assert_eq!(
        env.claim().await.0,
        204,
        "the refused task is not handed out"
    );
    let t = env.rt.task(task["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(t.status, TaskStatus::Failed);
    assert_eq!(t.error.as_deref(), Some(NOT_ON_TEAM));
    let feed = env.feed().await;
    assert_eq!(feed.last().unwrap()["text"], NOT_ON_TEAM);
    assert_eq!(feed.last().unwrap()["persona"], "chief");

    // gate-fix-4: the same ask from Slack moments later is refused at once, with no task and
    // no claim (the runtime can't read a team; this bounds the repeats).
    let before = env.feed().await.len();
    let (status, body) = ask_as(env, Some("chief"), "5.2", "Prep me again").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("persona_not_on_team")),
        "{body}"
    );
    assert_eq!(env.claim().await.0, 204, "nothing was queued");
    assert_eq!(env.feed().await.len(), before);
    // Another bot, or another person, is not affected.
    let (status, _) = ask_as(env, Some("nudge"), "5.3", "x").await;
    assert_eq!(status, 201);
    // A web ask naming the bot (pmos-web checked the team first) clears it.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "web",
            Some("ana"),
            json!({"bot": "pm-ana", "requested_by": "ana", "text": "x", "source": "web",
                   "persona": "chief"}),
        )
        .await;
    assert_eq!(status, 201);
    let (status, body) = ask_as(env, Some("chief"), "5.4", "and now?").await;
    assert_eq!(status, 201, "{body}");
}

/// A tool server that ignores `persona` (deployed before personas) gives a Nudge session: the
/// runtime refuses to run a persona task on it, and says so.
async fn a_tool_server_that_drops_the_persona_fails_the_task(env: &Env) {
    *env.ts.drop_persona.lock().unwrap() = true;
    let (_, task) = ask_as(env, Some("chief"), "6.1", "Prep me").await;
    assert_eq!(env.claim().await.0, 204);
    let t = env.rt.task(task["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(t.status, TaskStatus::Failed);
    let error = t.error.unwrap();
    assert!(error.contains("didn't run it as Chief"), "{error}");
    // The session that wasn't opened as Chief is closed, never handed to a worker.
    let n = env.ts.session_count();
    assert_eq!(*env.ts.revoked.lock().unwrap(), [format!("pmos_s_test{n}")]);
}

async fn a_withdrawn_persona_fails_a_queued_task_at_claim(env: &Env) {
    let (_, task) = ask_as(env, Some("chief"), "7.1", "Prep me").await;
    let mut catalog = persona_catalog();
    catalog = withdraw(catalog, "chief");
    env.rt.set_catalog(std::sync::Arc::new(catalog));
    let before = env.ts.session_count();
    assert_eq!(env.claim().await.0, 204);
    assert_eq!(env.ts.session_count(), before, "no session is asked for");
    let t = env.rt.task(task["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(t.status, TaskStatus::Failed);
    assert_eq!(
        t.error.as_deref(),
        Some("Chief isn't offered in PM OS any more. Ask Nudge instead.")
    );
}

fn withdraw(catalog: pmos_bots::catalog::Catalog, key: &str) -> pmos_bots::catalog::Catalog {
    let personas = ["nudge", "chief", "ghost"]
        .iter()
        .filter_map(|k| catalog.persona(k).cloned())
        .map(|mut p| {
            if p.name == key {
                p.offered = false;
            }
            p
        })
        .collect();
    catalog.with_personas(personas)
}

async fn history_keeps_to_one_persona_and_a_reply_inherits_it(env: &Env) {
    ask_as(env, Some("chief"), "8.1", "What slipped since Monday?").await;
    let (_, first) = env.claim().await;
    complete(env, &first, "PM-101 moved.").await;
    // A reply naming nobody stays with Chief, and sees Chief's turn.
    let (_, reply) = ask_as(env, None, "8.1", "and PM-102?").await;
    assert_eq!(reply["persona"], "chief");
    let (_, second) = env.claim().await;
    assert_eq!(second["role"]["name"], "chief-of-staff");
    assert_eq!(second["history"].as_array().unwrap().len(), 1);
    complete(env, &second, "No change.").await;
    // Naming Nudge in Chief's thread starts Nudge with no history of Chief's turns.
    let (_, nudge) = ask_as(env, Some("nudge"), "8.1", "Nudge, what about you?").await;
    assert_eq!(nudge["persona"], Value::Null);
    let (_, third) = env.claim().await;
    assert_eq!(third["role"]["name"], "pm-assistant");
    assert_eq!(
        third["history"],
        json!([]),
        "another persona's turns are left out"
    );
    complete(env, &third, "Hi.").await;
    // And the thread's latest task is now Nudge's: a reply naming nobody stays with Nudge.
    let (_, after) = ask_as(env, None, "8.1", "thanks").await;
    assert_eq!(after["persona"], Value::Null);
    let (_, fourth) = env.claim().await;
    assert_eq!(fourth["history"].as_array().unwrap().len(), 1);
}

/// Whose task a reply inherits (the reviewer's question, settled in R2): the latest task in
/// the thread that the **same requester** asked of the **same bot**, interactively. Another
/// person's task in that thread, and a routine's run (which starts no thread), pass nothing on.
async fn a_reply_inherits_only_the_requesters_own_interactive_task(env: &Env) {
    let thread = SlackRef {
        channel: "D0ANA".into(),
        thread_ts: "9.1".into(),
    };
    // Someone else's Chief task in the thread (only the store can put one there: a personal
    // bot serves its owner), and a routine's run as Chief with the same Slack place.
    let mut other = Task::queued(
        "pm-ana",
        "ben",
        Source::Slack,
        "ben asks",
        Some(thread.clone()),
        None,
        at(2026, 11, 20, 9, 0),
    );
    other.persona = Some("chief".into());
    env.store.create_task(&other, "queued").await.unwrap();
    let mut run = Task::queued(
        "pm-ana",
        "ana",
        Source::Routine,
        "[daily-brief] Write my daily brief with the daily-brief skill.",
        Some(thread.clone()),
        Some("rtn_x".into()),
        at(2026, 11, 20, 9, 1),
    );
    run.persona = Some("chief".into());
    env.store.create_task(&run, "queued").await.unwrap();
    let (status, reply) = ask_as(env, None, "9.1", "thanks").await;
    assert_eq!(status, 201, "{reply}");
    assert_eq!(
        reply["persona"],
        Value::Null,
        "neither another person's task nor a routine's run is inherited"
    );
    // The requester's own Chief task in the thread is.
    ask_as(env, Some("chief"), "9.1", "what slipped?").await;
    let (_, again) = ask_as(env, None, "9.1", "and then?").await;
    assert_eq!(again["persona"], "chief");
}

fn draft(persona: Option<&str>) -> Draft {
    Draft {
        bot: "pm-ana".into(),
        owner: "ana".into(),
        schedule: Schedule {
            cron: "0 8 * * 1-5".into(),
            tz: "UTC".into(),
        },
        prompt: "What slipped?".into(),
        output: Output::dm(),
        approval: "approve".into(),
        persona: persona.map(str::to_string),
    }
}

async fn routines_carry_a_persona_and_pause_when_it_is_refused(env: &Env) {
    // Created over HTTP with a persona, by the web client (R2: the only client that may name
    // one, since only pmos-web can check the team); an unknown one is refused.
    let mut body = json!({"draft": draft(Some("quill"))});
    let (status, refused) = env
        .post("/v1/routines", "web", Some("ana"), body.clone())
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (400, Some("unknown_persona"))
    );
    body["draft"]["persona"] = json!("chief");
    let (status, _) = env
        .post("/v1/routines", "slack", Some("ana"), body.clone())
        .await;
    assert_eq!(status, 403, "the Slack bot can't check the team");
    let (status, r) = env.post("/v1/routines", "web", Some("ana"), body).await;
    assert_eq!(status, 201, "{r}");
    assert_eq!(r["persona"], "chief");
    let id = r["id"].as_str().unwrap().to_string();
    // PATCH moves it to Nudge and back; the console may not.
    let path = format!("/v1/routines/{id}");
    let (status, moved) = env
        .patch(&path, "slack", Some("ana"), json!({"persona": null}))
        .await;
    assert_eq!((status, &moved["persona"]), (200, &Value::Null), "{moved}");
    let (status, moved) = env
        .patch(&path, "slack", Some("ana"), json!({"persona": "nudge"}))
        .await;
    assert_eq!((status, &moved["persona"]), (200, &Value::Null), "{moved}");
    let (status, _) = env
        .patch(&path, "console", None, json!({"persona": "chief"}))
        .await;
    assert_eq!(status, 403);
    let (status, _) = env
        .patch(&path, "slack", Some("ana"), json!({"persona": "chief"}))
        .await;
    assert_eq!(
        status, 403,
        "the Slack bot may only move a routine to Nudge"
    );
    let (status, moved) = env
        .patch(&path, "web", Some("ana"), json!({"persona": "chief"}))
        .await;
    assert_eq!((status, moved["persona"].as_str()), (200, Some("chief")));
    let (status, _) = env
        .patch(&path, "slack", Some("ana"), json!({"enabled": true}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        env.rt.routine(&id).await.unwrap().persona.as_deref(),
        Some("chief")
    );

    // Its run carries the persona; the tool server says Chief left the team: the run fails
    // with the sentence and the routine pauses.
    let due = env.rt.routine(&id).await.unwrap().next_run_at.unwrap();
    assert_eq!(env.rt.run_due_routines(due).await.unwrap(), 1);
    *env.ts.persona_refusal.lock().unwrap() =
        Some(("persona_not_on_team".into(), NOT_ON_TEAM.into()));
    assert!(env.rt.claim_now("w1", None, due).await.unwrap().is_none());
    let sent = env.ts.sessions.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent["persona"], "chief");
    let r = env.rt.routine(&id).await.unwrap();
    assert!(!r.enabled);
    assert_eq!(r.next_run_at, None);
    let tasks = env
        .store
        .tasks(&TaskFilter {
            limit: 10,
            ..TaskFilter::default()
        })
        .await
        .unwrap();
    let run = tasks
        .iter()
        .find(|t| t.routine_id.as_deref() == Some(&id))
        .unwrap();
    assert_eq!(run.persona.as_deref(), Some("chief"));
    assert_eq!(
        run.error.as_deref(),
        Some(
            format!(
                "Your routine is paused: {}.",
                NOT_ON_TEAM.trim_end_matches('.')
            )
            .as_str()
        )
    );
}

async fn a_withdrawn_personas_routine_pauses_at_its_next_run(env: &Env) {
    let created = at(2026, 11, 14, 12, 0);
    let r = env
        .rt
        .create_routine(draft(Some("chief")), created)
        .await
        .unwrap();
    env.rt
        .set_catalog(std::sync::Arc::new(withdraw(persona_catalog(), "chief")));
    let due = r.next_run_at.unwrap();
    assert_eq!(env.rt.run_due_routines(due).await.unwrap(), 1);
    let r = env.rt.routine(&r.id).await.unwrap();
    assert!(!r.enabled, "paused");
    let feed = env.feed().await;
    assert_eq!(
        feed.last().unwrap()["text"],
        "Your routine is paused: its bot Chief isn't offered in PM OS any more."
    );
}

async fn personas_are_listed_for_the_clients(env: &Env) {
    for client in ["web", "slack", "console", "evals"] {
        let (status, body) = env.get("/v1/personas", client, Some("ana")).await;
        assert_eq!(status, 200, "{client}: {body}");
        let keys: Vec<&str> = body["personas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["nudge", "chief"], "offered only, Nudge first");
        assert_eq!(body["personas"][1]["role"], "chief-of-staff");
        assert_eq!(body["personas"][0]["always_on"], true);
        // The Preview label is carried for the screens ("Preview: not yet benchmarked").
        assert_eq!(body["personas"][0]["preview"], false);
        assert_eq!(body["personas"][1]["preview"], true);
        // R3: the benchmark runner (and only it) is told what the runtime's own gateway
        // serves: model ids, upstream kinds and the class, never an address.
        if client == "evals" {
            assert_eq!(body["gateway"]["class"], "production", "{body}");
            assert_eq!(body["gateway"]["roles"][0]["model"], "qwen-prod");
            assert!(!body.to_string().contains("10.9.9.9"), "{body}");
        } else {
            assert!(body.get("gateway").is_none(), "{client}: {body}");
        }
    }
    let (status, _) = env.get("/v1/personas", "worker", None).await;
    assert_eq!(status, 403);
}

async fn tasks_list_by_persona(env: &Env) {
    ask_as(env, Some("chief"), "9.1", "a").await;
    ask_as(env, None, "9.2", "b").await;
    let (status, list) = env
        .get("/v1/tasks?persona=chief", "slack", Some("ana"))
        .await;
    assert_eq!(status, 200, "{list}");
    let texts: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["a"]);
    let (_, list) = env
        .get("/v1/tasks?persona=nudge", "slack", Some("ana"))
        .await;
    let texts: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["b"]);
    let (status, _) = env
        .get("/v1/tasks?persona=Chief!", "slack", Some("ana"))
        .await;
    assert_eq!(status, 400);
}
