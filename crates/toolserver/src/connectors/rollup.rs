//! `jira.rollup` (`docs/staff-principal-spec.md` §3): counts issues across teams, so a skill
//! quotes counted numbers instead of the model counting search results. The live Jira
//! connector pages through the search and the fake connector serves its own fixture set;
//! both turn each issue into a [`RollupIssue`] and count with [`rollup`].
//!
//! The counts have no descriptions or comments, but group names (projects, epics,
//! assignees, versions) are text from Jira, so the item goes back wrapped as untrusted like
//! every other Jira read.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde_json::{json, Value};

use super::{Item, SourceRef};

/// The ways `jira.rollup` groups; the first is the default.
pub const GROUP_BY: [&str; 5] = ["project", "epic", "status", "assignee", "fix_version"];
/// The most issues one rollup reads.
pub const MAX_ISSUES: i64 = 1000;
/// Issues read when `max_issues` isn't given.
pub const DEFAULT_MAX_ISSUES: usize = 500;
/// Issues per search page.
pub const PAGE_SIZE: usize = 100;
/// The most groups listed by name; the rest are summed into `other`.
pub const MAX_GROUPS: usize = 50;
/// The most links returned.
pub const MAX_LINKS: usize = 200;
/// The fields one rollup page asks for.
pub const FIELDS: &str = "status,project,parent,fixVersions,duedate,issuelinks,assignee,issuetype";
/// How long one rollup may take in all, by default.
pub const DEFAULT_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

/// The group an issue with no value for the grouping goes in.
pub const NONE_KEY: &str = "none";
/// The group the groups past [`MAX_GROUPS`] are summed into.
pub const OTHER_KEY: &str = "other";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Todo,
    InProgress,
    Done,
}

impl State {
    /// From Jira's status category key: `new`, `indeterminate` or `done`. Anything else
    /// (Jira's `undefined`) counts as to do.
    pub fn from_category(key: Option<&str>) -> Self {
        match key {
            Some("done") => State::Done,
            Some("indeterminate") => State::InProgress,
            _ => State::Todo,
        }
    }

    fn name(self) -> &'static str {
        match self {
            State::Todo => "todo",
            State::InProgress => "in_progress",
            State::Done => "done",
        }
    }
}

/// One link on an issue, as the issue sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RollupLink {
    /// The link type's name, such as `Blocks`.
    pub type_name: String,
    /// True when this issue is the outward side (it "blocks" the other one).
    pub outward: bool,
    /// The relation as this issue reads it: `blocks`, or `is blocked by`.
    pub relation: String,
    /// The relation from the other side.
    pub reverse: String,
    pub other: String,
    pub other_state: Option<State>,
}

/// The parts of an issue a rollup counts.
#[derive(Debug, Clone, PartialEq)]
pub struct RollupIssue {
    pub key: String,
    pub project: (String, String),
    /// The epic it belongs to, key and summary: its parent when the parent is an epic, or
    /// itself when it is an epic.
    pub epic: Option<(String, String)>,
    pub status: String,
    pub state: State,
    pub assignee: Option<(String, String)>,
    pub fix_versions: Vec<String>,
    pub due: Option<NaiveDate>,
    pub links: Vec<RollupLink>,
}

fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn is_epic(issuetype: &Value) -> bool {
    issuetype["name"].as_str() == Some("Epic") || issuetype["hierarchyLevel"].as_i64() == Some(1)
}

