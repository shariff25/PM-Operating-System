//! LLM judges: `evals/judges/<name>.yaml`, each one yes-or-no question about an answer or
//! draft, asked of a gateway alias.
//!
//! **Yes means pass.** A judge's `question` is phrased so that "yes" is the good answer, and a
//! label's `label` is `true` when a person answered "yes". So the true-positive rate (TPR) is
//! how often the judge says yes when the person did, and the true-negative rate (TNR) how
//! often it says no when the person did.
//!
//! A judge **blocks** (can fail the gate) only when its calibration has n ≥ 100 labels, TPR and
//! TNR of at least 0.8, and was made with the judge's current question and alias. Otherwise it
//! is **advisory**: reported, never failing. Judges never change a task's pass or fail.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::spec::is_slug;

/// The default alias: a different model family from the drafters (`docs/gateway.md`).
pub const DEFAULT_ALIAS: &str = "pmos/extraction";
/// The fewest labels a blocking judge is calibrated on.
pub const MIN_LABELS: usize = 100;
/// The lowest TPR and TNR a blocking judge may have.
pub const MIN_RATE: f64 = 0.8;

fn default_alias() -> String {
    DEFAULT_ALIAS.to_string()
}

/// A judge as its file defines it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Judge {
    pub name: String,
    /// The taxonomy sub-tag a "no" points to, such as `model/unfaithful-to-source`.
    pub failure_mode: String,
    /// The yes-or-no question; "yes" means the output passes.
    pub question: String,
    #[serde(default = "default_alias")]
    pub alias: String,
    /// The categories whose tasks it judges.
    pub applies_to: Vec<String>,
    /// The JSON-lines file of labels, relative to `evals/judges/`. An empty file means no
    /// labels yet.
    pub labels: String,
    /// The judge needs what the bot read: an attempt whose trace has no tool results (or
    /// couldn't be read) gets no verdict rather than a guess.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_sources: bool,
}

/// A person's answer in a label: yes, no, "unknown" (they couldn't tell from what the judge
/// is shown), or not given yet (`null`: a line from a labelling pack still to be labelled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Human {
    Yes,
    No,
    Unknown,
    Pending,
}

impl Human {
    /// `Some(true)` for yes, `Some(false)` for no; unknown and pending say nothing.
    pub fn answer(self) -> Option<bool> {
        match self {
            Human::Yes => Some(true),
            Human::No => Some(false),
            Human::Unknown | Human::Pending => None,
        }
    }
}

impl Serialize for Human {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Human::Yes => s.serialize_bool(true),
            Human::No => s.serialize_bool(false),
            Human::Unknown => s.serialize_str("unknown"),
            Human::Pending => s.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for Human {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Bool(true) => Ok(Human::Yes),
            Value::Bool(false) => Ok(Human::No),
            Value::Null => Ok(Human::Pending),
            Value::String(s) if s == "unknown" => Ok(Human::Unknown),
            other => Err(serde::de::Error::custom(format!(
                "`label` is true (yes), false (no), \"unknown\" or null (not labelled yet), not {other}"
            ))),
        }
    }
}

/// One labelled example: what the judge would see, and a person's answer to its question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub input: String,
    pub output: String,
    /// The person's answer: `true` is "yes" (the output passes), `false` "no", `"unknown"`
    /// when they couldn't tell, `null` while the line waits for a person.
    pub label: Human,
    /// Where a pack line came from, for the labeller: `<task> run <n>`. Never read
    /// by calibration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What the judge said when the run asked it (`yes`, `no` or `unknown`), so a pack can
    /// mix verdicts. Never read by calibration; a labeller need not look at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_said: Option<String>,
}

/// A judge's reply: yes, no, unknown (the judge says it can't tell), or none of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Yes,
    No,
    Unknown,
    Unparsed,
}

impl Reply {
    pub fn verdict(self) -> Option<bool> {
        match self {
            Reply::Yes => Some(true),
            Reply::No => Some(false),
            Reply::Unknown | Reply::Unparsed => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Reply::Yes => "yes",
            Reply::No => "no",
            Reply::Unknown => "unknown",
            Reply::Unparsed => "unparsed",
        }
    }
}

