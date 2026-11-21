//! Slice B1 of the bot marketplace (`docs/bot-marketplace-spec.md` §5–§7, §13): each PM's
//! team, the catalog read, persona sessions and attribution, on the in-memory stores and, with
//! `PMOS_TEST_DATABASE_URL`, Postgres.
//!
//! The fixture adds three personas to the repository's content (fake text only): `quill`
//! (offered, role `quill-role`: Jira and Slack replies, `slack.send_message: never`), `abacus`
//! (offered, reads and drafts Jira issues, needs an analytics source) and `ghost` (withdrawn). The
//! "hand-built" cases put content in force that `pmos lint` would refuse, to show the tool
//! server holds the ceiling on its own.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{now, write_content, Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::bots::{role_tools, ActionSetting, Role as ContentRole};
use pmos_contract::model::Role;
use pmos_contract::personas::Persona;
use pmos_contract::Sensitivity;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{Decision, Status};
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditEvent, AuditQuery, Client, Outcome, Purged};
use pmos_toolserver::connectors::fake::SLACK_WITH_OUTSIDER;
use pmos_toolserver::service::{DecisionError, SessionError, TeamRefusal, ToolError};
use pmos_toolserver::session::Session;
use pmos_toolserver::team::{AddedVia, TeamChange, TeamError, MAX_TEAM};
use pmos_toolserver::users::{Actor, Op};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
const UNKNOWN_TOKEN: &str = "not-a-token-0123456789abcdef0123";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

// ----- the fixture -------------------------------------------------------------------------

fn persona_file(
    key: &str,
    display: &str,
    role: &str,
    mascot: &str,
    offered: bool,
    order: u32,
    extra: &str,
) -> String {
    format!(
        "---\nname: {key}\ndisplay-name: {display}\ntitle: Fake {display} title\nrole: {role}\nmascot: {mascot}\nowner: \"TODO\"\nversion: \"0.1.0\"\noffered: {offered}\norder: {order}\nsummary: Fake persona for tests.\nexamples:\n  - Fake example ask\n{extra}---\n\n# {display}\n\nFake.\n"
    )
}

/// The personal role's standing sections, as every persona role carries them verbatim (the
/// lint's `persona-role-sections`), read from the content tree under test.
fn standing_sections(root: &Path) -> String {
    let base = std::fs::read_to_string(root.join("roles/pm-assistant/ROLE.md")).unwrap();
    let body = base.splitn(3, "---\n").nth(2).unwrap();
    pmos_contract::personas::role_sections(body)
        .into_iter()
        .filter(|(h, _)| pmos_contract::personas::STANDING_SECTIONS.contains(&h.as_str()))
        .map(|(_, text)| format!("{text}\n\n"))
        .collect()
}

/// The marketplace content the tests use (see the module docs), and Bo's personal bot.
fn market(root: &Path) {
    let standing = standing_sections(root);
    write_content(
        root,
        "roles/quill-role/ROLE.md",
        &format!("---\nname: quill-role\ndescription: Fake persona role that drafts Jira issues and Slack replies.\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"ask slack-reply jira-draft meeting-notes\"\ndata-class: \"restricted\"\nmodel: \"default\"\nactions:\n  jira.create_issue: approve\n  slack.post_reply: approve\n  slack.send_message: never\n---\n\n# Quill\n\nFake.\n\n{standing}"),
    );
    write_content(
        root,
        "roles/abacus-role/ROLE.md",
        &format!("---\nname: abacus-role\ndescription: Fake persona role that reads and drafts Jira issues.\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"ask jira-draft\"\ndata-class: \"restricted\"\nmodel: \"default\"\nactions:\n  jira.create_issue: approve\n---\n\n# Abacus\n\nFake.\n\n{standing}"),
    );
    write_content(
        root,
        "personas/quill/PERSONA.md",
        // Quill claims a passing benchmark (the lint checks the claim against the eval reports;
        // the tool server can't read them, so it trusts only this). Abacus claims none: the
        // tool server shows it as a Preview though its file doesn't say `preview: true` (M2).
        &persona_file(
            "quill",
            "Quill",
            "quill-role",
            "chief",
            true,
            900,
            "benchmarked: true\n",
        ),
    );
    write_content(
        root,
        "personas/abacus/PERSONA.md",
        &persona_file(
            "abacus",
            "Abacus",
            "abacus-role",
            "sprout",
            true,
            910,
            "needs:\n  - source: analytics\n    label: an analytics source\n    without: I work from numbers you paste in.\n",
        ),
    );
    write_content(
        root,
        "personas/ghost/PERSONA.md",
        &persona_file("ghost", "Ghost", "quill-role", "echo", false, 920, ""),
    );
    write_content(
        root,
        "bots/pm-bo/BOT.md",
        "---\nname: pm-bo\nrole: pm-assistant\nscope: personal\nowner: \"@bo\"\ndescription: Bo's assistant.\n---\n\n# Bo's assistant\n\nHelp Bo.\n",
    );
    // Ada (an Admin) has a personal bot too, so her own drafts can be held (gate-fix-4).
    write_content(
        root,
        "bots/pm-ada/BOT.md",
        "---\nname: pm-ada\nrole: pm-assistant\nscope: personal\nowner: \"@ada\"\ndescription: Ada's assistant.\n---\n\n# Ada's assistant\n\nHelp Ada.\n",
    );
}

/// `ada` admin; `ana`, `bo`, `evals` PMs; `rev` reviewer; `aud` auditor.
async fn fixture(postgres: bool) -> Option<(Fixture, Option<TestPg>)> {
    common::roles_fixture_with(
        &[
            ("ada", Role::Admin),
            ("ana", Role::Pm),
            ("bo", Role::Pm),
            ("evals", Role::Pm),
            ("rev", Role::Reviewer),
            ("aud", Role::Auditor),
        ],
        postgres,
        market,
    )
    .await
}

/// Each case on the in-memory stores and on Postgres (skipped locally without a database).
macro_rules! both {
    ($name:ident) => {
        mod $name {
            #[tokio::test]
            async fn memory() {
                let (f, _) = super::fixture(false).await.unwrap();
                super::$name(&f, None).await;
            }

            #[tokio::test]
            async fn postgres() {
                let Some((f, pg)) = super::fixture(true).await else {
                    return;
                };
                super::$name(&f, pg.as_ref()).await;
                if let Some(pg) = pg {
                    pg.drop_schema().await;
                }
            }
        }
    };
}

/// Every event in the audit log of the backend under test, oldest first.
async fn events(f: &Fixture) -> Vec<AuditEvent> {
    let mut found = f
        .stores
        .audit
        .search(&AuditQuery {
            limit: 1001,
            ..Default::default()
        })
        .await
        .unwrap()
        .expect("searchable audit log");
    found.reverse();
    found.into_iter().map(|s| s.event).collect()
}

async fn team_changes(f: &Fixture) -> Vec<AuditEvent> {
    events(f)
        .await
        .into_iter()
        .filter(|e| e.tool == "team.change")
        .collect()
}

async fn add(f: &Fixture, who: &str, keys: &[&str]) -> Result<Value, TeamRefusal> {
    let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    f.svc
        .change_team(Client::Web, who, &keys, &[], "bots_page", now())
        .await
}

async fn remove(f: &Fixture, who: &str, keys: &[&str]) -> Result<Value, TeamRefusal> {
    let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    f.svc
        .change_team(Client::Web, who, &[], &keys, "bots_page", now())
        .await
}

