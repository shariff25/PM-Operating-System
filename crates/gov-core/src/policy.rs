//! The policy a gate decides from (spec §4.6): rules, evaluated in memory.
//!
//! Refusals win; then every countersignature a matching rule asks for is required; then an
//! allow; and no match at all is a refusal. The templates, the connector map and the approver
//! roster join the bundle in later slices.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::action::{CanonicalAction, Kind, Reversal};
use crate::connector::ServerMap;
use crate::countersign::Class;
use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::{Digest, Id, ResourcePattern, Scope};
use crate::keys::{KeySet, PublicKey};
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
    /// `false` matches only operations that cannot be put back; `true` only those that can
    /// (spec §4.7). The rule "what cannot be undone needs a person" is one line with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversible: Option<bool>,
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

/// The Authority's online keys, by what each may sign (spec §7.6.1, review finding GR-109).
/// The root key, which signs the bundle, is never among them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnlineKeys {
    /// Mandates are issued under these.
    pub mandates: Vec<PublicKey>,
    /// Leases and the revocation feed are signed with these.
    pub leases: Vec<PublicKey>,
}

/// What an online key is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyRole {
    Mandates,
    Leases,
}

impl OnlineKeys {
    fn all(&self) -> impl Iterator<Item = (KeyRole, &PublicKey)> {
        self.mandates
            .iter()
            .map(|k| (KeyRole::Mandates, k))
            .chain(self.leases.iter().map(|k| (KeyRole::Leases, k)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBundle {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub issuer: Id,
    /// A label for people. Order is decided by `number` alone.
    pub version: Id,
    /// Rises by exactly one with each published bundle, from 1.
    pub number: u64,
    pub issued_at: i64,
    pub keys: OnlineKeys,
    pub freshness: FreshnessLimits,
    pub rules: Vec<Rule>,
    /// The connector map: what each upstream tool is and does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors: Vec<ServerMap>,
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
    fn applies(
        &self,
        subject: &Subject<'_>,
        action: &CanonicalAction,
        reversal: Reversal,
        refusing: bool,
    ) -> bool {
        self.agent.as_ref().is_none_or(|a| a == subject.agent)
            && self.reversible.is_none_or(|r| r == reversal.reversible())
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
        if self.number == 0 {
            return Err(Error::Malformed("a bundle's number starts at 1"));
        }
        // Each role has a key; a key id, and a key, appears once, under one role; and none
        // is a key anyone can forge for.
        if self.keys.mandates.is_empty() || self.keys.leases.is_empty() {
            return Err(Error::Malformed("a role has no key"));
        }
        let mut kids = BTreeSet::new();
        let mut seen: Vec<&PublicKey> = Vec::new();
        for (_, key) in self.keys.all() {
            key.enrolable()?;
            if !kids.insert(&key.kid) || seen.iter().any(|k| k.same_key(key)) {
                return Err(Error::Malformed("a key is listed twice"));
            }
            seen.push(key);
        }
        let mut servers = BTreeSet::new();
        for server in &self.connectors {
            if !servers.insert(&server.server) {
                return Err(Error::Malformed("a server is mapped twice"));
            }
            server.validate()?;
        }
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if !ids.insert(&rule.id) {
                return Err(Error::Malformed("two rules share an id"));
            }
            // No rule can name the kind the map gives what it could not classify.
            if rule
                .matches
                .kinds
                .as_ref()
                .is_some_and(|k| k.contains(&Kind::Unknown))
            {
                return Err(Error::Malformed("a rule names the unknown kind"));
            }
            // A system that does not tell upper case from lower has its resources written in
            // lower case. A rule written otherwise for it would match nothing, and a refusal
            // that matches nothing lets through what it meant to stop (finding GR-66).
            let folded = |system: &Id| {
                self.connectors
                    .iter()
                    .any(|c| c.fold_case && &c.system == system)
            };
            if let Some(resources) = &rule.matches.resources {
                let upper = resources.iter().any(|r| {
                    let text = String::from(r.clone());
                    let system = text.split('/').next().unwrap_or_default();
                    Id::new(system).is_ok_and(|s| folded(&s)) && text != text.to_ascii_lowercase()
                });
                if upper {
                    return Err(Error::Malformed(
                        "a folded system's patterns are lower case",
                    ));
                }
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

    /// The bundle may not name the key that signs it: the root signs bundles and nothing
    /// else, so it is never an online key (review finding GR-109).
    pub fn not_signed_by_its_own(&self, root: &PublicKey) -> Result<()> {
        if self
            .keys
            .all()
            .any(|(_, k)| k.kid == root.kid || k.same_key(root))
        {
            return Err(Error::Malformed("the bundle lists its own signer"));
        }
        Ok(())
    }

    /// Decide `action` for `subject`. `reversal` is the connector map's word on whether the
    /// operation can be put back.
    pub fn evaluate(
        &self,
        subject: &Subject<'_>,
        action: &CanonicalAction,
        reversal: Reversal,
    ) -> Evaluation {
        // A tool the connector map could not classify is refused whatever the rules say.
        if action.kind == Kind::Unknown {
            return Evaluation {
                verdict: Verdict::Refuse,
                freshness: Freshness::Strict,
                matched: Vec::new(),
            };
        }
        let matched: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| {
                r.matches
                    .applies(subject, action, reversal, r.then == Effect::Refuse)
            })
            .collect();
        // The strictest class a matching rule names; otherwise by what the action does.
        let freshness =
            matched
                .iter()
                .filter_map(|r| r.freshness)
                .max()
                .unwrap_or(match action.kind {
                    Kind::Destructive | Kind::Unknown => Freshness::Strict,
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

/// A bundle that opened under the root key and passed every check a gate makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedBundle {
    pub bundle: PolicyBundle,
    /// The hash of the signed bundle: what a record of publishing carries, and what tells
    /// two bundles under one number apart.
    pub digest: Digest,
}

impl OpenedBundle {
    /// What a receipt carries as its policy version: the number and the start of the
    /// digest (review finding GR-129).
    pub fn policy_version(&self) -> Id {
        let short = &self.digest.as_str()[..16];
        Id::new(&format!("{}-{short}", self.bundle.number)).expect("digits and hex make an id")
    }

    pub fn held(&self) -> HeldBundle {
        HeldBundle {
            number: self.bundle.number,
            digest: self.digest.clone(),
        }
    }
}

/// Open a signed bundle under the root key, and nothing but the root key: an online key,
/// or any other, opens no bundle.
pub fn open_bundle(
    envelope: &crate::Envelope,
    root: &PublicKey,
    scope: &Scope,
) -> Result<OpenedBundle> {
    root.enrolable()?;
    let opened = crate::open::<PolicyBundle>(envelope, &KeySet::new([root.clone()]), scope)?;
    opened.doc.validate()?;
    opened.doc.not_signed_by_its_own(root)?;
    Ok(OpenedBundle {
        bundle: opened.doc,
        digest: opened.hash,
    })
}

/// The newest bundle a gate or a shim has accepted, as it keeps it on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldBundle {
    pub number: u64,
    pub digest: Digest,
}

/// Whether a bundle may replace the one held (spec §7.6.1, point 3). A lower number is
/// refused; the same number with another digest is refused, and is an alarm; the same
/// bundle again, or any higher number, is taken.
pub fn may_replace(held: Option<&HeldBundle>, offered: &HeldBundle) -> Result<()> {
    match held {
        Some(h) if offered.number < h.number => Err(Error::BundleOlder),
        Some(h) if offered.number == h.number && offered.digest != h.digest => {
            Err(Error::BundleForked)
        }
        _ => Ok(()),
    }
}

/// What differs between the bundle a signer is about to sign and the one before it, for
/// the signer to read (review finding GR-128). Ids only: never free text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub keys_added: Vec<(KeyRole, Id)>,
    pub keys_removed: Vec<(KeyRole, Id)>,
    pub rules_added: Vec<Id>,
    pub rules_removed: Vec<Id>,
    pub rules_changed: Vec<Id>,
    pub maps_added: Vec<Id>,
    pub maps_removed: Vec<Id>,
    pub maps_changed: Vec<Id>,
    /// The freshness limits differ.
    pub freshness_changed: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self == &Changes::default()
    }
}

/// Compare `next` with `previous`; with no previous bundle, everything is added.
pub fn changes(previous: Option<&PolicyBundle>, next: &PolicyBundle) -> Changes {
    let mut out = Changes::default();
    let empty = OnlineKeys::default();
    let before_keys = previous.map_or(&empty, |p| &p.keys);
    // A key is the same key only if its id, its role and the key itself are the same: a
    // new key under an old id is a key removed and a key added.
    let same = |a: (KeyRole, &PublicKey), b: (KeyRole, &PublicKey)| a.0 == b.0 && a.1 == b.1;
    for key in next.keys.all() {
        if !before_keys.all().any(|k| same(k, key)) {
            out.keys_added.push((key.0, key.1.kid.clone()));
        }
    }
    for key in before_keys.all() {
        if !next.keys.all().any(|k| same(k, key)) {
            out.keys_removed.push((key.0, key.1.kid.clone()));
        }
    }
    let before_rules: &[Rule] = previous.map_or(&[], |p| &p.rules);
    for rule in &next.rules {
        match before_rules.iter().find(|r| r.id == rule.id) {
            None => out.rules_added.push(rule.id.clone()),
            Some(old) if old != rule => out.rules_changed.push(rule.id.clone()),
            Some(_) => {}
        }
    }
    for rule in before_rules {
        if !next.rules.iter().any(|r| r.id == rule.id) {
            out.rules_removed.push(rule.id.clone());
        }
    }
    let before_maps: &[ServerMap] = previous.map_or(&[], |p| &p.connectors);
    for map in &next.connectors {
        match before_maps.iter().find(|m| m.server == map.server) {
            None => out.maps_added.push(map.server.clone()),
            Some(old) if old != map => out.maps_changed.push(map.server.clone()),
            Some(_) => {}
        }
    }
    for map in before_maps {
        if !next.connectors.iter().any(|m| m.server == map.server) {
            out.maps_removed.push(map.server.clone());
        }
    }
    out.freshness_changed = previous.is_some_and(|p| p.freshness != next.freshness);
    out
}
