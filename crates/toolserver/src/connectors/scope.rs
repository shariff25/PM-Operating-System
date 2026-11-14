//! A team bot's read scope (decision 26 in `docs/bot-decisions.md`, `docs/bot-runtime-spec.md`
//! §2), applied by each connector:
//!
//! | Tool | How |
//! | --- | --- |
//! | `jira.search` | the JQL becomes `project in (…) AND (<jql>)` |
//! | `jira.read` | the issue key's project must be listed (checked again on the issue returned) |
//! | `confluence.search` | the CQL becomes `space in (…) AND (<cql>)` |
//! | `confluence.read` | the page's space must be listed, checked after the fetch |
//! | `slack.search` | results outside the listed channels are dropped |
//! | `slack.read_thread` | the channel must be listed |
//!
//! An empty list means no access to that system. Anything out of scope is
//! [`ConnectorError::Forbidden`], which the tool server returns as `403` and audits as
//! `denied`.
//!
//! The scope's keys are checked against a strict pattern before they reach a query, and a
//! query is wrapped only once its quotes are closed and its parentheses balanced, so it can't
//! reach outside the `( … )` it is put in.

use super::{ConnectorError, TeamBot};

/// A Jira project key or Confluence space key in a read scope: `^[A-Z][A-Z0-9_]{0,19}$`.
pub fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && key.len() <= 20
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A Slack channel id in a read scope: `C…` or `G…`, then uppercase letters and digits.
pub fn valid_channel(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('C' | 'G'))
        && (3..=32).contains(&id.len())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

impl<'a> TeamBot<'a> {
    fn denied(&self, why: String) -> ConnectorError {
        ConnectorError::Forbidden(why)
    }

    /// The listed entries, if there is at least one and every one is well-formed.
    fn listed(
        &self,
        list: &'a [String],
        system: &str,
        noun: &str,
        valid: fn(&str) -> bool,
    ) -> Result<&'a [String], ConnectorError> {
        if list.is_empty() {
            return Err(self.denied(format!(
                "the {} bot may not read {system}: its read scope lists no {noun}s",
                self.name
            )));
        }
        if let Some(bad) = list.iter().find(|k| !valid(k)) {
            return Err(self.denied(format!(
                "the {} bot's read scope lists `{bad}`, which isn't a valid {noun}; ask its owner to fix its BOT.md",
                self.name
            )));
        }
        Ok(list)
    }

    pub fn jira_projects(&self) -> Result<&'a [String], ConnectorError> {
        self.listed(&self.reads.jira_projects, "Jira", "Jira project", valid_key)
    }

    pub fn confluence_spaces(&self) -> Result<&'a [String], ConnectorError> {
        self.listed(
            &self.reads.confluence_spaces,
            "Confluence",
            "Confluence space",
            valid_key,
        )
    }

    pub fn slack_channels(&self) -> Result<&'a [String], ConnectorError> {
        self.listed(
            &self.reads.slack_channels,
            "Slack",
            "Slack channel id",
            valid_channel,
        )
    }

    /// `jira.search`: `project in (…) AND (<jql>)`.
    pub fn jql(&self, jql: &str) -> Result<String, ConnectorError> {
        wrap(jql, "project", self.jira_projects()?, "JQL")
    }

    /// `confluence.search`: `space in (…) AND (<cql>)`.
    pub fn cql(&self, cql: &str) -> Result<String, ConnectorError> {
        wrap(cql, "space", self.confluence_spaces()?, "CQL")
    }

    /// `jira.read`: the issue's project must be listed.
    pub fn check_issue(&self, key: &str) -> Result<(), ConnectorError> {
        let projects = self.jira_projects()?;
        let project = key.split_once('-').map_or(key, |(p, _)| p);
        if projects.iter().any(|p| p == project) {
            Ok(())
        } else {
            Err(self.denied(format!(
                "{key} is in Jira project {project}, outside the {} bot's read scope ({})",
                self.name,
                projects.join(", ")
            )))
        }
    }

    /// `confluence.read`, when the page's space is known by key.
    pub fn check_space(&self, page: &str, space: &str) -> Result<(), ConnectorError> {
        let spaces = self.confluence_spaces()?;
        if spaces.iter().any(|s| s == space) {
            Ok(())
        } else {
            Err(self.outside_spaces(page, spaces))
        }
    }

    pub(crate) fn outside_spaces(&self, page: &str, spaces: &[String]) -> ConnectorError {
        self.denied(format!(
            "page {page} is in a Confluence space outside the {} bot's read scope ({})",
            self.name,
            spaces.join(", ")
        ))
    }

    /// `slack.read_thread`: the channel must be listed.
    pub fn check_channel(&self, channel: &str) -> Result<(), ConnectorError> {
        let channels = self.slack_channels()?;
        if channels.iter().any(|c| c == channel) {
            Ok(())
        } else {
            Err(self.denied(format!(
                "channel {channel} is outside the {} bot's read scope ({})",
                self.name,
                channels.join(", ")
            )))
        }
    }
}