fn team_keys(team: &Value) -> Vec<String> {
    team["bots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["key"].as_str().unwrap().to_string())
        .collect()
}

async fn persona_session(
    f: &Fixture,
    user: &str,
    bot: &str,
    persona: Option<&str>,
    run: &str,
) -> Result<Session, SessionError> {
    f.svc
        .create_bot_session_as(user, bot, persona, run, None, Sensitivity::Public, now())
        .await
}

fn tool_names(f: &Fixture, s: &Session) -> BTreeSet<String> {
    f.svc.list_tools(s).iter().map(|t| t.name.clone()).collect()
}

/// The roster now in force, with `key` withdrawn (`offered: false`), as a reindex after an
/// admin's pull request would put it.
fn withdraw(f: &Fixture, key: &str) {
    let roster = f.svc.roster();
    let mut p: Persona = roster.persona(key).unwrap().as_ref().clone();
    p.offered = false;
    f.svc
        .replace_roster(roster.as_ref().clone().with_unlinted_persona(p, None));
}

// ----- the catalog -------------------------------------------------------------------------

async fn catalog_serves_only_offered_personas_and_their_fields(f: &Fixture, _: Option<&TestPg>) {
    let v = f.svc.persona_catalog();
    let personas = v["personas"].as_array().unwrap();
    let keys: Vec<&str> = personas
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys[0], "nudge", "Nudge first: {keys:?}");
    let (q, a) = (
        keys.iter().position(|k| *k == "quill").unwrap(),
        keys.iter().position(|k| *k == "abacus").unwrap(),
    );
    assert!(q < a, "by order");
    assert!(!keys.contains(&"ghost"), "a withdrawn persona isn't served");
    let wanted: BTreeSet<&str> = [
        "key",
        "display_name",
        "title",
        "mascot",
        "always_on",
        // The Preview label (the owner's exception to D10), added by slice M1.
        "preview",
        "summary",
        "examples",
        "can_do",
        "reads",
        "drafts",
        "needs",
    ]
    .into();
    for p in personas {
        let fields: BTreeSet<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(fields, wanted, "only the spec's fields: {p}");
    }
    let quill = &personas[q];
    assert_eq!(quill["display_name"], "Quill");
    assert_eq!(quill["always_on"], false);
    assert_eq!(quill["preview"], false);
    assert_eq!(personas[0]["preview"], false, "Nudge is never a Preview");
    // Offered with no benchmark claimed and no `preview: true`: served as a Preview all the
    // same, whatever CI's lint did (gate-fix-4, M2).
    assert_eq!(personas[a]["key"], "abacus");
    assert_eq!(personas[a]["preview"], true, "fail safe: {}", personas[a]);
    // The repository's three new bots are Previews until their benchmark passes.
    for p in personas {
        if ["chief", "sprout", "echo"].contains(&p["key"].as_str().unwrap()) {
            assert_eq!(p["preview"], true, "{p}");
        }
    }
    let drafts: Vec<&str> = quill["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(
        drafts.contains(&"Jira issues") && drafts.contains(&"Slack replies"),
        "{drafts:?}"
    );
    assert!(
        !drafts.contains(&"Slack messages"),
        "`never` in its role: {drafts:?}"
    );
    let reads: Vec<&str> = quill["reads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(
        reads.contains(&"jira") && reads.contains(&"slack"),
        "{reads:?}"
    );
    assert!(
        !reads.contains(&"context"),
        "PM OS's own systems aren't sources"
    );
    let abacus = &personas[a];
    assert_eq!(
        abacus["needs"],
        json!([{"source": "analytics", "label": "an analytics source",
                "without": "I work from numbers you paste in.", "met": false}])
    );
    assert_eq!(personas[0]["always_on"], true);
    // The catalog has the content's commit, as the ledger does.
    assert_eq!(v["catalog_sha"], json!(f.svc.content_sha()));

    // Content put in force without the lint (hand-built, as a merge that skipped CI would):
    // neither `preview` nor `benchmarked` on the persona itself is a Preview, in the catalog
    // and the admin view alike.
    let roster = f.svc.roster();
    let mut p: Persona = roster.persona("quill").unwrap().as_ref().clone();
    (p.preview, p.benchmarked) = (false, false);
    f.svc
        .replace_roster(roster.as_ref().clone().with_unlinted_persona(p, None));
    let v = f.svc.persona_catalog();
    let quill = v["personas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "quill")
        .unwrap()
        .clone();
    assert_eq!(quill["preview"], true, "{quill}");
    let admin = f.svc.admin_personas(now()).await.unwrap();
    let row = admin["personas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "quill")
        .unwrap()
        .clone();
    assert_eq!(row["preview"], true, "{row}");
}
both!(catalog_serves_only_offered_personas_and_their_fields);

// ----- defaults ---------------------------------------------------------------------------

async fn everyone_starts_with_nudge_only(f: &Fixture, _: Option<&TestPg>) {
    for who in ["ana", "bo", "ada", "rev"] {
        let team = f.svc.team(who, now()).await.unwrap();
        assert_eq!(
            team["bots"],
            json!([{"key": "nudge", "always_on": true, "added_at": null, "offered": true}]),
            "{who}"
        );
        assert_eq!(team["handle"], who);
        assert_eq!(team["drafts_this_month"], json!({}));
    }
    // Nobody gains a bot they didn't pick: no rows at all.
    assert!(f.stores.teams.rows().await.unwrap().is_empty());
}
both!(everyone_starts_with_nudge_only);

/// Rows written before migration 0024 (no `persona` column) read as Nudge's.
#[tokio::test]
async fn existing_rows_read_as_nudge_after_the_migration() {
    let Some((f, Some(pg))) = fixture(true).await else {
        return;
    };
    let pool = pg.store.pool();
    sqlx::query(
        "INSERT INTO approvals (id, user_handle, skill, bot, run_id, tool, args, args_hash, status, created_at)
         VALUES ('apr_old', 'ana', NULL, 'pm-ana', 'tsk_old', 'jira.create_issue', '{}'::jsonb, 'h', 'pending', now())",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audit_events (at, run_id, user_handle, skill, bot, tool, outcome, sources, client)
         VALUES (now(), 'tsk_old', 'ana', NULL, 'pm-ana', 'jira.search', 'ok', '{}', 'mcp')",
    )
    .execute(pool)
    .await
    .unwrap();
    let item = f.svc.approval("apr_old").await.unwrap().unwrap();
    assert_eq!(item.persona, None);
    assert_eq!(f.svc.approval_json(&item).await["persona"], Value::Null);
    let old = events(&f).await;
    assert!(old
        .iter()
        .any(|e| e.run_id == "tsk_old" && e.persona.is_none()));
    pg.drop_schema().await;
}

// ----- changing one's own team ------------------------------------------------------------

async fn a_pm_changes_their_own_team(f: &Fixture, _: Option<&TestPg>) {
    let keys: Vec<String> = vec!["quill".into()];
    let team = f
        .svc
        .change_team(Client::Web, "ana", &keys, &[], "onboarding", now())
        .await
        .unwrap();
    assert_eq!(team_keys(&team), ["nudge", "quill"]);
    let quill = &team["bots"][1];
    assert_eq!(quill["offered"], true);
    assert_eq!(quill["always_on"], false);
    assert_eq!(quill["added_via"], "onboarding");
    let added_at = quill["added_at"].clone();
    // Adding again, and removing what isn't there, are no-ops.
    let later = now() + Duration::minutes(5);
    let team = f
        .svc
        .change_team(
            Client::Web,
            "ana",
            &keys,
            &["abacus".into()],
            "picker",
            later,
        )
        .await
        .unwrap();
    assert_eq!(team_keys(&team), ["nudge", "quill"]);
    assert_eq!(team["bots"][1]["added_at"], added_at, "kept as first added");
    // Only Ana's team changed.
    assert_eq!(
        team_keys(&f.svc.team("bo", now()).await.unwrap()),
        ["nudge"]
    );
    let team = f
        .svc
        .change_team(
            Client::Web,
            "ana",
            &[],
            &keys,
            "bots_page",
            later + Duration::minutes(5),
        )
        .await
        .unwrap();
    assert_eq!(team_keys(&team), ["nudge"]);
    // Each change is one event, keys only.
    let details: Vec<(String, Outcome, Client)> = team_changes(f)
        .await
        .into_iter()
        .map(|e| (e.detail.unwrap(), e.outcome, e.client))
        .collect();
    assert_eq!(
        details,
        [
            (
                "add=quill remove=- via=onboarding".to_string(),
                Outcome::Ok,
                Client::Web
            ),
            (
                "add=quill remove=abacus via=picker".to_string(),
                Outcome::Ok,
                Client::Web
            ),
            (
                "add=- remove=quill via=bots_page".to_string(),
                Outcome::Ok,
                Client::Web
            ),
        ]
    );
    for e in team_changes(f).await {
        assert_eq!(e.user, "ana");
        assert!(e.persona.is_none() && e.sources.is_empty() && e.args_hash.is_none());
    }
}
both!(a_pm_changes_their_own_team);

async fn unknown_withdrawn_and_always_on_keys_fail_closed(f: &Fixture, _: Option<&TestPg>) {
    let cases: Vec<(Vec<&str>, Vec<&str>, &str, &str)> = vec![
        (vec!["nudge"], vec![], "bots_page", "always_on"),
        (vec![], vec!["nudge"], "bots_page", "always_on"),
        (vec!["ghost"], vec![], "bots_page", "bot_unavailable"),
        (vec!["nobody-here"], vec![], "bots_page", "bot_unavailable"),
        (
            vec!["quill", "ghost"],
            vec![],
            "bots_page",
            "bot_unavailable",
        ),
        (vec!["Quill"], vec![], "bots_page", "bad_request"),
        (vec!["<img src=x>"], vec![], "bots_page", "bad_request"),
        (vec![], vec![], "bots_page", "bad_request"),
        (vec!["quill"], vec!["quill"], "bots_page", "bad_request"),
        (vec!["quill"], vec![], "seed", "bad_request"),
        (vec!["quill"], vec![], "somewhere", "bad_request"),
    ];
    for (a, r, via, code) in &cases {
        let a: Vec<String> = a.iter().map(|k| k.to_string()).collect();
        let r: Vec<String> = r.iter().map(|k| k.to_string()).collect();
        let err = f
            .svc
            .change_team(Client::Web, "ana", &a, &r, via, now())
            .await
            .unwrap_err();
        assert_eq!(err.code(), *code, "{a:?} {r:?} {via}");
    }
    // Nothing changed, and every refusal was recorded with its fixed note.
    assert_eq!(
        team_keys(&f.svc.team("ana", now()).await.unwrap()),
        ["nudge"]
    );
    let notes: Vec<String> = team_changes(f)
        .await
        .into_iter()
        .map(|e| {
            assert_eq!(e.outcome, Outcome::Denied);
            e.detail.unwrap()
        })
        .collect();
    assert_eq!(notes.len(), cases.len());
    assert_eq!(
        notes.iter().filter(|n| *n == "not_offered").count(),
        3,
        "{notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains('<') || n.contains("Quill")),
        "{notes:?}"
    );
}
both!(unknown_withdrawn_and_always_on_keys_fail_closed);

async fn only_active_people_who_may_ask_have_a_team(f: &Fixture, _: Option<&TestPg>) {
    // An Auditor-only person has no team.
    assert_eq!(
        f.svc.team("aud", now()).await.unwrap_err(),
        TeamRefusal::NotAllowed
    );
    assert_eq!(
        add(f, "aud", &["quill"]).await.unwrap_err(),
        TeamRefusal::NotAllowed
    );
    // Not a user.
    assert_eq!(
        f.svc.team("zed", now()).await.unwrap_err(),
        TeamRefusal::NotAMember
    );
    assert_eq!(
        add(f, "zed", &["quill"]).await.unwrap_err(),
        TeamRefusal::NotAMember
    );
    // Deactivated: rows kept, nothing granted, no change.
    add(f, "bo", &["quill"]).await.unwrap();
    let s = persona_session(f, "bo", "pm-bo", Some("quill"), "tsk_bo")
        .await
        .unwrap();
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::Deactivate {
                handle: "bo".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.svc.team("bo", now()).await.unwrap_err(),
        TeamRefusal::NotAMember
    );
    assert_eq!(
        remove(f, "bo", &["quill"]).await.unwrap_err(),
        TeamRefusal::NotAMember
    );
    assert!(
        f.stores.teams.has("bo", "quill").await.unwrap(),
        "rows kept"
    );
    assert!(f.svc.session(&s.token, now()).await.unwrap().is_none());
    assert!(matches!(
        persona_session(f, "bo", "pm-bo", Some("quill"), "tsk_bo2").await,
        Err(SessionError::NotAMember(_))
    ));
}
both!(only_active_people_who_may_ask_have_a_team);

async fn thirty_changes_an_hour(f: &Fixture, _: Option<&TestPg>) {
    for i in 0..30 {
        let at = now() + Duration::seconds(i);
        let keys = vec!["quill".to_string()];
        let (a, r): (&[String], &[String]) = if i % 2 == 0 {
            (&keys, &[])
        } else {
            (&[], &keys)
        };
        f.svc
            .change_team(Client::Web, "ana", a, r, "bots_page", at)
            .await
            .unwrap();
    }
    let at = now() + Duration::seconds(30);
    let err = f
        .svc
        .change_team(Client::Web, "ana", &["quill".into()], &[], "bots_page", at)
        .await
        .unwrap_err();
    assert_eq!(err, TeamRefusal::RateLimited);
    assert_eq!(err.code(), "rate_limited");
    assert_eq!(
        team_changes(f).await.last().unwrap().detail.as_deref(),
        Some("rate_limited")
    );
    // Another person isn't limited by Ana's changes; an hour later Ana isn't either.
    add(f, "bo", &["quill"]).await.unwrap();
    f.svc
        .change_team(
            Client::Web,
            "ana",
            &["quill".into()],
            &[],
            "bots_page",
            now() + Duration::minutes(61),
        )
        .await
        .unwrap();
}
both!(thirty_changes_an_hour);

fn change(add: &[String]) -> TeamChange<'_> {
    TeamChange {
        handle: "ana",
        add,
        remove: &[],
        via: AddedVia::BotsPage,
        now: now(),
    }
}

async fn at_most_sixteen_rows(f: &Fixture, pg: Option<&TestPg>) {
    let keys: Vec<String> = (0..=MAX_TEAM).map(|i| format!("p{i}")).collect();
    let event =
        pmos_toolserver::team::change_event(Client::Web, "ana", Outcome::Ok, "x".into(), now());
    assert_eq!(
        f.stores
            .teams
            .change(&change(&keys), &event)
            .await
            .unwrap_err(),
        TeamError::Full
    );
    assert!(
        f.stores.teams.team("ana").await.unwrap().is_empty(),
        "nothing changed"
    );
    let rows = f
        .stores
        .teams
        .change(&change(&keys[..MAX_TEAM]), &event)
        .await
        .unwrap();
    assert_eq!(rows.len(), MAX_TEAM);
    assert_eq!(
        f.stores
            .teams
            .change(&change(&keys[MAX_TEAM..]), &event)
            .await
            .unwrap_err(),
        TeamError::Full
    );
    // In the database itself: a 17th row is refused at commit, whatever wrote it.
    if let Some(pg) = pg {
        let mut tx = pg.store.pool().begin().await.unwrap();
        sqlx::query("INSERT INTO user_bots (handle, persona, added_at, added_via) VALUES ('ana', 'p16', now(), 'bots_page')")
            .execute(&mut *tx)
            .await
            .unwrap();
        let err = tx.commit().await.unwrap_err();
        assert!(err.to_string().contains("at most 16 bots"), "{err}");
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_bots WHERE handle = 'ana'")
            .fetch_one(pg.store.pool())
            .await
            .unwrap();
        assert_eq!(n, 16);
        // Nudge is never a row, and a key must be a persona name.
        for bad in ["nudge", "Chief", "a--b"] {
            let r = sqlx::query("INSERT INTO user_bots (handle, persona, added_at, added_via) VALUES ('bo', $1, now(), 'bots_page')")
                .bind(bad)
                .execute(pg.store.pool())
                .await;
            assert!(r.is_err(), "{bad}");
        }
        // A row needs a user.
        let r = sqlx::query("INSERT INTO user_bots (handle, persona, added_at, added_via) VALUES ('zed', 'quill', now(), 'bots_page')")
            .execute(pg.store.pool())
            .await;
        assert!(r.is_err());
    }
}
both!(at_most_sixteen_rows);

/// Slice M1: the limits hold under concurrent requests. Forty change attempts at once by one
/// person: exactly thirty go through, the rest are `rate_limited`; twenty concurrent adds of
/// different bots to an empty team: exactly sixteen rows.
async fn the_limits_hold_under_concurrent_requests(f: &Fixture, _: Option<&TestPg>) {
    let mut attempts = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let svc = Arc::clone(&f.svc);
        attempts.spawn(async move {
            svc.change_team(Client::Web, "ana", &["quill".into()], &[], "picker", now())
                .await
        });
    }
    let results = attempts.join_all().await;
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let limited = results
        .iter()
        .filter(|r| matches!(r, Err(TeamRefusal::RateLimited)))
        .count();
    assert_eq!((ok, limited), (30, 10), "{results:?}");

    let event =
        pmos_toolserver::team::change_event(Client::Web, "bo", Outcome::Ok, "x".into(), now());
    let mut adds = tokio::task::JoinSet::new();
    for i in 0..20 {
        let (teams, event) = (Arc::clone(&f.stores.teams), event.clone());
        adds.spawn(async move {
            let key = vec![format!("c{i}")];
            let change = TeamChange {
                handle: "bo",
                add: &key,
                remove: &[],
                via: AddedVia::BotsPage,
                now: now(),
            };
            teams.change(&change, &event).await
        });
    }
    let results = adds.join_all().await;
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, MAX_TEAM, "{results:?}");
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|e| *e == TeamError::Full));
    assert_eq!(f.stores.teams.team("bo").await.unwrap().len(), MAX_TEAM);
}
both!(the_limits_hold_under_concurrent_requests);