impl RollupIssue {
    /// From an issue as `GET /rest/api/3/search/jql` returns it with [`FIELDS`].
    pub fn from_jira(issue: &Value) -> Option<Self> {
        let key = text(&issue["key"])?;
        let f = &issue["fields"];
        let prefix = project_of(&key).to_string();
        let project_key = text(&f["project"]["key"]).unwrap_or_else(|| prefix.clone());
        let project_name = text(&f["project"]["name"]).unwrap_or_else(|| project_key.clone());
        let parent = &f["parent"];
        // A subtask's parent is a story, not an epic; a parent with no type is taken as the
        // epic (the field's usual meaning for a story or task).
        let subtask = f["issuetype"]["subtask"].as_bool() == Some(true);
        let epic = if is_epic(&f["issuetype"]) {
            Some((
                key.clone(),
                text(&f["summary"]).unwrap_or_else(|| key.clone()),
            ))
        } else {
            text(&parent["key"])
                .filter(|_| {
                    let ptype = &parent["fields"]["issuetype"];
                    if ptype.is_object() {
                        is_epic(ptype)
                    } else {
                        !subtask
                    }
                })
                .map(|k| {
                    let name = text(&parent["fields"]["summary"]).unwrap_or_else(|| k.clone());
                    (k, name)
                })
        };
        let assignee = f["assignee"].as_object().map(|a| {
            let name = text(&a["displayName"]).unwrap_or_else(|| "Unknown".into());
            let id = text(&a["accountId"]).unwrap_or_else(|| name.clone());
            (id, name)
        });
        let fix_versions = f["fixVersions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| text(&v["name"]))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let due = f["duedate"]
            .as_str()
            .and_then(|d| NaiveDate::parse_from_str(d.get(..10).unwrap_or(d), "%Y-%m-%d").ok());
        let links = f["issuelinks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let t = &l["type"];
                let type_name = text(&t["name"]).unwrap_or_else(|| "Relates".into());
                let outward_text = text(&t["outward"]).unwrap_or_else(|| type_name.to_lowercase());
                let inward_text = text(&t["inward"]).unwrap_or_else(|| type_name.to_lowercase());
                let (outward, other) = if l["outwardIssue"].is_object() {
                    (true, &l["outwardIssue"])
                } else if l["inwardIssue"].is_object() {
                    (false, &l["inwardIssue"])
                } else {
                    return None;
                };
                let (relation, reverse) = if outward {
                    (outward_text, inward_text)
                } else {
                    (inward_text, outward_text)
                };
                Some(RollupLink {
                    type_name,
                    outward,
                    relation,
                    reverse,
                    other: text(&other["key"])?,
                    other_state: other["fields"]["status"]["statusCategory"]["key"]
                        .as_str()
                        .map(|k| State::from_category(Some(k))),
                })
            })
            .collect();
        Some(Self {
            key,
            project: (project_key, project_name),
            epic,
            status: text(&f["status"]["name"]).unwrap_or_else(|| "Unknown".into()),
            state: State::from_category(f["status"]["statusCategory"]["key"].as_str()),
            assignee,
            fix_versions,
            due,
            links,
        })
    }

    /// The groups the issue counts in, `(key, name)`: one, except with `fix_version` (one
    /// per version). Empty means `none`.
    fn groups(&self, group_by: &str) -> Vec<(String, String)> {
        match group_by {
            "epic" => self.epic.iter().cloned().collect(),
            "status" => vec![(self.status.clone(), self.status.clone())],
            "assignee" => self.assignee.iter().cloned().collect(),
            "fix_version" => self
                .fix_versions
                .iter()
                .map(|v| (v.clone(), v.clone()))
                .collect(),
            _ => vec![self.project.clone()],
        }
    }

    /// The group a link from this issue is reported from: its first group, or `none`.
    fn link_group(&self, group_by: &str) -> String {
        self.groups(group_by)
            .into_iter()
            .next()
            .map_or_else(|| NONE_KEY.to_string(), |(k, _)| k)
    }
}

/// The project part of an issue key: `PM` for `PM-101`.
pub fn project_of(key: &str) -> &str {
    key.rsplit_once('-').map_or(key, |(p, _)| p)
}

/// Why a rollup stopped before reading every issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncated {
    /// It reached `max_issues`.
    Cap,
    /// It ran out of time.
    Time,
}