/// `<field> in ("A", "B") AND (<query>) [ORDER BY …]`. `keys` are already checked by
/// [`valid_key`], so quoting them is enough.
fn wrap(
    query: &str,
    field: &str,
    keys: &[String],
    language: &str,
) -> Result<String, ConnectorError> {
    debug_assert!(keys.iter().all(|k| valid_key(k)));
    let (condition, order) = split(query, language)?;
    let list: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
    let mut out = format!("{field} in ({})", list.join(", "));
    if !condition.is_empty() {
        out.push_str(&format!(" AND ({condition})"));
    }
    if let Some(order) = order {
        out.push(' ');
        out.push_str(order);
    }
    Ok(out)
}

/// A query's condition and its `ORDER BY` clause (which must stay outside the group).
/// Refuses a query with an unclosed quote, unbalanced parentheses, a backslash outside a
/// quote, or an `ORDER BY` that isn't a plain list of fields.
fn split<'q>(query: &'q str, language: &str) -> Result<(&'q str, Option<&'q str>), ConnectorError> {
    let bad = |why: &str| ConnectorError::BadArgs(format!("the {language} {why}"));
    let bytes = query.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut order_at = None;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'\\' => return Err(bad("has a backslash outside quotes")),
            b'(' => depth += 1,
            b')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| bad("has unbalanced parentheses"))?
            }
            _ if depth == 0 && order_at.is_none() && order_by_at(bytes, i).is_some() => {
                order_at = Some(i)
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return Err(bad("has an unclosed quote"));
    }
    if depth != 0 {
        return Err(bad("has unbalanced parentheses"));
    }
    let Some(at) = order_at else {
        return Ok((query.trim(), None));
    };
    let fields_from = order_by_at(bytes, at).expect("matched above");
    let fields = &query[fields_from..];
    let plain = !fields.trim().is_empty()
        && fields.split(',').all(|item| {
            let mut words = item.split_whitespace();
            let field_ok = words.next().is_some_and(|f| {
                f.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '[' | ']'))
            });
            let dir_ok = words
                .next()
                .is_none_or(|d| d.eq_ignore_ascii_case("asc") || d.eq_ignore_ascii_case("desc"));
            field_ok && dir_ok && words.next().is_none()
        });
    if !plain {
        return Err(bad(
            "ORDER BY must be a list of fields, each optionally ASC or DESC",
        ));
    }
    Ok((query[..at].trim(), Some(query[at..].trim())))
}