/// Slice M1 (the reviewer: "the 16-row cap trigger can be raced by two concurrent inserts;
/// take a lock in the trigger"): two transactions that each add a row past the 15th, outside
/// the tool server's own lock. The first holds the trigger's per-person lock until it commits;
/// the second waits, then counts the first one's row and is refused. Without the lock the
/// second would count 16, not wait, and both would commit: 17 rows.
#[tokio::test]
async fn the_cap_trigger_serialises_concurrent_writers() {
    let Some((f, Some(pg))) = fixture(true).await else {
        return;
    };
    let keys: Vec<String> = (0..15).map(|i| format!("p{i}")).collect();
    let event =
        pmos_toolserver::team::change_event(Client::Web, "ana", Outcome::Ok, "x".into(), now());
    f.stores.teams.change(&change(&keys), &event).await.unwrap();
    let insert = |key: &'static str| {
        format!("INSERT INTO user_bots (handle, persona, added_at, added_via) VALUES ('ana', '{key}', now(), 'bots_page')")
    };
    let pool = pg.store.pool().clone();
    let mut first = pool.begin().await.unwrap();
    // The trigger is deferred to commit; run it now, so the first holds the lock from here.
    sqlx::query("SET CONSTRAINTS user_bots_cap IMMEDIATE")
        .execute(&mut *first)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(insert("p15")))
        .execute(&mut *first)
        .await
        .unwrap();
    let second = tokio::spawn({
        let pool = pool.clone();
        let sql = insert("p16");
        async move {
            let mut tx = pool.begin().await?;
            sqlx::query("SET CONSTRAINTS user_bots_cap IMMEDIATE")
                .execute(&mut *tx)
                .await?;
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !second.is_finished(),
        "the second writer waits for the first one's lock"
    );
    first.commit().await.unwrap();
    let err = second.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("at most 16 bots"), "{err}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_bots WHERE handle = 'ana'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 16);
    drop(f);
    pg.drop_schema().await;
}

// ----- persona sessions -------------------------------------------------------------------

async fn a_persona_session_gets_the_intersection(f: &Fixture, _: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_q")
        .await
        .unwrap();
    assert_eq!(s.persona.as_deref(), Some("quill"));
    let roster = f.svc.roster();
    let skills: Vec<_> = f.svc.catalog().skills().cloned().collect();
    let base = role_tools(&roster.role("pm-assistant").unwrap(), &skills);
    let own = role_tools(&roster.role("quill-role").unwrap(), &skills);
    let mut expected: BTreeSet<String> = own.intersection(&base).cloned().collect();
    // `never` for this bot (pm-ana) and in Quill's role.
    expected.remove("slack.send_message");
    assert_eq!(tool_names(f, &s), expected);
    assert!(
        own.contains("slack.send_message"),
        "the role's skills declare it"
    );
    let err = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": "C0PMTEAM", "text": "Fake."}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Forbidden(m) if m.contains("as quill")),
        "never stays never"
    );
    // Nudge: the personal bot's own tools, and the session names no persona.
    let n = persona_session(f, "ana", "pm-ana", Some("nudge"), "tsk_n")
        .await
        .unwrap();
    assert_eq!(n.persona, None);
    let mut nudge = base.clone();
    nudge.remove("slack.send_message");
    assert_eq!(tool_names(f, &n), nudge);
    assert!(
        tool_names(f, &s).is_subset(&tool_names(f, &n)),
        "never more than the personal bot"
    );
    // Stored and read back with its persona.
    let back = f.svc.session(&s.token, now()).await.unwrap().unwrap();
    assert_eq!(back.persona.as_deref(), Some("quill"));
    assert_eq!(serde_json::to_value(&back).unwrap()["persona"], "quill");
}
both!(a_persona_session_gets_the_intersection);

/// Content that would fail `pmos lint`, put in force anyway: a persona role with a tool the
/// personal role lacks, a write the personal role sets to `never` set to `approve`, and a
/// higher data class. The tool server still allows only the intersection, at the stricter
/// setting and the lower data class.
async fn the_tool_server_holds_the_ceiling_without_the_lint(f: &Fixture, _: Option<&TestPg>) {
    let roster = f.svc.roster();
    let mut base: ContentRole = roster.role("pm-assistant").unwrap().as_ref().clone();
    base.actions
        .insert("jira.create_issue".into(), ActionSetting::Never);
    base.data_class = Sensitivity::Internal;
    let mut rogue_role: ContentRole = roster.role("quill-role").unwrap().as_ref().clone();
    rogue_role.name = "rogue-role".into();
    rogue_role.skills = vec!["page-update".into(), "jira-draft".into(), "ask".into()];
    rogue_role.data_class = Sensitivity::Restricted;
    rogue_role.actions.clear();
    rogue_role
        .actions
        .insert("jira.create_issue".into(), ActionSetting::Approve);
    rogue_role
        .actions
        .insert("confluence.update_page".into(), ActionSetting::Approve);
    let mut rogue: Persona = roster.persona("quill").unwrap().as_ref().clone();
    rogue.name = "rogue".into();
    rogue.display_name = "Rogue".into();
    rogue.role = "rogue-role".into();
    f.svc.replace_roster(
        roster
            .as_ref()
            .clone()
            .with_unlinted_role(base)
            .with_unlinted_persona(rogue, Some(rogue_role)),
    );
    add(f, "ana", &["rogue"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("rogue"), "tsk_rogue")
        .await
        .unwrap();
    let tools = tool_names(f, &s);
    assert!(
        !tools.contains("confluence.update_page"),
        "not in the personal role: {tools:?}"
    );
    assert!(
        !tools.contains("jira.create_issue"),
        "`never` in the personal role: {tools:?}"
    );
    assert!(tools.contains("jira.search"));
    let err = f
        .svc
        .call(
            &s,
            "confluence_update_page",
            json!({"page_id": "9001", "title": "Fake", "body": "Fake."}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotInRole(_)), "{err:?}");
    let err = f
        .svc
        .call(&s, "jira_create_issue", json!({"project": "PM", "issue_type": "Story", "summary": "Fake", "description": "Fake."}), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err:?}");
    // The lower data class: the personal role's `internal`, not the persona's `restricted`.
    let err = f
        .svc
        .call(
            &s,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(m) if m.contains("at most internal")));
    assert!(f
        .stores
        .approvals
        .list("ana", None)
        .await
        .unwrap()
        .is_empty());
}
both!(the_tool_server_holds_the_ceiling_without_the_lint);