#[derive(Default)]
struct Counts {
    name: String,
    total: u64,
    todo: u64,
    in_progress: u64,
    done: u64,
    overdue: u64,
    unassigned: u64,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        self.total += other.total;
        self.todo += other.todo;
        self.in_progress += other.in_progress;
        self.done += other.done;
        self.overdue += other.overdue;
        self.unassigned += other.unassigned;
    }

    fn json(&self, key: &str) -> Value {
        json!({
            "key": key, "name": self.name, "total": self.total, "todo": self.todo,
            "in_progress": self.in_progress, "done": self.done, "overdue": self.overdue,
            "unassigned": self.unassigned,
        })
    }
}

/// Count `issues` by `group_by`, as of `today` (for `overdue`). `truncated` says whether,
/// and why, the reading stopped early.
pub fn rollup(
    issues: &[RollupIssue],
    group_by: &str,
    today: NaiveDate,
    truncated: Option<Truncated>,
) -> Value {
    // Each issue once, even if a page repeated it.
    let mut seen = BTreeSet::new();
    let issues: Vec<&RollupIssue> = issues
        .iter()
        .filter(|i| seen.insert(i.key.clone()))
        .collect();

    let mut groups: BTreeMap<String, Counts> = BTreeMap::new();
    let mut none = Counts {
        name: format!("No {}", group_by.replace('_', " ")),
        ..Counts::default()
    };
    for issue in &issues {
        let one = Counts {
            total: 1,
            todo: u64::from(issue.state == State::Todo),
            in_progress: u64::from(issue.state == State::InProgress),
            done: u64::from(issue.state == State::Done),
            overdue: u64::from(issue.state != State::Done && issue.due.is_some_and(|d| d < today)),
            unassigned: u64::from(issue.assignee.is_none()),
            ..Counts::default()
        };
        let in_groups = issue.groups(group_by);
        if in_groups.is_empty() {
            none.add(&one);
        }
        for (key, name) in in_groups {
            let g = groups.entry(key).or_default();
            if g.name.is_empty() {
                g.name = name;
            }
            g.add(&one);
        }
    }
    let mut ranked: Vec<(String, Counts)> = groups.into_iter().collect();
    // Largest first; ties by key, so the order is stable.
    ranked.sort_by(|(ka, a), (kb, b)| b.total.cmp(&a.total).then_with(|| ka.cmp(kb)));
    let rest: Vec<(String, Counts)> = ranked.split_off(ranked.len().min(MAX_GROUPS));
    let mut out: Vec<Value> = ranked.iter().map(|(k, c)| c.json(k)).collect();
    if none.total > 0 {
        out.push(none.json(NONE_KEY));
    }
    if !rest.is_empty() {
        let mut other = Counts {
            name: format!("{} more groups", rest.len()),
            ..Counts::default()
        };
        for (_, c) in &rest {
            other.add(c);
        }
        out.push(other.json(OTHER_KEY));
    }

    // Links that cross groups or leave the query, each pair once.
    let by_key: BTreeMap<&str, &RollupIssue> =
        issues.iter().map(|i| (i.key.as_str(), *i)).collect();
    let mut pairs = BTreeSet::new();
    let mut links = Vec::new();
    for issue in &issues {
        for link in &issue.links {
            // One pair: the link's type, its outward side and its inward side.
            let (out_key, in_key) = if link.outward {
                (issue.key.as_str(), link.other.as_str())
            } else {
                (link.other.as_str(), issue.key.as_str())
            };
            if !pairs.insert((
                link.type_name.clone(),
                out_key.to_string(),
                in_key.to_string(),
            )) {
                continue;
            }
            let entry = match by_key.get(link.other.as_str()) {
                Some(other) => {
                    let (from, to, relation) = if link.outward {
                        (*issue, *other, &link.relation)
                    } else {
                        (*other, *issue, &link.reverse)
                    };
                    let (from_group, to_group) =
                        (from.link_group(group_by), to.link_group(group_by));
                    if from_group == to_group {
                        continue;
                    }
                    json!({
                        "from": from.key, "from_group": from_group, "relation": relation,
                        "to": to.key, "to_group": to_group, "to_state": to.state.name(),
                    })
                }
                // Outside the query: reported from the issue inside it. By project, the other
                // issue's group is its key's prefix; otherwise it isn't known.
                None => json!({
                    "from": issue.key,
                    "from_group": issue.link_group(group_by),
                    "relation": link.relation,
                    "to": link.other,
                    "to_group": if group_by == "project" { json!(project_of(&link.other)) } else { Value::Null },
                    "to_state": link.other_state.map(State::name),
                }),
            };
            links.push(entry);
        }
    }
    links.sort_by(|a, b| {
        let k = |v: &Value| {
            (
                v["from"].as_str().unwrap_or_default().to_string(),
                v["to"].as_str().unwrap_or_default().to_string(),
                v["relation"].as_str().unwrap_or_default().to_string(),
            )
        };
        k(a).cmp(&k(b))
    });
    let links_truncated = links.len() > MAX_LINKS;
    links.truncate(MAX_LINKS);

    json!({
        "counted": issues.len(),
        "truncated": truncated.is_some(),
        "reason": match truncated {
            Some(Truncated::Cap) => json!("cap"),
            Some(Truncated::Time) => json!("time"),
            None => Value::Null,
        },
        "group_by": group_by,
        "groups": out,
        "links": links,
        "links_truncated": links_truncated,
    })
}

