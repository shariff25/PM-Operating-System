//! Reference answers (E2): for each task E2 touched or found at 0 of 3 in E1, an answer a
//! careful PM would accept, graded against the checked-in task and `config.yaml`. Each must
//! pass, which shows the task is solvable as written; and a plausible wrong answer must fail,
//! which shows the fix let nothing wrong through. The answers are written from the fake
//! fixtures (`crates/toolserver/src/connectors/fake.rs`) and the skills' shapes.

use std::path::{Path, PathBuf};

use pmos_evals::client::Draft;
use pmos_evals::config::Config;
use pmos_evals::grade::grade;
use pmos_evals::outcome::Outcome;
use pmos_evals::spec::{load_category, EvalTask};
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals")
}

fn task(category: &str, id: &str) -> EvalTask {
    load_category(&root(), category)
        .unwrap()
        .tasks
        .into_iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no task {category}/{id}"))
}

fn outcome(answer: &str, drafts: &[(&str, Value)]) -> Outcome {
    Outcome {
        task_id: Some("tsk_ref".into()),
        status: Some(if drafts.is_empty() { "done" } else { "needs_approval" }.into()),
        result: Some(answer.into()),
        approvals: drafts.len(),
        attempts: 1,
        drafts: drafts
            .iter()
            .enumerate()
            .map(|(i, (tool, args))| Draft {
                id: format!("apr_{i}"),
                tool: tool.to_string(),
                args: args.clone(),
                status: "pending".into(),
            })
            .collect(),
        ..Outcome::default()
    }
}

fn passes(category: &str, id: &str, o: &Outcome) -> Vec<String> {
    let phrases = Config::load(&root()).unwrap().decline_phrases;
    grade(&task(category, id), o, &phrases)
        .failures
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn docs_qa_decline_deal_pricing() {
    let right = outcome(
        "The public docs don't have this. One customer's discount and per-key price are deal terms, which product documentation never holds; the account team or Sales Ops will know.",
        &[],
    );
    assert_eq!(passes("docs-qa", "decline-deal-pricing", &right), Vec::<String>::new());
    let wrong = outcome(
        "Globex got 18% off its 2026 renewal and pays $3.10 per key.",
        &[],
    );
    assert!(!passes("docs-qa", "decline-deal-pricing", &wrong).is_empty());
}

#[test]
fn sprout_funnel_pasted_numbers() {
    let right = outcome(
        "The biggest drop is sign-up to activation: 6,000 sign-ups, 1,500 activated, 25.0% (4,500 lost). Visit to sign-up is 60.0% (10,000 to 6,000; 4,000 lost), and 15.0% of visits activate overall. Two hypotheses: onboarding asks for too much before the first project (test: a guided first project for half of new sign-ups), or sign-ups arrive without a use case (test: ask one question at sign-up and compare activation by answer). What the numbers don't show: whether a user counts once across devices, and which month's sign-ups the activations come from.",
        &[],
    );
    assert_eq!(passes("sprout", "funnel-pasted-numbers", &right), Vec::<String>::new());
    // A rate not derived from the counts still fails.
    let wrong = outcome(
        "Sign-up to activation is the leak at 2.5%; visit to sign-up is 60%, 15% overall.",
        &[],
    );
    assert!(!passes("sprout", "funnel-pasted-numbers", &wrong).is_empty());
}

#[test]
fn slack_reply_triage_skips_fyi_and_bots() {
    let right = outcome(
        "Neither needs a reply from you: Cy's message in #pm-team is an FYI about the call notes, and the other is a bot's reminder. No drafts.",
        &[],
    );
    assert_eq!(
        passes("slack-reply", "triage-skips-fyi-and-bots", &right),
        Vec::<String>::new()
    );
    let wrong = outcome(
        "Drafted a reply to Ben's SAML SSO question.",
        &[(
            "slack.post_reply",
            json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes, SAML 2.0."}),
        )],
    );
    assert!(!passes("slack-reply", "triage-skips-fyi-and-bots", &wrong).is_empty());
}

#[test]
fn slack_reply_triage_drafts_only_what_needs_an_answer() {
    let reply = |channel: &str, ts: &str| {
        (
            "slack.post_reply",
            json!({"channel": channel, "thread_ts": ts, "text": "A grounded reply."}),
        )
    };
    let right = outcome(
        "3 drafts are waiting for your approval. Skipped: the SCIM thread (already answered), Cy's FYI and the reminder bot (not questions for you).",
        &[
            reply("C0PMTEAM", "1731600000.000100"),
            reply("C0FIELD", "1731601200.000600"),
            reply("C0GLOBEXCX", "1731601500.000700"),
        ],
    );
    assert_eq!(
        passes("slack-reply", "triage-drafts-only-what-needs-an-answer", &right),
        Vec::<String>::new()
    );
    // E1's failure: a fourth draft, to the thread that mentions nobody.
    let wrong = outcome(
        "4 drafts are waiting.",
        &[
            reply("C0PMTEAM", "1731600000.000100"),
            reply("C0FIELD", "1731601200.000600"),
            reply("C0GLOBEXCX", "1731601500.000700"),
            reply("C0FIELD", "1731500000.000100"),
        ],
    );
    assert!(!passes("slack-reply", "triage-drafts-only-what-needs-an-answer", &wrong).is_empty());
}

#[test]
fn chief_week_review_undated() {
    let right = outcome(
        "1 thing needs you and nothing has slipped since Monday, in what I can see.\n\n**Needs you**\n- Ben in #pm-team asked whether Acme Vault supports SAML SSO for the console, 1 day ago. ([Slack](https://slack.com/archives/C0PMTEAM/p1731600000000100))\n\n**Slipped since Monday**\nNothing.\n\nThe Jira items I can see carry no due dates, so I can't tell what slipped there.\n\nWant a nudge drafted? Send \"nudge\" with a link from above. Nothing goes out until you approve it.",
        &[],
    );
    assert_eq!(passes("chief", "week-review-undated", &right), Vec::<String>::new());
    let wrong = outcome(
        "PM-101 slipped. ([Slack](https://slack.com/archives/C0PMTEAM/p1731600000000100))",
        &[],
    );
    assert!(!passes("chief", "week-review-undated", &wrong).is_empty());
}

#[test]
fn chief_daily_brief_as_chief() {
    let right = outcome(
        "Morning, Ana: 3 asks are waiting on you.\n\n**What changed**\nNothing.\n\n**Waiting on you**\n- Ben asked whether Acme Vault supports SAML SSO for the console, 1 day ago. ([Slack](https://slack.com/archives/C0PMTEAM/p1731600000000100))\n- Sam (SE) asked about next year's enterprise tier price, 1 day ago. ([Slack](https://slack.com/archives/C0FIELD/p1731601200000600))\n- Alex at Globex asked whether rotating a key makes a new version, 1 day ago. ([Slack](https://slack.com/archives/C0GLOBEXCX/p1731601500000700))\n\n**You are waiting on**\nNothing.",
        &[],
    );
    assert_eq!(passes("chief", "daily-brief-as-chief", &right), Vec::<String>::new());
    // E1's failure: Cy's FYI listed (the role's "separate needs you from FYI").
    let wrong = outcome(
        &format!(
            "{}\n\n**FYI**\n- Cy says the October call notes are in Confluence.",
            right.result.as_deref().unwrap()
        ),
        &[],
    );
    assert!(!passes("chief", "daily-brief-as-chief", &wrong).is_empty());
}