async fn sessions_are_refused_off_the_team_or_the_catalog(f: &Fixture, _: Option<&TestPg>) {
    let err = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_1")
        .await
        .unwrap_err();
    assert_eq!(err.persona_code(), Some("persona_not_on_team"));
    assert_eq!(
        err.to_string(),
        "Quill isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge."
    );
    let err = persona_session(f, "ana", "pm-ana", Some("ghost"), "tsk_1")
        .await
        .unwrap_err();
    assert_eq!(err.persona_code(), Some("persona_unavailable"));
    assert!(err.to_string().starts_with("Ghost isn't offered"), "{err}");
    for unknown in ["nobody-here", "<b>x</b>", ""] {
        let err = persona_session(f, "ana", "pm-ana", Some(unknown), "tsk_1")
            .await
            .unwrap_err();
        assert_eq!(
            err.persona_code(),
            Some("persona_unavailable"),
            "{unknown:?}"
        );
        assert!(
            !err.to_string().contains(unknown) || unknown.is_empty(),
            "never echoed: {err}"
        );
    }
    add(f, "ana", &["quill"]).await.unwrap();
    // Only on a personal bot of the personal role.
    let err = persona_session(f, "ana", "scout", Some("quill"), "tsk_1")
        .await
        .unwrap_err();
    assert_eq!(err.persona_code(), Some("persona_needs_personal_bot"));
    // Someone else's personal bot: today's rule first.
    assert!(matches!(
        persona_session(f, "bo", "pm-ana", Some("quill"), "tsk_1").await,
        Err(SessionError::NotOwner { .. })
    ));
    // Bo hasn't picked Quill, though Ana has.
    let err = persona_session(f, "bo", "pm-bo", Some("quill"), "tsk_1")
        .await
        .unwrap_err();
    assert_eq!(err.persona_code(), Some("persona_not_on_team"));
    assert!(persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_1")
        .await
        .is_ok());
}
both!(sessions_are_refused_off_the_team_or_the_catalog);

async fn removal_lets_a_task_finish_and_withdrawal_ends_it(f: &Fixture, _: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_q")
        .await
        .unwrap();
    // Removed from the team mid-task: the session the PM started finishes.
    remove(f, "ana", &["quill"]).await.unwrap();
    assert!(f.svc.session(&s.token, now()).await.unwrap().is_some());
    f.svc
        .call(&s, "jira_search", json!({"jql": "project = PM"}), now())
        .await
        .unwrap();
    // A new task with it fails at claim.
    assert_eq!(
        persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_q2")
            .await
            .unwrap_err()
            .persona_code(),
        Some("persona_not_on_team")
    );
    // Withdrawn from the catalog: the live session ends on its next call.
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_q3")
        .await
        .unwrap();
    withdraw(f, "quill");
    assert!(f.svc.session(&s.token, now()).await.unwrap().is_none());
    assert!(f.svc.list_tools(&s).is_empty());
    // The row is kept, inert, shown as withdrawn, and may only be removed.
    let team = f.svc.team("ana", now()).await.unwrap();
    assert_eq!(team["bots"][1]["key"], "quill");
    assert_eq!(team["bots"][1]["offered"], false);
    assert!(!f.svc.persona_catalog()["personas"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "quill"));
    assert_eq!(
        add(f, "ana", &["quill"]).await.unwrap_err().code(),
        "bot_unavailable"
    );
    assert_eq!(
        persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_q4")
            .await
            .unwrap_err()
            .persona_code(),
        Some("persona_unavailable")
    );
    assert_eq!(
        team_keys(&remove(f, "ana", &["quill"]).await.unwrap()),
        ["nudge"]
    );
}
both!(removal_lets_a_task_finish_and_withdrawal_ends_it);

// ----- attribution ------------------------------------------------------------------------

async fn jira_draft(f: &Fixture, s: &Session) -> String {
    let out = f
        .svc
        .call(
            s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "Fake: SSO grace period", "description": "Fake description."}),
            now(),
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

async fn a_draft_names_its_persona_and_outlives_it(f: &Fixture, _: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_attr")
        .await
        .unwrap();
    f.svc
        .call(&s, "jira_search", json!({"jql": "project = PM"}), now())
        .await
        .unwrap();
    let id = jira_draft(f, &s).await;
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.persona.as_deref(), Some("quill"));
    assert_eq!(item.bot.as_deref(), Some("pm-ana"));
    assert_eq!(f.svc.approval_json(&item).await["persona"], "quill");
    assert_eq!(
        f.svc.approvals_json(std::slice::from_ref(&item)).await[0]["persona"],
        "quill"
    );
    // Every call of the session is attributed.
    let calls: Vec<AuditEvent> = events(f)
        .await
        .into_iter()
        .filter(|e| e.run_id == "tsk_attr")
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(
        calls.iter().all(|e| e.persona.as_deref() == Some("quill")),
        "{calls:?}"
    );
    // A Nudge draft is `null`.
    let n = persona_session(f, "ana", "pm-ana", None, "tsk_nudge")
        .await
        .unwrap();
    let nid = jira_draft(f, &n).await;
    let nitem = f.svc.approval(&nid).await.unwrap().unwrap();
    assert_eq!(nitem.persona, None);
    assert_eq!(f.svc.approval_json(&nitem).await["persona"], Value::Null);
    // The draft counts for this month, per persona.
    let team = f.svc.team("ana", now()).await.unwrap();
    assert_eq!(team["drafts_this_month"], json!({"nudge": 1, "quill": 1}));
    // Quill leaves Ana's team: the draft stays Ana's and decidable (Quill is still offered).
    remove(f, "ana", &["quill"]).await.unwrap();
    assert_eq!(f.svc.held(&item).await.unwrap(), None);
    assert!(f.svc.approval_json(&item).await.get("held").is_none());
    // Quill is withdrawn from the catalog: its pending draft is held (the reviewer's change,
    // slice M1): not approvable until Quill is offered again or an Admin releases it.
    withdraw(f, "quill");
    assert_eq!(f.svc.held(&item).await.unwrap().as_deref(), Some("Quill"));
    assert_eq!(f.svc.approval_json(&item).await["held"], true);
    assert_eq!(
        f.svc.approvals_json(std::slice::from_ref(&item)).await[0]["held"],
        true
    );
    let err = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DecisionError::Held(name) if name == "Quill"),
        "{err}"
    );
    assert_eq!(
        f.svc.approval(&id).await.unwrap().unwrap().status,
        Status::Pending,
        "nothing was decided"
    );
    let refused = events(f)
        .await
        .into_iter()
        .rev()
        .find(|e| e.run_id == "tsk_attr" && e.outcome == Outcome::Denied)
        .unwrap();
    assert_eq!(
        refused.detail.as_deref(),
        Some(pmos_toolserver::service::HELD_DECISION)
    );
    assert_eq!(refused.persona.as_deref(), Some("quill"));
    // An Admin releases it; Ana then approves it under the personal bot's rules.
    f.svc
        .release_held_draft(Client::Web, "ada", &id, now())
        .await
        .unwrap();
    assert_eq!(f.svc.held(&item).await.unwrap(), None);
    let decided = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(decided.status, Status::Approved);
    assert_eq!(
        decided.persona.as_deref(),
        Some("quill"),
        "still attributed"
    );
    let decision = events(f)
        .await
        .into_iter()
        .rev()
        .find(|e| e.run_id == "tsk_attr" && e.outcome == Outcome::Approved)
        .unwrap();
    assert_eq!(decision.persona.as_deref(), Some("quill"));
    assert_eq!(decision.client, Client::Orchestrator);
}
both!(a_draft_names_its_persona_and_outlives_it);

/// Slice M1 (the reviewer's change to §6, "Removal"): a withdrawn bot's pending drafts are
/// held. The owner may reject one; approving or editing it is refused until the bot is offered
/// again or an Admin releases it. Only an active Admin releases, only a held draft, recorded.
async fn a_withdrawn_bots_drafts_are_held(f: &Fixture, _: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_held")
        .await
        .unwrap();
    let first = jira_draft(f, &s).await;
    let second = jira_draft(f, &s).await;
    let nudge = persona_session(f, "ana", "pm-ana", None, "tsk_held_n")
        .await
        .unwrap();
    let nudge_draft = jira_draft(f, &nudge).await;
    let offered = f.svc.roster();
    withdraw(f, "quill");

    // An edit is refused like an approval; nothing changes.
    let edited = json!({"project": "PM", "issue_type": "Story", "summary": "Fake: edited", "description": "Fake."});
    let err = f
        .svc
        .decide(&first, "ana", Decision::Edit, Some(edited), now())
        .await
        .unwrap_err();
    assert!(matches!(err, DecisionError::Held(_)), "{err:?}");
    assert!(f.jira.writes().is_empty(), "nothing was written");
    // Rejecting stays open to the owner.
    let rejected = f
        .svc
        .decide(&second, "ana", Decision::Reject, None, now())
        .await
        .unwrap();
    assert_eq!(rejected.status, Status::Rejected);
    // Nudge's drafts are never held.
    let item = f.svc.approval(&nudge_draft).await.unwrap().unwrap();
    assert_eq!(f.svc.held(&item).await.unwrap(), None);

    // Only an active Admin releases, and only a held draft; each attempt is recorded.
    use pmos_toolserver::service::{ReleaseRefusal, RELEASE_HOLD};
    for who in ["ana", "aud", "rev", "zed"] {
        assert_eq!(
            f.svc
                .release_held_draft(Client::Web, who, &first, now())
                .await
                .unwrap_err(),
            ReleaseRefusal::Forbidden,
            "{who}"
        );
    }
    for (id, why) in [
        (nudge_draft.as_str(), ReleaseRefusal::NotHeld),
        (second.as_str(), ReleaseRefusal::NotHeld),
        ("apr_nope", ReleaseRefusal::NotFound),
    ] {
        assert_eq!(
            f.svc
                .release_held_draft(Client::Web, "ada", id, now())
                .await
                .unwrap_err(),
            why,
            "{id}"
        );
    }
    let releases: Vec<AuditEvent> = events(f)
        .await
        .into_iter()
        .filter(|e| e.tool == RELEASE_HOLD)
        .collect();
    assert_eq!(releases.len(), 7);
    assert!(releases.iter().all(|e| e.outcome == Outcome::Denied));
    assert_eq!(releases[0].detail.as_deref(), Some("forbidden"));
    assert_eq!(releases[0].persona.as_deref(), Some("quill"));
    assert_eq!(releases[0].output_ref.as_deref(), Some(first.as_str()));

    // Offered again: no longer held, and approvable.
    f.svc.replace_roster(offered.as_ref().clone());
    let item = f.svc.approval(&first).await.unwrap().unwrap();
    assert_eq!(f.svc.held(&item).await.unwrap(), None);
    assert_eq!(
        f.svc
            .release_held_draft(Client::Web, "ada", &first, now())
            .await
            .unwrap_err(),
        ReleaseRefusal::NotHeld,
        "nothing to release while it is offered"
    );
    let decided = f
        .svc
        .decide(&first, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(decided.status, Status::Approved);

    // A release is once per draft.
    let third = jira_draft(f, &s).await;
    withdraw(f, "quill");
    let released = f
        .svc
        .release_held_draft(Client::Web, "ada", &third, now())
        .await
        .unwrap();
    assert_eq!(released.status, Status::Pending, "the owner still decides");
    assert_eq!(
        f.svc
            .release_held_draft(Client::Web, "ada", &third, now())
            .await
            .unwrap_err(),
        ReleaseRefusal::NotHeld
    );
    let ok = events(f)
        .await
        .into_iter()
        .rev()
        .find(|e| e.tool == RELEASE_HOLD && e.outcome == Outcome::Ok)
        .unwrap();
    assert_eq!(ok.user, "ada");
    assert_eq!(ok.persona.as_deref(), Some("quill"));
}
both!(a_withdrawn_bots_drafts_are_held);

/// Slice M1: a persona role lists every write it may make; the tool server refuses one it
/// doesn't list, even with content that bypassed the lint (no fallback to `approve`).
async fn an_unlisted_write_is_refused(f: &Fixture, _: Option<&TestPg>) {
    let roster = f.svc.roster();
    let mut role: ContentRole = roster.role("quill-role").unwrap().as_ref().clone();
    role.actions.remove("jira.create_issue");
    f.svc
        .replace_roster(roster.as_ref().clone().with_unlinted_role(role));
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_unlisted")
        .await
        .unwrap();
    let err = f
        .svc
        .call(&s, "jira_create_issue", json!({"project": "PM", "issue_type": "Story", "summary": "Fake", "description": "Fake."}), now())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Forbidden(m) if m.contains("doesn't list it")),
        "{err:?}"
    );
    assert!(f
        .stores
        .approvals
        .list("ana", None)
        .await
        .unwrap()
        .is_empty());
    // Listed writes still draft, and Nudge (no persona) keeps the personal role's rules.
    let out = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": "C0PMTEAM", "thread_ts": "1731601200.000600", "text": "Fake."}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "pending_approval");
    let n = persona_session(f, "ana", "pm-ana", None, "tsk_unlisted_n")
        .await
        .unwrap();
    jira_draft(f, &n).await;
}
both!(an_unlisted_write_is_refused);

