//! `jira.rollup` (`docs/staff-principal-spec.md` §3) against a mock of Jira's enhanced JQL
//! search: pages are followed by `nextPageToken`, the counts by project, epic and fix
//! version, `none` and `other`, links that cross groups or leave the query, the `max_issues`
//! cap and the time budget, the PM's token on every page, and a team bot's scope. Then the
//! fake connector's fixtures through the tool server.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use chrono::NaiveDate;
use common::{now, write_content};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::atlassian::Jira;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::rollup::FIELDS;
use pmos_toolserver::connectors::{Caller, Connector, ConnectorError, Item, TeamBot};
use pmos_toolserver::oauth::{AccessToken, TokenError, TokenSource};
use pmos_toolserver::vault::{Provider, Secret};
use pmos_toolserver::ToolService;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

// ----- the mock ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    query: HashMap<String, String>,
    auth: Option<String>,
}

#[derive(Default)]
struct Mock {
    issues: Mutex<Vec<Value>>,
    delay: Mutex<Duration>,
    log: Mutex<Vec<Req>>,
}

impl Mock {
    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

/// `GET /ex/jira/cloud-1/rest/api/3/search/jql`: pages of `maxResults`, the token is the
/// offset of the next page.
async fn search(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    mock.log.lock().unwrap().push(Req {
        query: query.clone(),
        auth: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
    });
    let delay = *mock.delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let issues = mock.issues.lock().unwrap().clone();
    let start: usize = query
        .get("nextPageToken")
        .and_then(|t| t.strip_prefix("p"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let size: usize = query["maxResults"].parse().unwrap();
    let end = (start + size).min(issues.len());
    let mut body = json!({"issues": issues[start.min(end)..end], "isLast": end >= issues.len()});
    if end < issues.len() {
        body["nextPageToken"] = json!(format!("p{end}"));
    }
    Json(body)
}

struct Tokens;

#[async_trait]
impl TokenSource for Tokens {
    async fn token(&self, user: &str, _provider: Provider) -> Result<AccessToken, TokenError> {
        Ok(AccessToken {
            token: Secret::new(format!("at-{user}")),
            cloud_id: Some("cloud-1".into()),
            site_url: Some("https://example.atlassian.net".into()),
        })
    }
}

struct Live {
    base: String,
    mock: Arc<Mock>,
    jira: Jira,
    cancel: CancellationToken,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn live(issues: Vec<Value>) -> Live {
    let mock = Arc::new(Mock::default());
    *mock.issues.lock().unwrap() = issues;
    let app = Router::new()
        .route("/ex/jira/cloud-1/rest/api/3/search/jql", get(search))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    let jira = Jira::new(Arc::new(Tokens), base.clone()).with_today(today());
    Live {
        base,
        mock,
        jira,
        cancel,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 15).unwrap()
}

fn ana() -> Caller<'static> {
    Caller::new("ana", "run-1")
}

/// An issue as the search returns it with the rollup's fields.
struct Issue {
    key: String,
    category: &'static str,
    epic: Option<&'static str>,
    versions: Vec<&'static str>,
    due: Option<&'static str>,
    assignee: Option<&'static str>,
    links: Vec<Value>,
}

fn issue(key: &str, category: &'static str) -> Issue {
    Issue {
        key: key.to_string(),
        category,
        epic: None,
        versions: Vec::new(),
        due: None,
        assignee: Some("Ana"),
        links: Vec::new(),
    }
}

impl Issue {
    fn json(&self) -> Value {
        let project = self.key.rsplit_once('-').unwrap().0;
        let name = match self.category {
            "new" => "To Do",
            "indeterminate" => "In Progress",
            _ => "Done",
        };
        let mut fields = json!({
            "status": {"name": name, "statusCategory": {"key": self.category}},
            "project": {"key": project, "name": format!("Project {project}")},
            "issuetype": {"name": "Story", "hierarchyLevel": 0},
            "fixVersions": self.versions.iter().map(|v| json!({"name": v})).collect::<Vec<_>>(),
            "issuelinks": self.links,
            "assignee": self.assignee.map(|a| json!({"accountId": format!("acc-{a}"), "displayName": a})),
        });
        if let Some(epic) = self.epic {
            fields["parent"] = json!({"key": epic, "fields": {"summary": format!("Epic {epic}"),
                "issuetype": {"name": "Epic", "hierarchyLevel": 1}}});
        }
        if let Some(due) = self.due {
            fields["duedate"] = json!(due);
        }
        json!({"key": self.key, "fields": fields})
    }
}

fn blocks(outward: bool, other: &str, category: &str) -> Value {
    let side = if outward {
        "outwardIssue"
    } else {
        "inwardIssue"
    };
    json!({
        "type": {"name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
        side: {"key": other, "fields": {"status": {"statusCategory": {"key": category}}}},
    })
}

async fn rollup(l: &Live, caller: Caller<'_>, args: Value) -> Item {
    let mut items = l.jira.read(caller, "rollup", &args).await.unwrap();
    assert_eq!(items.len(), 1);
    items.remove(0)
}

fn group<'a>(content: &'a Value, key: &str) -> &'a Value {
    content["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == key)
        .unwrap_or_else(|| panic!("no group {key} in {content}"))
}

// ----- the live connector -----------------------------------------------------------------

#[tokio::test]
async fn pages_are_followed_with_the_pms_token_and_counted_by_project() {
    // 250 issues: 150 in AAA (50 of each state), 100 in BBB, one overdue and one unassigned.
    let mut issues = Vec::new();
    for i in 0..150 {
        let cat = ["new", "indeterminate", "done"][i % 3];
        let mut x = issue(&format!("AAA-{i}"), cat);
        if i == 0 {
            x.due = Some("2026-11-01");
        }
        if i == 2 {
            x.due = Some("2026-11-01"); // done: not overdue
        }
        if i == 1 {
            x.assignee = None;
        }
        issues.push(x.json());
    }
    for i in 0..100 {
        let mut x = issue(&format!("BBB-{i}"), "new");
        x.due = Some("2026-12-01"); // not yet due
        issues.push(x.json());
    }
    let l = live(issues).await;
    let item = rollup(&l, ana(), json!({"jql": "project in (AAA, BBB)"})).await;
    assert_eq!(item.title, "Jira rollup: 250 issues by project");
    assert_eq!(item.source.system, "jira");
    assert!(item
        .source
        .url
        .starts_with("https://example.atlassian.net/issues/?jql="));
    let c = &item.content;
    assert_eq!(c["counted"], 250);
    assert_eq!(c["truncated"], false);
    assert_eq!(c["reason"], Value::Null);
    assert_eq!(c["group_by"], "project");
    assert_eq!(
        c["groups"],
        json!([
            {"key": "AAA", "name": "Project AAA", "total": 150, "todo": 50, "in_progress": 50,
             "done": 50, "overdue": 1, "unassigned": 1},
            {"key": "BBB", "name": "Project BBB", "total": 100, "todo": 100, "in_progress": 0,
             "done": 0, "overdue": 0, "unassigned": 0},
        ])
    );
    assert_eq!(c["links"], json!([]));
    assert_eq!(c["links_truncated"], false);

    let reqs = l.mock.requests();
    assert_eq!(reqs.len(), 3, "three pages of at most 100");
    for (i, r) in reqs.iter().enumerate() {
        assert_eq!(
            r.auth.as_deref(),
            Some("Bearer at-ana"),
            "page {i} as the PM"
        );
        assert_eq!(r.query["jql"], "project in (AAA, BBB)");
        assert_eq!(r.query["fields"], FIELDS);
        assert_eq!(r.query["maxResults"], "100");
    }
    assert!(!reqs[0].query.contains_key("nextPageToken"));
    assert_eq!(reqs[1].query["nextPageToken"], "p100");
    assert_eq!(reqs[2].query["nextPageToken"], "p200");
}

#[tokio::test]
async fn by_epic_by_fix_version_and_none() {
    let mut a = issue("PM-1", "new");
    a.epic = Some("PM-100");
    a.versions = vec!["3.1", "3.2"];
    let mut b = issue("PM-2", "done");
    b.epic = Some("PM-100");
    b.versions = vec!["3.2"];
    let c = issue("PM-3", "indeterminate");
    let l = live(vec![a.json(), b.json(), c.json()]).await;

    let item = rollup(
        &l,
        ana(),
        json!({"jql": "project = PM", "group_by": "epic"}),
    )
    .await;
    let e = &item.content;
    assert_eq!(e["counted"], 3);
    assert_eq!(
        group(e, "PM-100"),
        &json!({"key": "PM-100", "name": "Epic PM-100", "total": 2, "todo": 1, "in_progress": 0,
                "done": 1, "overdue": 0, "unassigned": 0})
    );
    assert_eq!(group(e, "none")["total"], 1);
    assert_eq!(group(e, "none")["in_progress"], 1);

    // An issue counts in each of its versions; `counted` is still the issues.
    let item = rollup(
        &l,
        ana(),
        json!({"jql": "project = PM", "group_by": "fix_version"}),
    )
    .await;
    let v = &item.content;
    assert_eq!(v["counted"], 3);
    assert_eq!(item.title, "Jira rollup: 3 issues by fix_version");
    let keys: Vec<&str> = v["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec!["3.2", "3.1", "none"],
        "largest first, none after"
    );
    assert_eq!(group(v, "3.2")["total"], 2);
    assert_eq!(group(v, "3.1")["total"], 1);

    let item = rollup(
        &l,
        ana(),
        json!({"jql": "project = PM", "group_by": "status"}),
    )
    .await;
    assert_eq!(group(&item.content, "To Do")["todo"], 1);
    let item = rollup(
        &l,
        ana(),
        json!({"jql": "project = PM", "group_by": "assignee"}),
    )
    .await;
    assert_eq!(group(&item.content, "acc-Ana")["name"], "Ana");
    assert_eq!(group(&item.content, "acc-Ana")["total"], 3);
}

#[tokio::test]
async fn more_than_fifty_groups_are_summed_into_other() {
    // 60 projects of 2 issues each, except P59 with 4: 50 are listed by name, largest first
    // and then by key.
    let mut issues = Vec::new();
    for p in 0..60 {
        for i in 0..(if p == 59 { 4 } else { 2 }) {
            issues.push(issue(&format!("P{p:02}-{i}"), "new").json());
        }
    }
    let total = issues.len();
    let l = live(issues).await;
    let item = rollup(
        &l,
        ana(),
        json!({"jql": "order by key", "max_issues": 1000}),
    )
    .await;
    let c = &item.content;
    assert_eq!(c["counted"], total);
    let groups = c["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 51);
    assert_eq!(
        (groups[0]["key"].as_str(), groups[0]["total"].as_u64()),
        (Some("P59"), Some(4))
    );
    assert_eq!(groups[1]["key"], "P00");
    assert_eq!(groups[49]["key"], "P48");
    let other = &groups[50];
    assert_eq!(other["key"], "other");
    // P49 … P58, 2 issues each.
    assert_eq!(other["total"], 20);
    assert_eq!(other["todo"], 20);
    assert_eq!(other["name"], "10 more groups");
}

#[tokio::test]
async fn links_that_cross_groups_or_leave_the_query_once_each() {
    // PM-1 blocks CLD-1 (both in the query: seen from both sides, reported once); PM-2 blocks
    // OPS-9 (outside the query); PM-3 blocks PM-1 (same project: not reported by project).
    let mut pm1 = issue("PM-1", "new");
    pm1.epic = Some("PM-100");
    pm1.links = vec![
        blocks(true, "CLD-1", "indeterminate"),
        blocks(false, "PM-3", "new"),
    ];
    let mut pm2 = issue("PM-2", "new");
    pm2.epic = Some("PM-100");
    pm2.links = vec![blocks(true, "OPS-9", "done")];
    let mut pm3 = issue("PM-3", "new");
    pm3.epic = Some("PM-200");
    pm3.links = vec![blocks(true, "PM-1", "new")];
    let mut cld1 = issue("CLD-1", "indeterminate");
    cld1.links = vec![blocks(false, "PM-1", "new")];
    let l = live(vec![pm1.json(), pm2.json(), pm3.json(), cld1.json()]).await;

    let item = rollup(&l, ana(), json!({"jql": "x"})).await;
    assert_eq!(
        item.content["links"],
        json!([
            {"from": "PM-1", "from_group": "PM", "relation": "blocks", "to": "CLD-1",
             "to_group": "CLD", "to_state": "in_progress"},
            {"from": "PM-2", "from_group": "PM", "relation": "blocks", "to": "OPS-9",
             "to_group": "OPS", "to_state": "done"},
        ])
    );

    // By epic, PM-3 → PM-1 crosses epics, and the issue outside the query has no group.
    let item = rollup(&l, ana(), json!({"jql": "x", "group_by": "epic"})).await;
    assert_eq!(
        item.content["links"],
        json!([
            {"from": "PM-1", "from_group": "PM-100", "relation": "blocks", "to": "CLD-1",
             "to_group": "none", "to_state": "in_progress"},
            {"from": "PM-2", "from_group": "PM-100", "relation": "blocks", "to": "OPS-9",
             "to_group": null, "to_state": "done"},
            {"from": "PM-3", "from_group": "PM-200", "relation": "blocks", "to": "PM-1",
             "to_group": "PM-100", "to_state": "todo"},
        ])
    );
}

#[tokio::test]
async fn at_most_200_links() {
    let mut hub = issue("PM-1", "new");
    hub.links = (0..210)
        .map(|i| blocks(true, &format!("OPS-{i}"), "new"))
        .collect();
    let l = live(vec![hub.json()]).await;
    let item = rollup(&l, ana(), json!({"jql": "x"})).await;
    assert_eq!(item.content["links"].as_array().unwrap().len(), 200);
    assert_eq!(item.content["links_truncated"], true);
}

#[tokio::test]
async fn it_stops_at_max_issues() {
    let issues: Vec<Value> = (0..250)
        .map(|i| issue(&format!("PM-{i}"), "new").json())
        .collect();
    let l = live(issues).await;
    let item = rollup(&l, ana(), json!({"jql": "x", "max_issues": 150})).await;
    assert_eq!(item.content["counted"], 150);
    assert_eq!(item.content["truncated"], true);
    assert_eq!(item.content["reason"], "cap");
    let sizes: Vec<String> = l
        .mock
        .requests()
        .iter()
        .map(|r| r.query["maxResults"].clone())
        .collect();
    assert_eq!(sizes, vec!["100", "50"], "no more than it needs");

    // The default is 500; exactly reaching the cap with nothing left isn't truncated.
    let issues: Vec<Value> = (0..200)
        .map(|i| issue(&format!("PM-{i}"), "new").json())
        .collect();
    let l = live(issues).await;
    let item = rollup(&l, ana(), json!({"jql": "x", "max_issues": 200})).await;
    assert_eq!(item.content["counted"], 200);
    assert_eq!(item.content["truncated"], false);
    assert_eq!(item.content["reason"], Value::Null);
    let item = rollup(&l, ana(), json!({"jql": "x"})).await;
    assert_eq!(item.content["counted"], 200);
}

#[tokio::test]
async fn it_stops_when_time_runs_out() {
    let issues: Vec<Value> = (0..400)
        .map(|i| issue(&format!("PM-{i}"), "new").json())
        .collect();
    let mut l = live(issues).await;
    *l.mock.delay.lock().unwrap() = Duration::from_millis(300);
    // Half a second in all: the first page arrives, the second doesn't in time.
    l.jira = Jira::new(Arc::new(Tokens), l.base.clone())
        .with_today(today())
        .with_rollup_budget(Duration::from_millis(500));
    let item = rollup(&l, ana(), json!({"jql": "x"})).await;
    assert_eq!(item.content["counted"], 100);
    assert_eq!(item.content["truncated"], true);
    assert_eq!(item.content["reason"], "time");
    assert_eq!(l.mock.requests().len(), 2);
}

#[tokio::test]
async fn a_team_bots_scope_wraps_the_query_and_it_reads_as_the_bot() {
    let l = live(vec![issue("PM-1", "new").json()]).await;
    let reads = pmos_contract::bots::Reads {
        jira_projects: vec!["PM".into()],
        confluence_spaces: Vec::new(),
        slack_channels: Vec::new(),
    };
    let scout = Caller::new("ana", "tsk_1").for_team_bot(TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    rollup(&l, scout, json!({"jql": "status != Done ORDER BY key"})).await;
    let r = l.mock.requests().pop().unwrap();
    assert_eq!(
        r.query["jql"],
        r#"project in ("PM") AND (status != Done) ORDER BY key"#
    );
    assert_eq!(r.auth.as_deref(), Some("Bearer at-bots.scout"));
    // A query that would escape the scope is refused before anything is sent.
    let err = l
        .jira
        .read(scout, "rollup", &json!({"jql": "x) OR (project = HR"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectorError::BadArgs(_)), "{err:?}");
    assert_eq!(l.mock.requests().len(), 1);
}

// ----- the fake fixtures, through the tool server -----------------------------------------

const ROLLUP_SKILL: &str = "---\nname: rollup-test\ndescription: Test skill that counts issues.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"reasoning\"\n  data-class: \"internal\"\n  tools: \"jira.search jira.rollup stats.cashflow\"\n---\n\n# /rollup-test\n\nTest.\n";

#[tokio::test]
async fn the_fake_fixtures_through_the_tool_server() {
    let mut f = common::fixture();
    write_content(&f.root, "skills/rollup-test/SKILL.md", ROLLUP_SKILL);
    let catalog = Catalog::load(&f.root, now().date_naive()).expect("content lints");
    let connectors: Vec<Arc<dyn Connector>> = vec![
        f.jira.clone(),
        f.confluence.clone(),
        f.slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    f.svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );
    let s = f
        .svc
        .create_session("ana", "rollup-test", "r1", None, now())
        .await
        .unwrap();
    let names: Vec<String> = f
        .svc
        .list_tools(&s)
        .iter()
        .map(|t| t.harness_name.clone())
        .collect();
    assert_eq!(names, vec!["jira_rollup", "jira_search", "stats_cashflow"]);

    let out = f
        .svc
        .call(&s, "jira_rollup", json!({"jql": "anything at all"}), now())
        .await
        .unwrap();
    let wrapped = &out["results"][0];
    assert_eq!(wrapped["untrusted"], true, "group names are Jira's text");
    assert_eq!(wrapped["title"], "Jira rollup: 6 issues by project");
    let c = &wrapped["content"];
    assert_eq!(
        c,
        &json!({
            "counted": 6, "truncated": false, "reason": null, "group_by": "project",
            "groups": [
                {"key": "CLD", "name": "CLD", "total": 3, "todo": 1, "in_progress": 1, "done": 1,
                 "overdue": 1, "unassigned": 1},
                {"key": "PM", "name": "PM", "total": 3, "todo": 2, "in_progress": 1, "done": 0,
                 "overdue": 0, "unassigned": 0},
            ],
            "links": [
                {"from": "PM-102", "from_group": "PM", "relation": "blocks", "to": "CLD-13",
                 "to_group": "CLD", "to_state": "todo"},
            ],
            "links_truncated": false,
        })
    );
    let by_epic = f
        .svc
        .call(
            &s,
            "jira_rollup",
            json!({"jql": "x", "group_by": "epic"}),
            now(),
        )
        .await
        .unwrap();
    let c = &by_epic["results"][0]["content"];
    let groups: Vec<(&str, u64)> = c["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["key"].as_str().unwrap(), g["total"].as_u64().unwrap()))
        .collect();
    assert_eq!(groups, vec![("CLD-11", 3), ("PM-101", 2), ("none", 1)]);
    // Search results didn't change: the rollup's fixtures are a set of their own.
    let hits = f
        .svc
        .call(&s, "jira_search", json!({"jql": "CLD"}), now())
        .await
        .unwrap();
    assert_eq!(hits["results"], json!([]));

    // Bad arguments are refused by the schema.
    for args in [
        json!({"jql": "x", "group_by": "team"}),
        json!({"jql": "x", "max_issues": 1001}),
        json!({"jql": "x", "max_issues": 0}),
        json!({"jql": "x".repeat(2001)}),
        json!({"group_by": "epic"}),
    ] {
        assert!(
            matches!(
                f.svc.call(&s, "jira_rollup", args.clone(), now()).await,
                Err(pmos_toolserver::service::ToolError::InvalidArgs(_))
            ),
            "{args}"
        );
    }

    // stats.cashflow, the worked case.
    let out = f
        .svc
        .call(
            &s,
            "stats_cashflow",
            json!({"scenarios": [{"name": "base", "upfront": 100, "years": [
                {"revenue": 60, "cost": 10}, {"revenue": 80, "cost": 10}]}], "discount_rate": 0.1}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(out["scenarios"][0]["payback_years"], 1.7);
    assert_eq!(out["scenarios"][0]["npv"], 3.31);
    let err = f
        .svc
        .call(
            &s,
            "stats_cashflow",
            json!({"scenarios": [{"name": "base", "upfront": -1, "years": [{"revenue": 1, "cost": 0}]}]}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, pmos_toolserver::service::ToolError::InvalidArgs(ref m) if m.contains("scenarios[0].upfront")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_team_bot_sees_only_its_projects_in_the_fake() {
    let jira = FakeConnector::jira();
    let reads = pmos_contract::bots::Reads {
        jira_projects: vec!["CLD".into()],
        confluence_spaces: Vec::new(),
        slack_channels: Vec::new(),
    };
    let bot = Caller::new("ana", "t").for_team_bot(TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    let items = jira
        .read(bot, "rollup", &json!({"jql": "x"}))
        .await
        .unwrap();
    assert_eq!(items[0].content["counted"], 3);
    assert_eq!(items[0].content["groups"][0]["key"], "CLD");
    // PM-102 isn't in the bot's view, so CLD-13's link to it leaves the query.
    assert_eq!(
        items[0].content["links"],
        json!([{"from": "CLD-13", "from_group": "CLD", "relation": "is blocked by",
                "to": "PM-102", "to_group": "PM", "to_state": "todo"}])
    );
}
