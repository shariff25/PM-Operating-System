//! The only string types a signed document may carry: an identifier, a resource path, a
//! resource pattern and a digest. None can hold free text (spec §4.5, threat T17).

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};

const MAX_ID: usize = 128;
const MAX_SEGMENTS: usize = 16;

fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '-')
}

fn check_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > MAX_ID || !s.chars().all(id_char) {
        return Err(Error::Malformed("identifier"));
    }
    Ok(())
}

/// An identifier: 1 to 128 characters of `A-Z a-z 0-9 . _ : @ -`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl Id {
    pub fn new(s: &str) -> Result<Self> {
        check_id(s)?;
        Ok(Id(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        check_id(&s)?;
        Ok(Id(s))
    }
}

impl From<Id> for String {
    fn from(id: Id) -> String {
        id.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn check_segments(s: &str) -> Result<()> {
    let mut n = 0;
    for seg in s.split('/') {
        check_id(seg).map_err(|_| Error::Malformed("resource"))?;
        n += 1;
    }
    if n > MAX_SEGMENTS {
        return Err(Error::Malformed("resource"));
    }
    Ok(())
}

/// A concrete resource: identifiers joined by `/`, the first being the system
/// (`jira/project/PAY/issue/PAY-12`). At most 16 segments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourcePath(String);

impl ResourcePath {
    pub fn new(s: &str) -> Result<Self> {
        check_segments(s)?;
        Ok(ResourcePath(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
    /// The first segment.
    pub fn system(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }
}

impl TryFrom<String> for ResourcePath {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        check_segments(&s)?;
        Ok(ResourcePath(s))
    }
}

impl From<ResourcePath> for String {
    fn from(p: ResourcePath) -> String {
        p.0
    }
}

/// A resource pattern: an exact path, or a path ending in `/*`, which stands for one or more
/// further whole segments. No other wildcard exists, so "is inside" is decidable (spec §4.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourcePattern {
    prefix: ResourcePath,
    wildcard: bool,
}

impl ResourcePattern {
    pub fn new(s: &str) -> Result<Self> {
        match s.strip_suffix("/*") {
            Some(prefix) => Ok(ResourcePattern {
                prefix: ResourcePath::new(prefix)?,
                wildcard: true,
            }),
            None => Ok(ResourcePattern {
                prefix: ResourcePath::new(s)?,
                wildcard: false,
            }),
        }
    }

    pub fn system(&self) -> &str {
        self.prefix.system()
    }

    /// Does this pattern match `path`?
    pub fn covers(&self, path: &ResourcePath) -> bool {
        if !self.wildcard {
            return self.prefix == *path;
        }
        let mut mine = self.prefix.segments();
        let mut theirs = path.segments();
        loop {
            match (mine.next(), theirs.next()) {
                (Some(a), Some(b)) if a == b => continue,
                // The prefix is used up and the path goes on: one or more further segments.
                (None, Some(_)) => return true,
                _ => return false,
            }
        }
    }

    /// Is every path this pattern matches also matched by `parent`?
    pub fn within(&self, parent: &ResourcePattern) -> bool {
        match (self.wildcard, parent.wildcard) {
            (false, _) => parent.covers(&self.prefix),
            (true, false) => false,
            (true, true) => {
                let mut mine = self.prefix.segments();
                parent.prefix.segments().all(|seg| mine.next() == Some(seg))
            }
        }
    }
}

impl TryFrom<String> for ResourcePattern {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        ResourcePattern::new(&s)
    }
}

impl From<ResourcePattern> for String {
    fn from(p: ResourcePattern) -> String {
        if p.wildcard {
            format!("{}/*", p.prefix.0)
        } else {
            p.prefix.0
        }
    }
}

/// A SHA-256 digest as 64 lower-case hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    /// SHA-256 over the parts, in order.
    pub fn of(parts: &[&[u8]]) -> Self {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        Digest(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        let ok = s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Digest(s))
        } else {
            Err(Error::Malformed("digest"))
        }
    }
}

impl From<Digest> for String {
    fn from(d: Digest) -> String {
        d.0
    }
}

/// The tenant and environment a verifier belongs to. Every document names both, and a verifier
/// refuses any other (threat T19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub tenant: Id,
    pub env: Id,
}

impl Scope {
    pub fn new(tenant: &str, env: &str) -> Result<Self> {
        Ok(Scope {
            tenant: Id::new(tenant)?,
            env: Id::new(env)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_refuse_free_text() {
        assert!(Id::new("jira.issue-create_1:a@b").is_ok());
        for bad in ["", "two words", "a/b", "tab\there", "é", &"x".repeat(129)] {
            assert!(Id::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn patterns_match_whole_segments() {
        let p = ResourcePattern::new("jira/project/PAY/*").unwrap();
        let path = |s| ResourcePath::new(s).unwrap();
        assert!(p.covers(&path("jira/project/PAY/issue/PAY-1")));
        assert!(!p.covers(&path("jira/project/PAY")));
        assert!(!p.covers(&path("jira/project/PAYROLL/issue/1")));
        assert!(!p.covers(&path("jira/project")));
        let exact = ResourcePattern::new("jira/project/PAY").unwrap();
        assert!(exact.covers(&path("jira/project/PAY")));
        assert!(!exact.covers(&path("jira/project/PAY/issue/1")));
        for bad in [
            "*", "jira/*/x", "jira//x", "jira/*x", "jira/x*", "/jira", "jira/",
        ] {
            assert!(ResourcePattern::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn within_is_about_what_is_matched() {
        let p = |s| ResourcePattern::new(s).unwrap();
        assert!(p("jira/project/PAY/*").within(&p("jira/project/*")));
        assert!(p("jira/project/PAY/*").within(&p("jira/project/PAY/*")));
        assert!(p("jira/project/PAY/issue/1").within(&p("jira/project/PAY/*")));
        assert!(!p("jira/project/*").within(&p("jira/project/PAY/*")));
        assert!(!p("jira/project/PAY/*").within(&p("jira/project/PAY")));
        assert!(!p("jira/project/PAY").within(&p("jira/project/PAY/*")));
        assert!(!p("jira/project/PAYROLL/*").within(&p("jira/project/PAY/*")));
    }
}
