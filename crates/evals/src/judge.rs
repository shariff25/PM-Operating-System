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

/// One labelled example: what the judge would see, and a person's answer to its question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub input: String,
    pub output: String,
    /// `true`: the person answered "yes" (the output passes).
    pub label: bool,
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
    /// Replies that were neither yes nor no; each counts as a wrong verdict.
    pub unparsed: usize,
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

/// Calibration numbers from labels and the judge's verdicts on them, in the same order. An
/// unparsed verdict counts as wrong.
pub fn calibrate(
    judge: &Judge,
    date: NaiveDate,
    labels: &[bool],
    verdicts: &[Option<bool>],
) -> Calibration {
    assert_eq!(labels.len(), verdicts.len(), "one verdict per label");
    let (mut pos, mut neg, mut tp, mut tn, mut unparsed) = (0, 0, 0, 0, 0);
    for (label, verdict) in labels.iter().zip(verdicts) {
        if verdict.is_none() {
            unparsed += 1;
        }
        if *label {
            pos += 1;
            tp += usize::from(*verdict == Some(true));
        } else {
            neg += 1;
            tn += usize::from(*verdict == Some(false));
        }
    }
    let rate = |hit: usize, of: usize| (of > 0).then(|| hit as f64 / of as f64);
    Calibration {
        judge: judge.name.clone(),
        date,
        n: labels.len(),
        positives: pos,
        negatives: neg,
        tpr: rate(tp, pos),
        tnr: rate(tn, neg),
        unparsed,
        alias: judge.alias.clone(),
        question_sha256: judge.hash(),
    }
}

/// A judge's reply as a verdict. Reasoning in `<think>…</think>` is ignored. The reply's first
/// word decides when it is yes or no; otherwise a reply that says only one of yes and no (as a
/// word) is that. Anything else is `None`.
pub fn parse_verdict(reply: &str) -> Option<bool> {
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
        Some(&"yes") => return Some(true),
        Some(&"no") => return Some(false),
        _ => {}
    }
    let yes = words.contains(&"yes");
    let no = words.contains(&"no");
    match (yes, no) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

const SYSTEM: &str = "You grade an AI assistant's work. You answer one yes-or-no question about \
it. Reply with exactly one word: yes or no.";

/// The user message a judge is asked with.
pub fn prompt(judge: &Judge, input: &str, output: &str) -> String {
    format!(
        "Question: {}\n\n<input>\n{}\n</input>\n\n<output>\n{}\n</output>\n\nAnswer the question with exactly one word: yes or no.",
        judge.question.trim(),
        input.trim(),
        output.trim()
    )
}

/// The gateway, as client `evals` (`PMOS_EVALS_GATEWAY_TOKEN`).
#[derive(Clone)]
pub struct Gateway {
    http: reqwest::Client,
    /// The OpenAI base, ending in `/v1`.
    base: String,
    token: String,
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
        }
    }

    /// Ask a judge about one output: `Ok(Some(verdict))`, `Ok(None)` when the reply was
    /// neither yes nor no, or the error.
    pub async fn ask(
        &self,
        judge: &Judge,
        input: &str,
        output: &str,
    ) -> Result<Option<bool>, String> {
        let body = json!({
            "model": judge.alias,
            "temperature": 0,
            "max_tokens": 1024,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": prompt(judge, input, output)},
            ],
        });
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .header("X-Pmos-Priority", "personal")
            .header("X-Pmos-Run", format!("eval-judge:{}", judge.name))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("the gateway: {e}"))?;
        let code = resp.status();
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
        Ok(parse_verdict(content))
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