/// `evals/judges/<name>.calibration.json`, written by `pmos-evals judge calibrate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Calibration {
    pub judge: String,
    pub date: NaiveDate,
    pub n: usize,
    /// Labels that are "yes" and "no".
    pub positives: usize,
    pub negatives: usize,
    /// `None` when there were no labels of that kind.
    pub tpr: Option<f64>,
    pub tnr: Option<f64>,
    /// Replies that were neither yes nor no (unknown included); each counts as a wrong
    /// verdict.
    pub unparsed: usize,
    /// Of `unparsed`, the replies that were "unknown".
    #[serde(default)]
    pub judge_unknown: usize,
    /// Labels a person marked "unknown", and lines not labelled yet: neither is in `n` or the
    /// rates.
    #[serde(default)]
    pub labels_unknown: usize,
    #[serde(default)]
    pub labels_pending: usize,
    pub alias: String,
    /// [`question_hash`] of the question and alias calibrated.
    pub question_sha256: String,
}

/// Whether a judge blocks, and why (or why not).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgeStatus {
    pub name: String,
    pub blocking: bool,
    pub reason: String,
}

/// One judge's verdict on one attempt, as a report keeps it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgeVerdict {
    pub name: String,
    /// `None`: the judge couldn't be asked or didn't answer yes or no (see `error`).
    pub verdict: Option<bool>,
    /// Whether the judge was blocking when this run asked it.
    pub blocking: bool,
    /// The judge answered "unknown": it couldn't tell from what it was shown (`verdict` is
    /// then `None`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unknown: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn judges_dir(root: &Path) -> PathBuf {
    root.join("judges")
}

/// sha256, in hex, of the question text, a newline, and the alias.
pub fn question_hash(question: &str, alias: &str) -> String {
    let mut h = Sha256::new();
    h.update(question.as_bytes());
    h.update(b"\n");
    h.update(alias.as_bytes());
    h.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl Judge {
    pub fn labels_path(&self, root: &Path) -> PathBuf {
        judges_dir(root).join(&self.labels)
    }

    pub fn calibration_path(&self, root: &Path) -> PathBuf {
        judges_dir(root).join(format!("{}.calibration.json", self.name))
    }

    pub fn applies_to(&self, category: &str) -> bool {
        self.applies_to.iter().any(|c| c == category)
    }

    pub fn hash(&self) -> String {
        question_hash(&self.question, &self.alias)
    }
}

/// Every judge under `<root>/judges`, by file name. A missing directory means none. Errors
/// are `path: message`, all at once.
pub fn load_judges(root: &Path) -> Result<Vec<Judge>, Vec<String>> {
    let dir = judges_dir(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(vec![format!("{}: can't read: {e}", dir.display())]),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("yaml") | Some("yml")
            )
        })
        .collect();
    files.sort();
    let mut judges = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        let shown = file.display().to_string();
        let judge: Judge = match std::fs::read_to_string(&file)
            .map_err(|e| format!("can't read: {e}"))
            .and_then(|t| serde_norway::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(j) => j,
            Err(e) => {
                errors.push(format!("{shown}: {e}"));
                continue;
            }
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !is_slug(&judge.name) || judge.name != stem {
            errors.push(format!(
                "{shown}: `name` `{}` must be lowercase letters, digits and hyphens, and match the file name",
                judge.name
            ));
        }
        if !judge.failure_mode.contains('/') {
            errors.push(format!(
                "{shown}: `failure_mode` `{}` is a taxonomy sub-tag such as `model/missing-citation`",
                judge.failure_mode
            ));
        }
        if judge.question.trim().is_empty() {
            errors.push(format!("{shown}: `question` is empty"));
        }
        if !judge.alias.starts_with("pmos/") {
            errors.push(format!(
                "{shown}: `alias` `{}` is a gateway alias such as `{DEFAULT_ALIAS}`",
                judge.alias
            ));
        }
        if judge.applies_to.is_empty() {
            errors.push(format!("{shown}: `applies_to` names no category"));
        }
        if let Err(e) = load_labels(root, &judge) {
            errors.push(e);
        }
        if let Err(e) = load_calibration(root, &judge) {
            errors.push(e);
        }
        judges.push(judge);
    }
    if errors.is_empty() {
        Ok(judges)
    } else {
        Err(errors)
    }
}

/// A judge's labels. An empty file is no labels; a missing file is an error.
pub fn load_labels(root: &Path, judge: &Judge) -> Result<Vec<Label>, String> {
    let path = judge.labels_path(root);
    let shown = path.display().to_string();
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!("{shown}: can't read: {e} (with no labels yet, commit an empty file)")
    })?;
    let mut labels = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let label: Label = serde_json::from_str(line).map_err(|e| {
            format!(
                "{shown}:{}: not a label {{input, output, label}}: {e}",
                i + 1
            )
        })?;
        labels.push(label);
    }
    Ok(labels)
}