/// The rollup's arguments: `group_by` (default `project`) and `max_issues` (default 500).
/// The schema has already checked them; these are the defaults and a last check.
pub fn options(args: &Value) -> Result<(&str, usize), super::ConnectorError> {
    let group_by = args["group_by"].as_str().unwrap_or(GROUP_BY[0]);
    if !GROUP_BY.contains(&group_by) {
        return Err(super::ConnectorError::BadArgs(format!(
            "`group_by` must be one of {}",
            GROUP_BY.join(", ")
        )));
    }
    let max = match args.get("max_issues") {
        None | Some(Value::Null) => DEFAULT_MAX_ISSUES,
        Some(v) => match v.as_i64() {
            Some(n) if (1..=MAX_ISSUES).contains(&n) => n as usize,
            _ => {
                return Err(super::ConnectorError::BadArgs(format!(
                    "`max_issues` must be between 1 and {MAX_ISSUES}"
                )))
            }
        },
    };
    Ok((group_by, max))
}

/// The one item a rollup returns.
pub fn item(content: Value, url: String) -> Item {
    let n = content["counted"].as_u64().unwrap_or(0);
    let group_by = content["group_by"]
        .as_str()
        .unwrap_or("project")
        .to_string();
    Item {
        source: SourceRef {
            system: "jira".into(),
            id: format!("rollup:{group_by}"),
            url,
        },
        title: format!("Jira rollup: {n} issues by {group_by}"),
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(key: &str, cat: &str) -> Value {
        json!({"key": key, "fields": {"status": {"name": cat, "statusCategory": {"key": cat}},
               "project": {"key": project_of(key), "name": project_of(key)}}})
    }

    #[test]
    fn states_groups_and_none() {
        let mut a = issue("PM-1", "new");
        a["fields"]["fixVersions"] = json!([{"name": "1.0"}, {"name": "1.1"}]);
        a["fields"]["duedate"] = json!("2026-01-01");
        let b = issue("PM-2", "done");
        let issues: Vec<_> = [a, b].iter().filter_map(RollupIssue::from_jira).collect();
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let out = rollup(&issues, "fix_version", today, None);
        assert_eq!(out["counted"], 2);
        let keys: Vec<_> = out["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["key"].clone())
            .collect();
        assert_eq!(keys, vec!["1.0", "1.1", "none"]);
        assert_eq!(out["groups"][0]["overdue"], 1);
        assert_eq!(out["groups"][0]["unassigned"], 1);
        assert_eq!(out["groups"][2]["done"], 1);
        assert_eq!(out["reason"], Value::Null);
        assert_eq!(
            State::from_category(Some("indeterminate")),
            State::InProgress
        );
        assert_eq!(State::from_category(Some("undefined")), State::Todo);
    }
}