/// Slice M1 and the owner's decision of 2026-11-21: every page Echo drafts carries the line
/// "Draft for internal review. Not approved for external use.", put there by the tool server
/// when the draft is recorded and again when it is written, whatever the model or an edit did.
async fn echos_pages_carry_the_internal_review_line(f: &Fixture, _: Option<&TestPg>) {
    use pmos_contract::personas::INTERNAL_REVIEW_NOTICE as NOTICE;
    add(f, "ana", &["echo"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("echo"), "tsk_echo")
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "PM", "title": "Fake: 4.2 release notes", "body": "## New\nFake."}),
            now(),
        )
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap().to_string();
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.persona.as_deref(), Some("echo"));
    assert_eq!(item.args["body"], format!("{NOTICE}\n\n## New\nFake."));
    // Written once even when the model already put it in.
    let out = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "PM", "title": "Fake: battlecard", "body": format!("{NOTICE}\n\nFake.")}),
            now(),
        )
        .await
        .unwrap();
    let again = f
        .svc
        .approval(out["approval_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        again.args["body"].as_str().unwrap().matches(NOTICE).count(),
        1
    );
    // A Jira issue is internal work: unchanged.
    let issue = jira_draft(f, &s).await;
    let issue = f.svc.approval(&issue).await.unwrap().unwrap();
    assert!(!issue.args.to_string().contains(NOTICE));
    // An edit that drops the line: what is written still starts with it.
    let edited = json!({"space": "PM", "title": "Fake: 4.2 release notes", "body": "Edited without the line."});
    f.svc
        .decide(&id, "ana", Decision::Edit, Some(edited), now())
        .await
        .unwrap();
    let written = f.confluence.writes();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(
        written[0].args["body"],
        format!("{NOTICE}\n\nEdited without the line.")
    );
    // Nudge's pages carry no notice.
    let n = persona_session(f, "ana", "pm-ana", None, "tsk_echo_n")
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &n,
            "confluence_create_draft",
            json!({"space": "PM", "title": "Fake: PRD", "body": "Fake."}),
            now(),
        )
        .await
        .unwrap();
    let nudge = f
        .svc
        .approval(out["approval_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(nudge.args["body"], "Fake.");
}
both!(echos_pages_carry_the_internal_review_line);

async fn no_route_writes_a_drafts_persona(f: &Fixture, pg: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_w")
        .await
        .unwrap();
    let id = jira_draft(f, &s).await;
    // A saved edit and an edit decision change the arguments, never the persona.
    let edited = json!({"project": "PM", "issue_type": "Story", "summary": "Fake: edited", "description": "Fake."});
    let saved = f
        .svc
        .save_draft(
            Client::Orchestrator,
            &id,
            "ana",
            edited.clone(),
            None,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(saved.persona.as_deref(), Some("quill"));
    let mut again = edited.clone();
    again["summary"] = json!("Fake: edited twice");
    let decided = f
        .svc
        .decide(&id, "ana", Decision::Edit, Some(again), now())
        .await
        .unwrap();
    assert_eq!(decided.persona.as_deref(), Some("quill"));
    // The database refuses any update of it.
    if let Some(pg) = pg {
        for to in [Some("abacus"), None] {
            let r = sqlx::query("UPDATE approvals SET persona = $1 WHERE id = $2")
                .bind(to)
                .bind(&id)
                .execute(pg.store.pool())
                .await;
            assert!(r.is_err(), "{to:?}");
        }
        let still: Option<String> =
            sqlx::query_scalar("SELECT persona FROM approvals WHERE id = $1")
                .bind(&id)
                .fetch_one(pg.store.pool())
                .await
                .unwrap();
        assert_eq!(still.as_deref(), Some("quill"));
    }
}
both!(no_route_writes_a_drafts_persona);

/// The sensitivity, audience, owner-only and reviewer rules, run under a persona: unchanged.
async fn restricted_and_approval_rules_hold_under_a_persona(f: &Fixture, _: Option<&TestPg>) {
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::AssignReviewer {
                reviewer: "rev".into(),
                pms: BTreeSet::from(["ana".to_string()]),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_r")
        .await
        .unwrap();
    f.svc
        .call(
            &s,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": SLACK_WITH_OUTSIDER, "thread_ts": "1731601200.000600", "text": "Fake: under NDA."}),
            now(),
        )
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap().to_string();
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.sources_sensitivity, Some(Sensitivity::Restricted));
    assert_eq!(item.persona.as_deref(), Some("quill"));
    // Restricted sources never reach outsiders, persona or not.
    let err = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert!(matches!(err, DecisionError::AudienceBlocked(_)), "{err:?}");
    assert_eq!(
        f.svc.approval(&id).await.unwrap().unwrap().status,
        Status::Pending
    );
    assert!(f.slack.writes().is_empty());
    // Only the owner decides.
    assert!(f
        .svc
        .decide(&id, "bo", Decision::Approve, None, now())
        .await
        .is_err());
    // A reviewer sees a restricted draft as metadata only, still attributed.
    let view = f.svc.review_item("rev", &id, now()).await.unwrap();
    assert_eq!(view["restricted"], true);
    assert_eq!(view["persona"], "quill");
    assert!(view.get("args").is_none());
}
both!(restricted_and_approval_rules_hold_under_a_persona);

/// Events with a persona, and team changes, go with the one retention job like every other
/// event: no new delete path.
async fn persona_events_go_with_the_existing_purge(f: &Fixture, _: Option<&TestPg>) {
    let old = Utc::now() - Duration::days(100);
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_old")
        .await
        .unwrap();
    let mut e = pmos_toolserver::team::change_event(
        Client::Web,
        "ana",
        Outcome::Ok,
        "add=quill remove=- via=bots_page".into(),
        old,
    );
    f.stores.audit.record(e.clone()).await.unwrap();
    e.tool = "jira.search".into();
    e.run_id = s.run_id.clone();
    e.persona = Some("quill".into());
    f.stores.audit.record(e).await.unwrap();
    let Some(Purged::Ran(batch)) = f.stores.audit.purge(90, 1000, Utc::now()).await.unwrap() else {
        panic!("both stores purge");
    };
    assert!(batch.deleted >= 2, "{batch:?}");
    let left = events(f).await;
    assert!(!left.iter().any(|e| e.at < Utc::now() - Duration::days(90)));
    // The team itself is current state: untouched by the purge.
    assert!(f.stores.teams.has("ana", "quill").await.unwrap());
}
both!(persona_events_go_with_the_existing_purge);

// ----- the admin's read and the eval seed -------------------------------------------------

async fn the_admin_read_is_keys_handles_and_counts(f: &Fixture, _: Option<&TestPg>) {
    add(f, "ana", &["quill"]).await.unwrap();
    add(f, "bo", &["quill", "abacus"]).await.unwrap();
    // A row naming a persona no longer in the catalog.
    let event =
        pmos_toolserver::team::change_event(Client::Web, "bo", Outcome::Ok, "x".into(), now());
    let gone = vec!["gone-bot".to_string()];
    f.stores
        .teams
        .change(
            &TeamChange {
                handle: "bo",
                add: &gone,
                remove: &[],
                via: AddedVia::BotsPage,
                now: now(),
            },
            &event,
        )
        .await
        .unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_a")
        .await
        .unwrap();
    jira_draft(f, &s).await;
    let v = f.svc.admin_personas(Utc::now()).await.unwrap();
    let find = |key: &str| {
        v["personas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == key)
            .cloned()
            .unwrap()
    };
    let quill = find("quill");
    assert_eq!(quill["users"], 2);
    assert_eq!(quill["handles"], json!(["ana", "bo"]));
    assert_eq!(quill["offered"], true);
    assert_eq!(find("ghost")["offered"], false);
    assert_eq!(find("ghost")["users"], 0);
    assert_eq!(find("nudge")["users"], Value::Null);
    assert_eq!(
        v["unknown"],
        json!([{"key": "gone-bot", "users": 1, "handles": ["bo"], "drafts_30d": 0, "held": []}])
    );
    // Drafts in the last 30 days (the draft is dated by the fixture's clock).
    let recent = (Utc::now() - now()).num_days() < 30;
    assert_eq!(quill["drafts_30d"], if recent { 1 } else { 0 });
    // Never a prompt, a summary or an example.
    for p in v["personas"].as_array().unwrap() {
        for field in ["summary", "examples", "title", "can_do"] {
            assert!(p.get(field).is_none(), "{field}");
        }
        assert_eq!(p["name_clashes"], json!([]), "{p}");
    }
    // Slice M1: a persona named like a PM OS user (display name or handle) is shown to the
    // admin; people's names aren't in the content, so the lint can't refuse it.
    f.stores
        .users
        .apply(
            &Actor::bot("ada"),
            Op::CreateUser {
                user: pmos_toolserver::users::NewUser {
                    handle: "kim".into(),
                    display_name: "Quill".into(),
                    slack_id: Some("U0KIM0001".into()),
                    roles: [pmos_toolserver::users::UserRole::Pm].into(),
                    ..Default::default()
                },
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let v = f.svc.admin_personas(Utc::now()).await.unwrap();
    let quill = v["personas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "quill")
        .unwrap();
    assert_eq!(quill["name_clashes"], json!(["kim"]));
}
both!(the_admin_read_is_keys_handles_and_counts);

async fn the_eval_seed_gives_evals_every_offered_persona(f: &Fixture, _: Option<&TestPg>) {
    let n = pmos_toolserver::team::seed_evals(
        f.stores.teams.as_ref(),
        f.stores.users.as_ref(),
        &f.svc.roster(),
        now(),
    )
    .await
    .unwrap();
    let rows = f.stores.teams.team("evals").await.unwrap();
    assert_eq!(rows.len(), n);
    let keys: Vec<&str> = rows.iter().map(|r| r.persona.as_str()).collect();
    assert!(
        keys.contains(&"quill") && keys.contains(&"abacus"),
        "{keys:?}"
    );
    assert!(!keys.contains(&"ghost") && !keys.contains(&"nudge"));
    assert!(rows.iter().all(|r| r.added_via == AddedVia::Seed));
    // Nobody else.
    assert!(f.stores.teams.team("ana").await.unwrap().is_empty());
}
both!(the_eval_seed_gives_evals_every_offered_persona);

// ----- the routes ---------------------------------------------------------------------------

struct Server {
    base: String,
    http: reqwest::Client,
    cancel: CancellationToken,
    f: Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

enum Assert {
    Require,
    /// `PMOS_ASSERTION_MODE=off` with `PMOS_ASSERTION_INSECURE_OFF=1`, the web key set.
    InsecureOff,
}

async fn start(assert: Assert) -> Server {
    let (f, _) = fixture(false).await.unwrap();
    let keys = format!("web:toolserver:w1:{WEB_KEY}");
    let keys = parse_keys(&keys, "the tool server", |iss, aud| {
        aud == AUDIENCE && ISSUERS.contains(&iss)
    })
    .unwrap();
    let assertions = match assert {
        Assert::Require => Assertions::new(Mode::Require, keys).unwrap(),
        Assert::InsecureOff => {
            let lookup = move |k: &str| match k {
                "PMOS_ASSERTION_MODE" => Some("off".to_string()),
                "PMOS_ASSERTION_INSECURE_OFF" => Some("1".to_string()),
                "PMOS_ASSERTION_KEYS" => Some(format!("web:toolserver:w1:{WEB_KEY}")),
                _ => None,
            };
            Assertions::from_lookup(lookup, &[("web", "PMOS_WEB_TOKEN")]).unwrap()
        }
    };
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: Some(CONSOLE_TOKEN.into()),
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        None,
        PersonClients {
            entra: None,
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
    Server {
        base,
        http: reqwest::Client::new(),
        cancel,
        f,
    }
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[Role::Pm], "s-test").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

impl Server {
    /// Send `body` (raw bytes) with `token`, signed for `signer` when given.
    async fn send(
        &self,
        method: Method,
        target: &str,
        token: &str,
        body: Option<&[u8]>,
        signer: Option<&str>,
    ) -> (u16, Value) {
        let bytes = body.unwrap_or_default().to_vec();
        let mut req = self
            .http
            .request(method.clone(), format!("{}{target}", self.base))
            .bearer_auth(token);
        if body.is_some() {
            req = req
                .header("content-type", "application/json")
                .body(bytes.clone());
        }
        if let Some(who) = signer {
            req = req.header(
                pmos_assertion::HEADER,
                sign(who, method.as_str(), target, &bytes),
            );
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn json(
        &self,
        method: Method,
        target: &str,
        token: &str,
        body: Value,
        signer: Option<&str>,
    ) -> (u16, Value) {
        let bytes = serde_json::to_vec(&body).unwrap();
        self.send(method, target, token, Some(&bytes), signer).await
    }
}

fn body(add: &[&str], remove: &[&str]) -> Value {
    json!({"add": add, "remove": remove, "via": "bots_page"})
}

#[tokio::test]
async fn each_route_answers_only_its_tokens() {
    let srv = start(Assert::Require).await;
    // The catalog: the runtime, the Slack bot and pmos-web, the same projection for each.
    let mut seen = Vec::new();
    for token in [SERVICE_TOKEN, BOT_TOKEN, WEB_TOKEN] {
        let (s, v) = srv
            .send(Method::GET, "/internal/personas", token, None, None)
            .await;
        assert_eq!(s, 200, "{v}");
        seen.push(v);
    }
    assert!(seen.windows(2).all(|w| w[0] == w[1]));
    let (s, _) = srv
        .send(Method::GET, "/internal/personas", CONSOLE_TOKEN, None, None)
        .await;
    assert_eq!(s, 403);
    let (s, _) = srv
        .send(Method::GET, "/internal/personas", UNKNOWN_TOKEN, None, None)
        .await;
    assert_eq!(s, 401);
    // A PM's team and the admin read: pmos-web only.
    let put = serde_json::to_vec(&body(&["quill"], &[])).unwrap();
    for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
        let (s, v) = srv
            .send(Method::GET, "/internal/team/ana", token, None, Some("ana"))
            .await;
        assert_eq!(s, 403, "{v}");
        let (s, v) = srv
            .send(
                Method::PUT,
                "/internal/team/ana",
                token,
                Some(&put),
                Some("ana"),
            )
            .await;
        assert_eq!(s, 403, "{v}");
        let (s, v) = srv
            .send(
                Method::GET,
                "/internal/admin/personas?user=ada",
                token,
                None,
                Some("ada"),
            )
            .await;
        assert_eq!(s, 403, "{v}");
    }
    for path in ["/internal/team/ana", "/internal/admin/personas?user=ada"] {
        let (s, _) = srv.send(Method::GET, path, UNKNOWN_TOKEN, None, None).await;
        assert_eq!(s, 401, "{path}");
    }
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/team/ana",
            WEB_TOKEN,
            None,
            Some("ana"),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .send(
            Method::PUT,
            "/internal/team/ana",
            WEB_TOKEN,
            Some(&put),
            Some("ana"),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(team_keys(&v), ["nudge", "quill"]);
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/admin/personas?user=ada",
            WEB_TOKEN,
            None,
            Some("ada"),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // Sessions stay the orchestrator's: pmos-web and the console can't start a persona's.
    let body = serde_json::to_vec(
        &json!({"user": "ana", "bot": "pm-ana", "persona": "quill", "run_id": "tsk_x"}),
    )
    .unwrap();
    for token in [WEB_TOKEN, CONSOLE_TOKEN, BOT_TOKEN] {
        let (s, v) = srv
            .send(Method::POST, "/internal/sessions", token, Some(&body), None)
            .await;
        assert_eq!(s, 403, "{v}");
    }
}

#[tokio::test]
async fn the_team_is_the_signed_persons_own() {
    let srv = start(Assert::Require).await;
    let put = serde_json::to_vec(&body(&["quill"], &[])).unwrap();
    // Another person's team: 403 forbidden, whoever signs.
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/team/ana",
            WEB_TOKEN,
            None,
            Some("bo"),
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("forbidden")), "{v}");
    let (s, v) = srv
        .send(
            Method::PUT,
            "/internal/team/ana",
            WEB_TOKEN,
            Some(&put),
            Some("ada"),
        )
        .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (403, Some("forbidden")),
        "an admin neither: {v}"
    );
    // No assertion, or one for another request: 401 bad_assertion.
    let (s, v) = srv
        .send(Method::GET, "/internal/team/ana", WEB_TOKEN, None, None)
        .await;
    assert_eq!((s, v["code"].as_str()), (401, Some("bad_assertion")));
    let signed_for_get = sign("ana", "GET", "/internal/team/ana", b"");
    let res = srv
        .http
        .put(format!("{}/internal/team/ana", srv.base))
        .bearer_auth(WEB_TOKEN)
        .header(pmos_assertion::HEADER, signed_for_get)
        .header("content-type", "application/json")
        .body(put.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 401);
    assert_eq!(
        team_keys(&srv.f.svc.team("ana", now()).await.unwrap()),
        ["nudge"]
    );
    // Refusals before the person is verified are recorded as anonymous, fixed notes only.
    let denied: Vec<(String, String)> = team_changes(&srv.f)
        .await
        .into_iter()
        .map(|e| (e.user, e.detail.unwrap()))
        .collect();
    assert_eq!(
        denied,
        [
            ("anonymous".to_string(), "forbidden".to_string()),
            ("anonymous".to_string(), "bad_assertion".to_string()),
        ]
    );
}

/// Slice M1: `POST /internal/admin/personas/drafts/{id}/release?user=<admin>`, `pmos-web`'s
/// token, the admin's own assertion, the tool server in `require`.
#[tokio::test]
async fn an_admin_releases_a_held_draft_over_http() {
    let srv = start(Assert::Require).await;
    let f = &srv.f;
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_http_held")
        .await
        .unwrap();
    let id = jira_draft(f, &s).await;
    withdraw(f, "quill");
    let path = |who: &str| format!("/internal/admin/personas/drafts/{id}/release?user={who}");
    let (st, v) = srv
        .send(Method::POST, &path("ana"), WEB_TOKEN, None, Some("ana"))
        .await;
    assert_eq!((st, v["code"].as_str()), (403, Some("forbidden")), "{v}");
    let (st, _) = srv
        .send(Method::POST, &path("ada"), WEB_TOKEN, None, None)
        .await;
    assert_eq!(st, 401, "unsigned");
    let (st, _) = srv
        .send(Method::POST, &path("ada"), WEB_TOKEN, None, Some("ana"))
        .await;
    assert_ne!(st, 200, "signed for someone else");
    for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
        let (st, _) = srv
            .send(Method::POST, &path("ada"), token, None, None)
            .await;
        assert_eq!(st, 403);
    }
    assert!(f
        .svc
        .held(&f.svc.approval(&id).await.unwrap().unwrap())
        .await
        .unwrap()
        .is_some());
    let (st, v) = srv
        .send(Method::POST, &path("ada"), WEB_TOKEN, None, Some("ada"))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["id"], id.as_str());
    assert!(v.get("held").is_none(), "{v}");
    let (st, v) = srv
        .send(Method::POST, &path("ada"), WEB_TOKEN, None, Some("ada"))
        .await;
    assert_eq!((st, v["code"].as_str()), (409, Some("not_held")));
    // Without assertion `require`, nothing is released.
    let srv = start(Assert::InsecureOff).await;
    let (st, v) = srv
        .send(Method::POST, &path("ada"), WEB_TOKEN, None, Some("ada"))
        .await;
    assert_eq!(
        (st, v["code"].as_str()),
        (503, Some("assertion_required")),
        "{v}"
    );
}

#[tokio::test]
async fn changes_need_assertion_require() {
    let srv = start(Assert::InsecureOff).await;
    let put = serde_json::to_vec(&body(&["quill"], &[])).unwrap();
    let (s, v) = srv
        .send(
            Method::PUT,
            "/internal/team/ana",
            WEB_TOKEN,
            Some(&put),
            Some("ana"),
        )
        .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (503, Some("assertion_required")),
        "{v}"
    );
    assert!(srv.f.stores.teams.team("ana").await.unwrap().is_empty());
    // Reads still need the person's assertion, in every mode.
    let (s, _) = srv
        .send(Method::GET, "/internal/team/ana", WEB_TOKEN, None, None)
        .await;
    assert_eq!(s, 401);
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/team/ana",
            WEB_TOKEN,
            None,
            Some("ana"),
        )
        .await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test]
async fn refusals_over_http_carry_the_spec_codes() {
    let srv = start(Assert::Require).await;
    let cases: Vec<(&str, Value, u16, &str)> = vec![
        ("aud", body(&["quill"], &[]), 403, "not_allowed"),
        ("ana", body(&["ghost"], &[]), 409, "bot_unavailable"),
        ("ana", body(&["nobody-here"], &[]), 409, "bot_unavailable"),
        ("ana", body(&["nudge"], &[]), 409, "always_on"),
        ("ana", body(&[], &["nudge"]), 409, "always_on"),
        ("ana", body(&[], &[]), 400, "bad_request"),
        (
            "ana",
            json!({"add": ["quill"], "via": "bots_page", "routines": "nudge"}),
            400,
            "bad_request",
        ),
        (
            "ana",
            json!({"add": "quill", "via": "bots_page"}),
            400,
            "bad_request",
        ),
    ];
    for (who, body, status, code) in cases {
        let path = format!("/internal/team/{who}");
        let (s, v) = srv
            .json(Method::PUT, &path, WEB_TOKEN, body.clone(), Some(who))
            .await;
        assert_eq!((s, v["code"].as_str()), (status, Some(code)), "{body}: {v}");
    }
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/team/aud",
            WEB_TOKEN,
            None,
            Some("aud"),
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("not_allowed")));
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/team/zed",
            WEB_TOKEN,
            None,
            Some("zed"),
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("not_a_member")));
    // The 31st change in an hour.
    for i in 0..30 {
        let body = if i % 2 == 0 {
            body(&["quill"], &[])
        } else {
            body(&[], &["quill"])
        };
        let (s, v) = srv
            .json(
                Method::PUT,
                "/internal/team/bo",
                WEB_TOKEN,
                body,
                Some("bo"),
            )
            .await;
        assert_eq!(s, 200, "{i}: {v}");
    }
    let (s, v) = srv
        .json(
            Method::PUT,
            "/internal/team/bo",
            WEB_TOKEN,
            body(&["quill"], &[]),
            Some("bo"),
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (429, Some("rate_limited")));
}