/// Write a run's labelling-pack candidates, one file per judge:
/// `<dir>/<date>-<category>-<judge>.jsonl`, appended to. Returns each file and its count.
pub fn write_candidates(
    dir: &Path,
    date: &NaiveDate,
    category: &str,
    lines: &[(String, Label)],
) -> std::io::Result<Vec<(PathBuf, usize)>> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let mut by_judge: std::collections::BTreeMap<&str, Vec<&Label>> = Default::default();
    for (judge, label) in lines {
        by_judge.entry(judge).or_default().push(label);
    }
    let mut out = Vec::new();
    for (judge, labels) in by_judge {
        let path = dir.join(format!("{date}-{category}-{judge}.jsonl"));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        for l in &labels {
            let line = serde_json::to_string(l).map_err(std::io::Error::other)?;
            writeln!(f, "{line}")?;
        }
        out.push((path, labels.len()));
    }
    Ok(out)
}

/// How many pack lines of each judge verdict were added.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Packed {
    pub yes: usize,
    pub no: usize,
    pub unknown: usize,
    /// Candidates left out because the labels file already has the same input and output.
    pub already: usize,
}

/// Build a judge's labelling pack: from the candidates in `dir` (`*-<judge>.jsonl`), pick up to
/// `per_judge` lines, taking in turn one the judge said no to, one it said yes to and one it
/// said unknown to, so the pack mixes verdicts (a person's label is what counts; the mix only
/// keeps the sample from being all one kind). Lines whose input and output are already in the
/// labels file are skipped. Picked lines are appended to the judge's labels file with `label`
/// empty (`null`), for a person to fill in. Candidates are taken in file and line order, so
/// the same candidates give the same pack.
pub fn pack(root: &Path, judge: &Judge, dir: &Path, per_judge: usize) -> Result<Packed, String> {
    use std::io::Write as _;
    let suffix = format!("-{}.jsonl", judge.name);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: can't read: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .collect();
    files.sort();
    let existing = load_labels(root, judge)?;
    let mut seen: std::collections::HashSet<(String, String)> = existing
        .iter()
        .map(|l| (l.input.clone(), l.output.clone()))
        .collect();
    let mut groups: [Vec<Label>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut packed = Packed::default();
    for f in &files {
        let text =
            std::fs::read_to_string(f).map_err(|e| format!("{}: can't read: {e}", f.display()))?;
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut l: Label = serde_json::from_str(line)
                .map_err(|e| format!("{}:{}: not a candidate: {e}", f.display(), i + 1))?;
            if !seen.insert((l.input.clone(), l.output.clone())) {
                packed.already += 1;
                continue;
            }
            // A pack line always starts unlabelled, whatever the candidate held.
            l.label = Human::Pending;
            let g = match l.judge_said.as_deref() {
                Some("no") => 0,
                Some("yes") => 1,
                _ => 2,
            };
            groups[g].push(l);
        }
    }
    let mut picked = Vec::new();
    let mut next = [0usize; 3];
    while picked.len() < per_judge {
        let mut took = false;
        for g in 0..3 {
            if picked.len() < per_judge && next[g] < groups[g].len() {
                picked.push((g, groups[g][next[g]].clone()));
                next[g] += 1;
                took = true;
            }
        }
        if !took {
            break;
        }
    }
    let path = judge.labels_path(root);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: can't open: {e}", path.display()))?;
    for (g, l) in picked {
        let line = serde_json::to_string(&l).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()))?;
        match g {
            0 => packed.no += 1,
            1 => packed.yes += 1,
            _ => packed.unknown += 1,
        }
    }
    Ok(packed)
}

/// A judge's calibration, `None` if it has never been calibrated.
pub fn load_calibration(root: &Path, judge: &Judge) -> Result<Option<Calibration>, String> {
    let path = judge.calibration_path(root);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| format!("{}: not a calibration: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: can't read: {e}", path.display())),
    }
}