/// If `ORDER BY` (any case, any spacing) starts at `i` as whole words, the index just after
/// `BY`.
fn order_by_at(bytes: &[u8], i: usize) -> Option<usize> {
    let word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if i > 0 && word_char(bytes[i - 1]) {
        return None;
    }
    let rest = &bytes[i..];
    if rest.len() < 5 || !rest[..5].eq_ignore_ascii_case(b"order") {
        return None;
    }
    let mut j = 5;
    let gap = rest[j..]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    if gap == 0 {
        return None;
    }
    j += gap;
    if rest.len() < j + 2 || !rest[j..j + 2].eq_ignore_ascii_case(b"by") {
        return None;
    }
    j += 2;
    match rest.get(j) {
        None => Some(i + j),
        Some(b) if b.is_ascii_whitespace() => Some(i + j),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmos_contract::bots::Reads;

    fn reads() -> Reads {
        Reads {
            jira_projects: vec!["PM".into(), "ENG".into()],
            confluence_spaces: vec!["PM".into()],
            slack_channels: vec!["C0FIELD".into()],
        }
    }

    fn bot(reads: &Reads) -> TeamBot<'_> {
        TeamBot {
            name: "scout",
            identity: "bots.scout",
            reads,
        }
    }

    fn bad_args(r: Result<String, ConnectorError>) -> bool {
        matches!(r, Err(ConnectorError::BadArgs(_)))
    }

    #[test]
    fn keys_and_channels() {
        for good in ["PM", "ENG2", "A_B", "ABCDEFGHIJKLMNOPQRST"] {
            assert!(valid_key(good), "{good}");
        }
        for bad in [
            "",
            "pm",
            "2PM",
            "PM-1",
            "~ana",
            "PM\"",
            "P M",
            "ABCDEFGHIJKLMNOPQRSTU",
            "PM) OR (x",
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
        assert!(valid_channel("C0FIELD"));
        assert!(valid_channel("G01ABC"));
        for bad in ["D0DM", "U0ANA", "C1", "c0field", "C0&x", "#launch"] {
            assert!(!valid_channel(bad), "{bad}");
        }
    }

    #[test]
    fn queries_are_wrapped_in_the_scope() {
        let r = reads();
        let b = bot(&r);
        assert_eq!(
            b.jql("text ~ SSO").unwrap(),
            r#"project in ("PM", "ENG") AND (text ~ SSO)"#
        );
        assert_eq!(
            b.jql("status = Done order  BY created DESC, cf[10010] asc")
                .unwrap(),
            r#"project in ("PM", "ENG") AND (status = Done) order  BY created DESC, cf[10010] asc"#
        );
        assert_eq!(
            b.jql("ORDER BY updated").unwrap(),
            r#"project in ("PM", "ENG") ORDER BY updated"#
        );
        assert_eq!(
            b.cql("type = page AND text ~ \"SSO\"").unwrap(),
            r#"space in ("PM") AND (type = page AND text ~ "SSO")"#
        );
        // `order by` inside a quote, or inside a word, is just text.
        assert_eq!(
            b.jql(r#"text ~ "order by x" OR summary ~ border"#).unwrap(),
            r#"project in ("PM", "ENG") AND (text ~ "order by x" OR summary ~ border)"#
        );
        // A quoted parenthesis stays inside its string.
        assert_eq!(
            b.jql(r#"summary ~ "a) OR project = SECRET OR (b""#)
                .unwrap(),
            r#"project in ("PM", "ENG") AND (summary ~ "a) OR project = SECRET OR (b")"#
        );
        assert_eq!(
            b.jql(r#"summary ~ "say \"hi\" (x""#).unwrap(),
            r#"project in ("PM", "ENG") AND (summary ~ "say \"hi\" (x")"#
        );
    }

    #[test]
    fn queries_that_could_escape_the_scope_are_refused() {
        let r = reads();
        let b = bot(&r);
        for escape in [
            "x) OR (project = SECRET",
            "x) OR project = SECRET OR (y",
            ") OR (",
            "(x",
            r#"summary ~ "unclosed"#,
            r#"summary ~ 'a) OR (b"#,
            r#"summary ~ \"x) OR project = SECRET OR (y ~ \""#,
            "x ORDER BY created OR project = SECRET",
            r#"x ORDER BY "created""#,
            "x ORDER BY created) OR (project = SECRET",
            "x ORDER BY",
        ] {
            assert!(bad_args(b.jql(escape)), "{escape}");
            assert!(bad_args(b.cql(escape)), "{escape}");
        }
    }

    #[test]
    fn reads_outside_the_scope_are_forbidden() {
        let r = reads();
        let b = bot(&r);
        assert!(b.check_issue("PM-1").is_ok());
        assert!(b.check_issue("ENG-22").is_ok());
        let err = b.check_issue("SECRET-1").unwrap_err();
        assert_eq!(
            err,
            ConnectorError::Forbidden(
                "SECRET-1 is in Jira project SECRET, outside the scout bot's read scope (PM, ENG)"
                    .into()
            )
        );
        assert!(b.check_issue("PMX-1").is_err(), "a prefix is not a match");
        assert!(b.check_space("9001", "PM").is_ok());
        assert!(b.check_space("9001", "HR").is_err());
        assert!(b.check_channel("C0FIELD").is_ok());
        assert!(b.check_channel("C0SECRET").is_err());
    }

    #[test]
    fn an_empty_or_malformed_list_means_no_access() {
        let empty = Reads::default();
        let b = bot(&empty);
        for err in [
            b.jql("x").unwrap_err(),
            b.cql("x").unwrap_err(),
            b.check_issue("PM-1").unwrap_err(),
            b.check_space("1", "PM").unwrap_err(),
            b.check_channel("C0FIELD").unwrap_err(),
        ] {
            assert!(
                matches!(err, ConnectorError::Forbidden(ref m) if m.contains("lists no")),
                "{err:?}"
            );
        }
        let malformed = Reads {
            jira_projects: vec!["PM".into(), "pm) OR (x".into()],
            confluence_spaces: vec!["~ana".into()],
            slack_channels: vec!["#launch".into()],
        };
        let b = bot(&malformed);
        for err in [
            b.jql("x").unwrap_err(),
            b.check_issue("PM-1").unwrap_err(),
            b.cql("x").unwrap_err(),
            b.check_channel("C0FIELD").unwrap_err(),
        ] {
            assert!(
                matches!(err, ConnectorError::Forbidden(ref m) if m.contains("isn't a valid")),
                "{err:?}"
            );
        }
    }
}
