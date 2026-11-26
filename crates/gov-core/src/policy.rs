//! The policy a gate decides from (spec §4.6): rules, evaluated in memory.
//!
//! Refusals win; then every countersignature a matching rule asks for is required; then an
//! allow; and no match at all is a refusal. The templates, the connector map and the approver
//! roster join the bundle in later slices.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::action::{CanonicalAction, Kind};
use crate::countersign::Class;
use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::{Id, ResourcePattern};
use crate::lease::{Freshness, FreshnessLimits};

/// What a rule applies to. An absent field matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operations: Option<BTreeSet<Id>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<BTreeSet<Kind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<ResourcePattern>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    Allow,
    /// Allow once a person in `group` has countersigned, with at least this class. Unless
    /// `issuer_may_approve` is set, the person who issued the mandate may not be that person.
    Countersign {
        class: Class,
        group: Id,
        #[serde(default)]
        issuer_may_approve: bool,
    },
    Refuse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: Id,
    #[serde(rename = "match")]
    pub matches: Match,
    /// What happens when the rule matches.
    pub then: Effect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBundle {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub issuer: Id,
    pub version: Id,
    pub issued_at: i64,
    pub freshness: FreshnessLimits,
    pub rules: Vec<Rule>,
}

document!(PolicyBundle, "gov.policy.v1");

/// The agent a decision is about.
pub struct Subject<'a> {
    pub agent: &'a Id,
    pub labels: &'a BTreeSet<Id>,
}

/// One countersignature policy asks for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Needed {
    pub class: Class,
    pub group: Id,
    pub issuer_may_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Allowed once every one of these is satisfied.
    Countersign(Vec<Needed>),
    /// A rule refuses.
    Refuse,
    /// No rule matched, which is a refusal.
    NoMatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub freshness: Freshness,
    /// The rules that matched, in bundle order.
    pub matched: Vec<Id>,
}

impl Match {
    /// `refusing`: a refusal's `X/*` also covers `X` itself (review finding GR-7).
    fn applies(&self, subject: &Subject<'_>, action: &CanonicalAction, refusing: bool) -> bool {
        self.agent.as_ref().is_none_or(|a| a == subject.agent)
            && self
                .label
                .as_ref()
                .is_none_or(|l| subject.labels.contains(l))
            && self.system.as_ref().is_none_or(|s| s == &action.system)
            && self
                .operations
                .as_ref()
                .is_none_or(|ops| ops.contains(&action.operation))
            && self.kinds.as_ref().is_none_or(|k| k.contains(&action.kind))
            && self.resources.as_ref().is_none_or(|rs| {
                rs.iter().any(|r| {
                    if refusing {
                        r.covers_or_is(&action.resource)
                    } else {
                        r.covers(&action.resource)
                    }
                })
            })
    }
}

impl PolicyBundle {
    /// The lint a bundle must pass before it is signed, and again when a gate loads it. A rule
    /// that lets something through must say which system it is about; rule ids are unique.
    pub fn validate(&self) -> Result<()> {
        self.freshness.validate()?;
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if !ids.insert(&rule.id) {
                return Err(Error::Malformed("two rules share an id"));
            }
            if rule.then != Effect::Refuse && rule.matches.system.is_none() {
                return Err(Error::Malformed("a permitting rule names no system"));
            }
            let empty = rule
                .matches
                .operations
                .as_ref()
                .is_some_and(|s| s.is_empty())
                || rule.matches.kinds.as_ref().is_some_and(|s| s.is_empty())
                || rule
                    .matches
                    .resources
                    .as_ref()
                    .is_some_and(|s| s.is_empty());
            if empty {
                return Err(Error::Malformed("a rule matches an empty list"));
            }
            // A rule about one system cannot name another system's resources.
            if let (Some(system), Some(resources)) = (&rule.matches.system, &rule.matches.resources)
            {
                if resources.iter().any(|r| r.system() != system.as_str()) {
                    return Err(Error::Malformed("a rule's resource is not in its system"));
                }
            }
        }
        Ok(())
    }

    /// Decide `action` for `subject`.
    pub fn evaluate(&self, subject: &Subject<'_>, action: &CanonicalAction) -> Evaluation {
        let matched: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.matches.applies(subject, action, r.then == Effect::Refuse))
            .collect();
        // The strictest class a matching rule names; otherwise by what the action does.
        let freshness =
            matched
                .iter()
                .filter_map(|r| r.freshness)
                .max()
                .unwrap_or(match action.kind {
                    Kind::Destructive => Freshness::Strict,
                    Kind::Read | Kind::Write => Freshness::Standard,
                });
        let mut needed = BTreeSet::new();
        let mut refused = false;
        let mut allowed = false;
        for rule in &matched {
            match &rule.then {
                Effect::Refuse => refused = true,
                Effect::Allow => allowed = true,
                Effect::Countersign {
                    class,
                    group,
                    issuer_may_approve,
                } => {
                    needed.insert(Needed {
                        class: *class,
                        group: group.clone(),
                        issuer_may_approve: *issuer_may_approve,
                    });
                }
            }
        }
        let verdict = if matched.is_empty() {
            Verdict::NoMatch
        } else if refused {
            Verdict::Refuse
        } else if !needed.is_empty() {
            Verdict::Countersign(needed.into_iter().collect())
        } else if allowed {
            Verdict::Allow
        } else {
            Verdict::NoMatch
        };
        Evaluation {
            verdict,
            freshness,
            matched: matched.iter().map(|r| r.id.clone()).collect(),
        }
    }
}
