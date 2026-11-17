//! A PM's level (`docs/staff-principal-spec.md` §1): the settings routes, who may call them,
//! `level` in the claim and a task's own level. Every case runs on both stores.

#[macro_use]
mod common;

use common::Env;
use serde_json::json;

const ANA: &str = "/v1/members/ana/settings";

async fn an_unset_level_reads_as_senior(env: &Env) {
    let (status, body) = env.get(ANA, "slack", Some("ana")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"handle": "ana", "level": "senior"}));
    let (status, body) = env.get(ANA, "console", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"handle": "ana", "level": "senior"}));
}

async fn a_level_is_set_and_read_back(env: &Env) {
    let (status, body) = env
        .put(ANA, "slack", Some("ana"), json!({"level": "staff"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"handle": "ana", "level": "staff"}));
    let (_, body) = env.get(ANA, "slack", Some("ana")).await;
    assert_eq!(body, json!({"handle": "ana", "level": "staff"}));
    let (_, body) = env.get(ANA, "console", None).await;
    assert_eq!(body["level"], "staff", "the console may read it");
    let (status, body) = env
        .put(ANA, "slack", Some("ana"), json!({"level": "principal"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["level"], "principal");
    // Ben's is untouched.
    let (_, body) = env
        .get("/v1/members/ben/settings", "slack", Some("ben"))
        .await;
    assert_eq!(body, json!({"handle": "ben", "level": "senior"}));
}

async fn only_the_member_themselves_sets_their_level(env: &Env) {
    let ben = "/v1/members/ben/settings";
    let (status, body) = env.get(ben, "slack", Some("ana")).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = env
        .put(ben, "slack", Some("ana"), json!({"level": "staff"}))
        .await;
    assert_eq!(status, 403, "another member's handle: {body}");
    let (status, _) = env.get(ANA, "slack", None).await;
    assert_eq!(status, 403, "the Slack client names its PM");
    let (status, _) = env.put(ANA, "slack", None, json!({"level": "staff"})).await;
    assert_eq!(status, 403);
    for client in ["console", "evals", "worker"] {
        let (status, _) = env.put(ANA, client, None, json!({"level": "staff"})).await;
        assert_eq!(status, 403, "{client} may not set a level");
    }
    for client in ["evals", "worker"] {
        let (status, _) = env.get(ANA, client, None).await;
        assert_eq!(status, 403, "{client} may not read a level");
    }
    let (status, _) = env.get(ANA, "nobody", None).await;
    assert_eq!(status, 401);
    let (_, body) = env.get(ben, "console", None).await;
    assert_eq!(body["level"], "senior", "nothing changed");
}

async fn an_unknown_level_is_a_400(env: &Env) {
    let (status, body) = env
        .put(ANA, "slack", Some("ana"), json!({"level": "director"}))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("senior, staff or principal"),
        "{body}"
    );
    for bad in [
        json!({}),
        json!({"level": 3}),
        json!({"level": "staff", "x": 1}),
    ] {
        let (status, _) = env.put(ANA, "slack", Some("ana"), bad).await;
        assert_eq!(status, 400);
    }
    let (_, body) = env.get(ANA, "slack", Some("ana")).await;
    assert_eq!(body["level"], "senior");
}

async fn a_non_member_is_refused(env: &Env) {
    let eve = "/v1/members/eve/settings";
    let (status, body) = env
        .put(eve, "slack", Some("eve"), json!({"level": "staff"}))
        .await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("isn't a PM OS user"),
        "{body}"
    );
    let (_, body) = env.get(eve, "console", None).await;
    assert_eq!(body["level"], "senior");
}

async fn the_claim_carries_the_requesters_level(env: &Env) {
    env.ask("Where do we stand?").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["level"], "senior", "unset reads as senior");

    env.put(ANA, "slack", Some("ana"), json!({"level": "staff"}))
        .await;
    env.ask("And across the teams?").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["level"], "staff");
    assert!(
        claim["task"].get("level").is_none(),
        "the task has no level of its own: {claim}"
    );
}

async fn a_tasks_own_level_wins(env: &Env) {
    // ben is the benchmark member (PMOS_EVALS_AS) in these tests.
    env.put(
        "/v1/members/ben/settings",
        "slack",
        Some("ben"),
        json!({"level": "staff"}),
    )
    .await;
    let (status, task) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "Write the vision",
                   "source": "eval", "level": "principal"}),
        )
        .await;
    assert_eq!(status, 201, "{task}");
    assert_eq!(task["level"], "principal", "stored on the task");
    let (_, read) = env
        .get(
            &format!("/v1/tasks/{}", task["id"].as_str().unwrap()),
            "evals",
            None,
        )
        .await;
    assert_eq!(read["level"], "principal");
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["level"], "principal");

    let (status, body) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "t",
                   "source": "eval", "level": "vp"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
}

async fn only_the_evals_client_sets_a_tasks_level(env: &Env) {
    let (status, body) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "docs", "requested_by": "ana", "text": "t",
                   "source": "slack", "level": "principal"}),
        )
        .await;
    assert_eq!(status, 403, "{body}");
    for client in ["console", "worker"] {
        let (status, _) = env
            .post(
                "/v1/tasks",
                client,
                None,
                json!({"bot": "docs", "requested_by": "ana", "text": "t",
                       "source": "eval", "level": "staff"}),
            )
            .await;
        assert_eq!(status, 403, "{client}");
    }
    let (_, tasks) = env.get("/v1/tasks", "console", None).await;
    assert_eq!(tasks.as_array().unwrap().len(), 0, "nothing was created");
}

both_envs!(
    an_unset_level_reads_as_senior,
    a_level_is_set_and_read_back,
    only_the_member_themselves_sets_their_level,
    an_unknown_level_is_a_400,
    a_non_member_is_refused,
    the_claim_carries_the_requesters_level,
    a_tasks_own_level_wins,
    only_the_evals_client_sets_a_tasks_level,
);

/// The setting is in Postgres, not the process: a restarted runtime still has it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_level_survives_a_restart_on_postgres() {
    let Some(env) = Env::postgres().await else {
        return;
    };
    let (status, _) = env
        .put(ANA, "slack", Some("ana"), json!({"level": "principal"}))
        .await;
    assert_eq!(status, 200);
    let again = env.restarted().await.expect("a Postgres env restarts");
    let (status, body) = again.get(ANA, "slack", Some("ana")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"handle": "ana", "level": "principal"}));
    again.ask("Which bets define the business?").await;
    let (_, claim) = again.claim().await;
    assert_eq!(claim["level"], "principal");
    again.finish().await;
    env.finish().await;
}