#[tokio::test]
async fn sessions_over_http_name_the_persona_or_say_why_not() {
    let srv = start(Assert::Require).await;
    let session = |persona: Value| json!({"user": "ana", "bot": "pm-ana", "persona": persona, "run_id": "tsk_h"});
    let (s, v) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            session(json!("quill")),
            None,
        )
        .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (403, Some("persona_not_on_team")),
        "{v}"
    );
    assert_eq!(
        v["error"],
        "Quill isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge."
    );
    let (s, v) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            session(json!("ghost")),
            None,
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("persona_unavailable")));
    let body = json!({"user": "ana", "bot": "scout", "persona": "quill", "run_id": "tsk_h"});
    srv.f
        .svc
        .change_team(Client::Web, "ana", &["quill".into()], &[], "picker", now())
        .await
        .unwrap();
    let (s, v) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            body,
            None,
        )
        .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (403, Some("persona_needs_personal_bot"))
    );
    let body = json!({"user": "ana", "skill": "ask", "persona": "quill", "run_id": "tsk_h"});
    let (s, _) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            body,
            None,
        )
        .await;
    assert_eq!(s, 400);
    let (s, v) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            session(json!("quill")),
            None,
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["session"]["persona"], "quill");
    assert_eq!(
        v["persona"],
        json!({"key": "quill", "display_name": "Quill", "title": "Fake Quill title", "role": "quill-role"})
    );
    let tools: Vec<&str> = v["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"jira_create_issue") && !tools.contains(&"slack_send_message"),
        "{tools:?}"
    );
    for nudge in [json!("nudge"), Value::Null] {
        let (s, v) = srv
            .json(
                Method::POST,
                "/internal/sessions",
                SERVICE_TOKEN,
                session(nudge),
                None,
            )
            .await;
        assert_eq!(s, 201);
        assert_eq!(v["persona"], Value::Null);
        assert!(v["session"].get("persona").is_none());
    }
}