/// Blocking or advisory, with the reason.
pub fn status(judge: &Judge, calibration: Option<&Calibration>) -> JudgeStatus {
    let advisory = |reason: String| JudgeStatus {
        name: judge.name.clone(),
        blocking: false,
        reason,
    };
    let Some(c) = calibration else {
        return advisory("advisory: not calibrated".into());
    };
    if c.alias != judge.alias {
        return advisory(format!(
            "advisory: calibrated with `{}`, now `{}`; calibrate again",
            c.alias, judge.alias
        ));
    }
    if c.question_sha256 != judge.hash() {
        return advisory(
            "advisory: the question changed since calibration; calibrate again".into(),
        );
    }
    if c.n < MIN_LABELS {
        return advisory(format!(
            "advisory: {} labels, at least {MIN_LABELS} needed",
            c.n
        ));
    }
    let rate = |r: Option<f64>| r.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into());
    let ok = |r: Option<f64>| r.is_some_and(|x| x + 1e-9 >= MIN_RATE);
    if !ok(c.tpr) || !ok(c.tnr) {
        return advisory(format!(
            "advisory: TPR {}, TNR {}; both must be at least {MIN_RATE}",
            rate(c.tpr),
            rate(c.tnr)
        ));
    }
    JudgeStatus {
        name: judge.name.clone(),
        blocking: true,
        reason: format!(
            "blocking: n {}, TPR {}, TNR {} ({})",
            c.n,
            rate(c.tpr),
            rate(c.tnr),
            c.date
        ),
    }
}

/// Calibration numbers from labels and the judge's replies on them, in the same order. Only
/// labels a person answered yes or no count: "unknown" and pending labels are counted apart
/// and their replies ignored (pass `Reply::Unparsed` for them; they need not be asked). A
/// reply that isn't yes or no, "unknown" included, counts as wrong.
pub fn calibrate(
    judge: &Judge,
    date: NaiveDate,
    labels: &[Human],
    replies: &[Reply],
) -> Calibration {
    assert_eq!(labels.len(), replies.len(), "one reply per label");
    let (mut pos, mut neg, mut tp, mut tn, mut unparsed, mut unknown) = (0, 0, 0, 0, 0, 0);
    let (mut labels_unknown, mut labels_pending) = (0, 0);
    for (label, reply) in labels.iter().zip(replies) {
        let truth = match label {
            Human::Unknown => {
                labels_unknown += 1;
                continue;
            }
            Human::Pending => {
                labels_pending += 1;
                continue;
            }
            Human::Yes => true,
            Human::No => false,
        };
        let verdict = reply.verdict();
        if verdict.is_none() {
            unparsed += 1;
            unknown += usize::from(*reply == Reply::Unknown);
        }
        if truth {
            pos += 1;
            tp += usize::from(verdict == Some(true));
        } else {
            neg += 1;
            tn += usize::from(verdict == Some(false));
        }
    }
    let rate = |hit: usize, of: usize| (of > 0).then(|| hit as f64 / of as f64);
    Calibration {
        judge: judge.name.clone(),
        date,
        n: pos + neg,
        positives: pos,
        negatives: neg,
        tpr: rate(tp, pos),
        tnr: rate(tn, neg),
        unparsed,
        judge_unknown: unknown,
        labels_unknown,
        labels_pending,
        alias: judge.alias.clone(),
        question_sha256: judge.hash(),
    }
}

/// A judge's reply as a verdict. Reasoning in `<think>…</think>` is ignored. The reply's first
/// word decides when it is yes or no; otherwise a reply that says only one of yes and no (as a
/// word) is that. Anything else is `None`.
pub fn parse_verdict(reply: &str) -> Option<bool> {
    parse_reply(reply).verdict()
}

/// A judge's reply: [`parse_verdict`]'s rules, plus "unknown" (also "can't tell"): a first
/// word "unknown", or a reply whose only verdict word is "unknown".
pub fn parse_reply(reply: &str) -> Reply {
    let mut text = reply.to_string();
    while let Some(start) = text.find("<think>") {
        match text[start..].find("</think>") {
            Some(end) => text.replace_range(start..start + end + "</think>".len(), " "),
            None => text.truncate(start),
        }
    }
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match words.first() {
        Some(&"yes") => return Reply::Yes,
        Some(&"no") => return Reply::No,
        Some(&"unknown") => return Reply::Unknown,
        _ => {}
    }
    let yes = words.contains(&"yes");
    let no = words.contains(&"no");
    let unknown = words.contains(&"unknown");
    match (yes, no, unknown) {
        (true, false, false) => Reply::Yes,
        (false, true, false) => Reply::No,
        (false, false, true) => Reply::Unknown,
        _ => Reply::Unparsed,
    }
}

const SYSTEM: &str = "You grade an AI assistant's work. You answer one yes-or-no question about \
it. Reply with exactly one word: yes, no, or unknown. Say unknown only when what you are shown \
is not enough to decide.";