/// The reviewer's change to the session body: a field this server doesn't know is refused,
/// never dropped, so a caller that sends one (a persona-like field a later version adds, or a
/// typo of `persona`) gets no session at all rather than one without it.
#[tokio::test]
async fn a_session_body_with_an_unknown_field_is_refused() {
    let srv = start(Assert::Require).await;
    srv.f
        .svc
        .change_team(Client::Web, "ana", &["quill".into()], &[], "picker", now())
        .await
        .unwrap();
    for extra in [
        json!({"personna": "quill"}),
        json!({"persona_key": "quill"}),
        json!({"level": "staff"}),
    ] {
        let mut body = json!({"user": "ana", "bot": "pm-ana", "run_id": "tsk_unknown"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (s, v) = srv
            .json(
                Method::POST,
                "/internal/sessions",
                SERVICE_TOKEN,
                body,
                None,
            )
            .await;
        assert_eq!(s, 422, "{extra}: {v}");
        assert!(v.get("token").is_none(), "{v}");
    }
    // The same body without the unknown field starts a session.
    let (s, _) = srv
        .json(
            Method::POST,
            "/internal/sessions",
            SERVICE_TOKEN,
            json!({"user": "ana", "bot": "pm-ana", "run_id": "tsk_unknown", "persona": "quill"}),
            None,
        )
        .await;
    assert_eq!(s, 201);
}

#[tokio::test]
async fn the_admin_read_is_for_admins_and_auditors() {
    let srv = start(Assert::Require).await;
    for who in ["ada", "aud"] {
        let path = format!("/internal/admin/personas?user={who}");
        let (s, v) = srv
            .send(Method::GET, &path, WEB_TOKEN, None, Some(who))
            .await;
        assert_eq!(s, 200, "{who}: {v}");
        assert!(
            v["personas"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["key"] == "ghost"),
            "withdrawn ones too"
        );
    }
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/admin/personas?user=ana",
            WEB_TOKEN,
            None,
            Some("ana"),
        )
        .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("forbidden")));
    // Someone else's name on the read: the assertion must match.
    let (s, _) = srv
        .send(
            Method::GET,
            "/internal/admin/personas?user=ada",
            WEB_TOKEN,
            None,
            Some("ana"),
        )
        .await;
    assert_eq!(s, 401);
    let reads: Vec<(String, Outcome)> = events(&srv.f)
        .await
        .into_iter()
        .filter(|e| e.tool == "admin.personas_read")
        .map(|e| (e.user, e.outcome))
        .collect();
    assert_eq!(
        reads,
        [
            ("ada".to_string(), Outcome::Ok),
            ("aud".to_string(), Outcome::Ok),
            ("ana".to_string(), Outcome::Denied),
        ]
    );
}

/// A team change's event reaches the admin audit view with its fixed detail; a persona
/// session's calls with their persona.
#[tokio::test]
async fn the_audit_view_shows_team_changes_and_personas() {
    let srv = start(Assert::Require).await;
    let (s, _) = srv
        .json(
            Method::PUT,
            "/internal/team/ana",
            WEB_TOKEN,
            body(&["quill"], &[]),
            Some("ana"),
        )
        .await;
    assert_eq!(s, 200);
    let sess = persona_session(&srv.f, "ana", "pm-ana", Some("quill"), "tsk_v")
        .await
        .unwrap();
    srv.f
        .svc
        .call(&sess, "jira_search", json!({"jql": "project = PM"}), now())
        .await
        .unwrap();
    let (s, v) = srv
        .send(
            Method::GET,
            "/internal/admin/audit?user=ada",
            WEB_TOKEN,
            None,
            Some("ada"),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let rows = v["events"].as_array().unwrap();
    let team = rows.iter().find(|e| e["action"] == "team.change").unwrap();
    assert_eq!(team["detail"], "add=quill remove=- via=bots_page");
    let call = rows.iter().find(|e| e["task_id"] == "tsk_v").unwrap();
    assert_eq!(call["persona"], "quill");
}

/// Slice A1: the admin view lists each held draft (metadata only) under its bot, and drops it
/// once it is released or decided; the audit search filters by persona and never matches
/// Nudge's events (they carry none).
async fn the_admin_view_lists_held_drafts_and_the_audit_filters_by_bot(
    f: &Fixture,
    _: Option<&TestPg>,
) {
    add(f, "ana", &["quill"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_a1")
        .await
        .unwrap();
    let first = jira_draft(f, &s).await;
    let second = jira_draft(f, &s).await;
    let nudge = persona_session(f, "ana", "pm-ana", None, "tsk_a1_n")
        .await
        .unwrap();
    jira_draft(f, &nudge).await;
    let quill = |v: &Value| {
        v["personas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "quill")
            .cloned()
            .unwrap()
    };
    // Offered: nothing is held.
    let v = f.svc.admin_personas(Utc::now()).await.unwrap();
    assert_eq!(quill(&v)["held"], json!([]));
    assert!(quill(&v)["mascot"].is_string(), "{}", quill(&v));
    assert_eq!(v["held_total"], 0);

    withdraw(f, "quill");
    let v = f.svc.admin_personas(Utc::now()).await.unwrap();
    let held = quill(&v)["held"].as_array().unwrap().clone();
    let ids: Vec<&str> = held.iter().map(|h| h["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 2, "{v}");
    assert!(ids.contains(&first.as_str()) && ids.contains(&second.as_str()));
    for h in &held {
        let mut keys: Vec<&str> = h.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["created_at", "id", "tool", "user"],
            "metadata only: {h}"
        );
        assert_eq!(h["user"], "ana");
        assert_eq!(h["tool"], "jira.create_issue");
    }
    assert_eq!(v["held_total"], 2);
    for p in v["personas"].as_array().unwrap() {
        if p["key"] != "quill" {
            assert_eq!(p["held"], json!([]), "{p}");
        }
    }
    // Released or decided: no longer listed.
    f.svc
        .release_held_draft(Client::Web, "ada", &first, now())
        .await
        .unwrap();
    f.svc
        .decide(&second, "ana", Decision::Reject, None, now())
        .await
        .unwrap();
    let v = f.svc.admin_personas(Utc::now()).await.unwrap();
    assert_eq!(quill(&v)["held"], json!([]));
    assert_eq!(v["held_total"], 0);

    // The audit search by persona: Quill's events only, never Nudge's.
    let search = |persona: &str| AuditQuery {
        limit: 500,
        persona: Some(persona.into()),
        ..Default::default()
    };
    let found = f
        .stores
        .audit
        .search(&search("quill"))
        .await
        .unwrap()
        .unwrap();
    assert!(!found.is_empty());
    assert!(found
        .iter()
        .all(|e| e.event.persona.as_deref() == Some("quill")));
    let all = events(f).await;
    assert!(all
        .iter()
        .any(|e| e.persona.is_none() && e.bot.as_deref() == Some("pm-ana")));
    let none = f
        .stores
        .audit
        .search(&search("nudge"))
        .await
        .unwrap()
        .unwrap();
    assert!(none.is_empty(), "Nudge's events carry no persona");
}
both!(the_admin_view_lists_held_drafts_and_the_audit_filters_by_bot);

// ----- the security gate's marketplace findings on 8e8f8fe (gate-fix-4) -------------------

/// Echo's line is stored on the item: a saved edit that drops it, or moves it to the bottom,
/// and the approved write both start with it, even once Echo's file no longer says it.
async fn the_notice_is_stored_on_the_item_and_heads_every_edit(f: &Fixture, _: Option<&TestPg>) {
    use pmos_contract::personas::INTERNAL_REVIEW_NOTICE as NOTICE;
    add(f, "ana", &["echo"]).await.unwrap();
    let s = persona_session(f, "ana", "pm-ana", Some("echo"), "tsk_notice")
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "PM", "title": "Fake: brief", "body": format!("Fake.\n\n{NOTICE}")}),
            now(),
        )
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap().to_string();
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert!(
        item.args["body"].as_str().unwrap().starts_with(NOTICE),
        "a copy at the bottom doesn't count: {}",
        item.args["body"]
    );
    assert_eq!(item.draft_notice.as_deref(), Some(NOTICE), "stored");
    // The item names its bot for a card, from the catalog here (never a client's copy).
    let view = f.svc.approval_json(&item).await;
    assert_eq!(
        (&view["persona_name"], &view["persona_preview"]),
        (&json!("Echo"), &json!(true))
    );
    assert!(view.get("draft_notice").is_none(), "never served");
    // Echo's file no longer has the line (as if gone or changed): the item's copy holds.
    let roster = f.svc.roster();
    let mut echo: Persona = roster.persona("echo").unwrap().as_ref().clone();
    echo.draft_notice = None;
    f.svc
        .replace_roster(roster.as_ref().clone().with_unlinted_persona(echo, None));
    let saved = f
        .svc
        .save_draft(
            Client::Web,
            &id,
            "ana",
            json!({"space": "PM", "title": "Fake: brief", "body": "Edited, line dropped."}),
            None,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        saved.args["body"],
        format!("{NOTICE}\n\nEdited, line dropped.")
    );
    let shown = saved.args_hash.chars().take(16).collect::<String>();
    f.svc
        .decide_verdict(
            Client::Web,
            &id,
            "ana",
            pmos_toolserver::approvals::Verdict::new(Decision::Approve, None).shown(Some(shown)),
            now(),
        )
        .await
        .unwrap();
    let written = f.confluence.writes();
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].args["body"]
        .as_str()
        .unwrap()
        .starts_with(NOTICE));
    // Only page tools store it.
    let issue = jira_draft(f, &s).await;
    let issue = f.svc.approval(&issue).await.unwrap().unwrap();
    assert_eq!(issue.draft_notice, None);

    // A bot named like a colleague is marked as a bot where a card names it.
    let roster = f.svc.roster();
    let mut echo: Persona = roster.persona("echo").unwrap().as_ref().clone();
    echo.display_name = "Ana".into();
    f.svc
        .replace_roster(roster.as_ref().clone().with_unlinted_persona(echo, None));
    let view = f.svc.approval_json(&issue).await;
    assert_eq!(view["persona_name"], "Ana (bot)", "{view}");
}
both!(the_notice_is_stored_on_the_item_and_heads_every_edit);

/// An Admin can't release their own held draft; a held draft can't be saved; and the store
/// refuses a held approve under the decision's own lock, unless released.
async fn holds_are_checked_with_the_decision_and_the_save(f: &Fixture, _: Option<&TestPg>) {
    use pmos_toolserver::approvals::{DecideError, Verdict};
    use pmos_toolserver::service::{ReleaseRefusal, RELEASE_HOLD};
    add(f, "ada", &["quill"]).await.unwrap();
    add(f, "ana", &["quill"]).await.unwrap();
    let own = persona_session(f, "ada", "pm-ada", Some("quill"), "tsk_own")
        .await
        .unwrap();
    let ada_draft = jira_draft(f, &own).await;
    let s = persona_session(f, "ana", "pm-ana", Some("quill"), "tsk_hold2")
        .await
        .unwrap();
    let first = jira_draft(f, &s).await;
    let second = jira_draft(f, &s).await;
    let view = f
        .svc
        .approval_json(&f.svc.approval(&first).await.unwrap().unwrap())
        .await;
    assert_eq!(
        (&view["persona_name"], &view["persona_preview"]),
        (&json!("Quill"), &json!(false)),
        "Quill claims a benchmark"
    );
    withdraw(f, "quill");
    let view = f
        .svc
        .approval_json(&f.svc.approval(&first).await.unwrap().unwrap())
        .await;
    assert_eq!(view["persona_name"], Value::Null, "no longer offered");

    // Ada is an Admin, but the draft is her own: another Admin releases it.
    let err = f
        .svc
        .release_held_draft(Client::Web, "ada", &ada_draft, now())
        .await
        .unwrap_err();
    assert_eq!(err, ReleaseRefusal::OwnDraft);
    assert_eq!(err.code(), "own_draft");
    let last = events(f)
        .await
        .into_iter()
        .rev()
        .find(|e| e.tool == RELEASE_HOLD)
        .unwrap();
    assert_eq!(
        (last.outcome, last.detail.as_deref()),
        (Outcome::Denied, Some("own_draft"))
    );

    // Saving an edit of a held draft is refused, and nothing changes.
    let before = f.svc.approval(&first).await.unwrap().unwrap();
    let err = f
        .svc
        .save_draft(
            Client::Web,
            &first,
            "ana",
            json!({"project": "PM", "issue_type": "Story", "summary": "Fake: edited", "description": "Fake."}),
            None,
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DecisionError::Held(_)), "{err:?}");
    let after = f.svc.approval(&first).await.unwrap().unwrap();
    assert_eq!(after.args_hash, before.args_hash);

    // The store itself refuses an approve of a held item when given what is offered, under
    // the decision's lock; a reject is never held; a released item is approvable.
    let offered = f.svc.offered_keys();
    assert!(!offered.contains("quill"));
    let err = f
        .stores
        .approvals
        .decide_with(
            &first,
            "ana",
            Verdict::new(Decision::Approve, None).holding(offered.clone()),
            now(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, DecideError::Held);
    let err = f
        .stores
        .approvals
        .save_draft_held(&first, "ana", json!({}), None, Some(&offered), now())
        .await
        .unwrap_err();
    assert_eq!(err, DecideError::Held);
    f.stores
        .approvals
        .release_hold(&first, "ada", now())
        .await
        .unwrap();
    let ok = f
        .stores
        .approvals
        .decide_with(
            &first,
            "ana",
            Verdict::new(Decision::Approve, None).holding(offered.clone()),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status, Status::Executing);
    let rejected = f
        .stores
        .approvals
        .decide_with(
            &second,
            "ana",
            Verdict::new(Decision::Reject, None).holding(offered),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, Status::Rejected);
}
both!(holds_are_checked_with_the_decision_and_the_save);

/// Over the hourly limit, refused team changes write one audit row an hour, not one each.
async fn rate_limited_changes_are_recorded_once_an_hour(f: &Fixture, _: Option<&TestPg>) {
    for i in 0..30 {
        let at = now() + Duration::seconds(i);
        let keys = vec!["quill".to_string()];
        let (a, r): (&[String], &[String]) = if i % 2 == 0 {
            (&keys, &[])
        } else {
            (&[], &keys)
        };
        f.svc
            .change_team(Client::Web, "ana", a, r, "bots_page", at)
            .await
            .unwrap();
    }
    for i in 0..5 {
        let at = now() + Duration::seconds(30 + i);
        let err = f
            .svc
            .change_team(Client::Web, "ana", &["quill".into()], &[], "bots_page", at)
            .await
            .unwrap_err();
        assert_eq!(err, TeamRefusal::RateLimited);
    }
    let limited = team_changes(f)
        .await
        .into_iter()
        .filter(|e| e.detail.as_deref() == Some("rate_limited"))
        .count();
    assert_eq!(limited, 1, "the first refusal of the hour only");
    // Other refusals are still each recorded.
    for _ in 0..2 {
        f.svc
            .change_team(
                Client::Web,
                "bo",
                &["ghost".into()],
                &[],
                "bots_page",
                now(),
            )
            .await
            .unwrap_err();
    }
    let bo = team_changes(f)
        .await
        .into_iter()
        .filter(|e| e.user == "bo" && e.outcome == Outcome::Denied)
        .count();
    assert_eq!(bo, 2);
}
both!(rate_limited_changes_are_recorded_once_an_hour);

/// The admin bots view, which the audit log's page also reads for its filter, is recorded
/// once per viewer per ten minutes when allowed; every refusal is recorded.
#[tokio::test]
async fn admin_bots_reads_are_recorded_once_per_window() {
    let srv = start(Assert::Require).await;
    for _ in 0..3 {
        let (s, _) = srv
            .send(
                Method::GET,
                "/internal/admin/personas?user=ada",
                WEB_TOKEN,
                None,
                Some("ada"),
            )
            .await;
        assert_eq!(s, 200);
        let (s, _) = srv
            .send(
                Method::GET,
                "/internal/admin/personas?user=ana",
                WEB_TOKEN,
                None,
                Some("ana"),
            )
            .await;
        assert_eq!(s, 403);
    }
    let reads: Vec<(String, Outcome)> = events(&srv.f)
        .await
        .into_iter()
        .filter(|e| e.tool == "admin.personas_read")
        .map(|e| (e.user, e.outcome))
        .collect();
    assert_eq!(
        reads
            .iter()
            .filter(|(u, o)| u == "ada" && *o == Outcome::Ok)
            .count(),
        1
    );
    assert_eq!(
        reads
            .iter()
            .filter(|(u, o)| u == "ana" && *o == Outcome::Denied)
            .count(),
        3
    );
}