/// The user message a judge is asked with.
pub fn prompt(judge: &Judge, input: &str, output: &str) -> String {
    format!(
        "Question: {}\n\n<input>\n{}\n</input>\n\n<output>\n{}\n</output>\n\nAnswer the question with exactly one word: yes, no, or unknown (only if the input and output are not enough to decide).",
        judge.question.trim(),
        input.trim(),
        output.trim()
    )
}

/// The gateway, as client `evals` (`PMOS_EVALS_GATEWAY_TOKEN`).
///
/// Slice H4 (r7, X4): every judge call names, in `X-Pmos-Model-Source`, the model source the
/// gateway reported in `GET /v1/pmos/roles` when this run first asked ([`Gateway::models`]),
/// and an answer from another source is refused: a gateway restarted on another source
/// mid-run can't judge part of it.
#[derive(Clone)]
pub struct Gateway {
    http: reqwest::Client,
    /// The OpenAI base, ending in `/v1`.
    base: String,
    token: String,
    /// The source the gateway reported at the run's first roles read.
    source: std::sync::Arc<tokio::sync::OnceCell<String>>,
}

impl Gateway {
    /// `url` is `PMOS_GATEWAY_URL`; `/v1` is added if it's missing, as the harness does.
    pub fn new(url: &str, token: &str) -> Self {
        let mut base = url.trim_end_matches('/').to_string();
        if !base.ends_with("/v1") {
            base.push_str("/v1");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("a reqwest client with a timeout always builds");
        Self {
            http,
            base,
            token: token.to_string(),
            source: std::sync::Arc::default(),
        }
    }

    /// The source this run expects: the one the gateway reported at its first roles read.
    async fn expected_source(&self) -> Result<String, String> {
        if let Some(s) = self.source.get() {
            return Ok(s.clone());
        }
        self.models().await?;
        self.source
            .get()
            .cloned()
            .ok_or_else(|| "the gateway didn't say which model source it serves".to_string())
    }

    /// Which model serves each role, as the gateway itself says (`GET /v1/pmos/roles`): the
    /// report's [`crate::report::GatewayModels`]. Never a URL: errors don't carry one.
    pub async fn models(&self) -> Result<crate::report::GatewayModels, String> {
        let resp = self
            .http
            .get(format!("{}/pmos/roles", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("the gateway: {}", e.without_url()))?;
        let code = resp.status();
        if !code.is_success() {
            return Err(format!("the gateway answered {code} to GET /v1/pmos/roles"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("the gateway's roles answer: {}", e.without_url()))?;
        // The first answer of the run fixes the source every judge call names.
        if let Some(s) = v.get("model_source").and_then(Value::as_str) {
            if pmos_contract::model_source::valid_reported(s) {
                let _ = self.source.set(s.to_string());
            }
        }
        crate::report::GatewayModels::from_roles_answer(&v)
    }

    /// Ask a judge about one output: its [`Reply`], or the error.
    pub async fn ask(&self, judge: &Judge, input: &str, output: &str) -> Result<Reply, String> {
        let body = json!({
            "model": judge.alias,
            "temperature": 0,
            "max_tokens": 1024,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": prompt(judge, input, output)},
            ],
        });
        let source = self.expected_source().await?;
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .header("X-Pmos-Priority", "personal")
            .header("X-Pmos-Run", format!("eval-judge:{}", judge.name))
            .header(pmos_contract::model_source::HEADER, &source)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("the gateway: {}", e.without_url()))?;
        let code = resp.status();
        let answered = resp
            .headers()
            .get(pmos_contract::model_source::HEADER)
            .and_then(|v| v.to_str().ok());
        if code.as_u16() == 409 || answered != Some(source.as_str()) {
            return Err(format!(
                "the gateway now serves another model source than this run started on ({source}); \
                 judging stopped (model_source_changed)"
            ));
        }
        let text = resp.text().await.map_err(|e| format!("the gateway: {e}"))?;
        if !code.is_success() {
            let cut: String = text.chars().take(300).collect();
            return Err(format!("the gateway answered {code}: {cut}"));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("the gateway's reply isn't JSON: {e}"))?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("the gateway's reply has no choices[0].message.content")?;
        Ok(parse_reply(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_parse_robustly() {
        for (reply, want) in [
            ("yes", Some(true)),
            ("No", Some(false)),
            ("**Yes.**", Some(true)),
            ("  no\n", Some(false)),
            ("Answer: yes", Some(true)),
            ("<think>it says no port</think>\nYes", Some(true)),
            ("<think>yes or no? hmm", None),
            ("Yes, although no page says so", Some(true)),
            ("I think the answer is no.", Some(false)),
            ("maybe", None),
            ("I can't say yes or no", None),
            ("yesterday", None),
            ("", None),
        ] {
            assert_eq!(parse_verdict(reply), want, "{reply:?}");
        }
    }

    #[test]
    fn unknown_is_its_own_reply() {
        for (reply, want) in [
            ("unknown", Reply::Unknown),
            ("Unknown.", Reply::Unknown),
            ("<think>no source</think> unknown", Reply::Unknown),
            ("The answer is unknown", Reply::Unknown),
            ("yes", Reply::Yes),
            ("no", Reply::No),
            ("yes or unknown", Reply::Yes),
            ("I'd say unknown, maybe no", Reply::Unparsed),
            ("maybe", Reply::Unparsed),
        ] {
            assert_eq!(parse_reply(reply), want, "{reply:?}");
        }
        assert_eq!(parse_verdict("unknown"), None);
    }

    #[test]
    fn labels_are_yes_no_unknown_or_pending() {
        let read = |s: &str| serde_json::from_str::<Label>(s).map(|l| l.label);
        assert_eq!(
            read(r#"{"input":"i","output":"o","label":true}"#).unwrap(),
            Human::Yes
        );
        assert_eq!(
            read(r#"{"input":"i","output":"o","label":false}"#).unwrap(),
            Human::No
        );
        assert_eq!(
            read(r#"{"input":"i","output":"o","label":"unknown"}"#).unwrap(),
            Human::Unknown
        );
        assert_eq!(
            read(r#"{"input":"i","output":"o","label":null}"#).unwrap(),
            Human::Pending
        );
        // The field must be there, even when empty: a line with no `label` is a mistake.
        assert!(read(r#"{"input":"i","output":"o"}"#).is_err());
        assert!(read(r#"{"input":"i","output":"o","label":"yes"}"#).is_err());
        assert!(read(r#"{"input":"i","output":"o","label":1}"#).is_err());
        // A pack line round-trips, its label still empty.
        let line = Label {
            input: "i".into(),
            output: "o".into(),
            label: Human::Pending,
            from: Some("docs-qa/vault-saml-sso run 1".into()),
            judge_said: Some("yes".into()),
        };
        let text = serde_json::to_string(&line).unwrap();
        assert!(text.contains(r#""label":null"#), "{text}");
        assert_eq!(serde_json::from_str::<Label>(&text).unwrap(), line);
    }

    #[test]
    fn calibration_counts_only_yes_and_no_labels() {
        let judge = Judge {
            name: "j".into(),
            failure_mode: "model/x".into(),
            question: "q".into(),
            alias: DEFAULT_ALIAS.into(),
            applies_to: vec!["c".into()],
            labels: "j.labels.jsonl".into(),
            needs_sources: false,
        };
        let labels = [
            Human::Yes,
            Human::Yes,
            Human::No,
            Human::No,
            Human::Unknown,
            Human::Pending,
        ];
        let replies = [
            Reply::Yes,
            Reply::Unknown,
            Reply::No,
            Reply::Yes,
            Reply::Unparsed,
            Reply::Unparsed,
        ];
        let date = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        let c = calibrate(&judge, date, &labels, &replies);
        assert_eq!((c.n, c.positives, c.negatives), (4, 2, 2));
        assert_eq!((c.tpr, c.tnr), (Some(0.5), Some(0.5)));
        assert_eq!((c.unparsed, c.judge_unknown), (1, 1));
        assert_eq!((c.labels_unknown, c.labels_pending), (1, 1));
    }

    #[test]
    fn the_hash_covers_question_and_alias() {
        let a = question_hash("Is it faithful?", "pmos/extraction");
        assert_eq!(a.len(), 64);
        assert_eq!(a, question_hash("Is it faithful?", "pmos/extraction"));
        assert_ne!(a, question_hash("Is it faithful? ", "pmos/extraction"));
        assert_ne!(a, question_hash("Is it faithful?", "pmos/default"));
        // `printf 'q\npmos/x' | sha256sum`
        assert_eq!(
            question_hash("q", "pmos/x"),
            "f0e449d67375f72eebd63a86011e699286b58ca7f704d7db2f8ed1b918625781"
        );
    }
}
