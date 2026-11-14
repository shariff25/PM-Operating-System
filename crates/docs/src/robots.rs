//! A small `robots.txt` reader: the groups that apply to our user agent (or `*`), with the
//! longest matching `Allow`/`Disallow` rule winning, as RFC 9309 specifies. Patterns support
//! `*` (any run of characters) and a trailing `$` (end of path).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    /// `(allow, path prefix)` rules from the group that applies to us.
    rules: Vec<(bool, String)>,
}

impl Robots {
    /// Parse `text` for `agent` (the product token, such as `pmos-docs`). A group naming
    /// our agent replaces the `*` group.
    pub fn parse(text: &str, agent: &str) -> Self {
        let agent = agent.to_ascii_lowercase();
        let mut star: Vec<(bool, String)> = Vec::new();
        let mut ours: Option<Vec<(bool, String)>> = None;
        // Agents named by the group being read, and whether a rule line has been seen in it.
        let mut agents: Vec<String> = Vec::new();
        let mut in_rules = false;

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" => {
                    if in_rules {
                        agents.clear();
                        in_rules = false;
                    }
                    agents.push(value.to_ascii_lowercase());
                }
                "allow" | "disallow" => {
                    in_rules = true;
                    // An empty Disallow allows everything: no rule to add.
                    if value.is_empty() {
                        continue;
                    }
                    let rule = (key == "allow", value.to_string());
                    if agents.iter().any(|a| a == &agent) {
                        ours.get_or_insert_with(Vec::new).push(rule.clone());
                    }
                    if agents.iter().any(|a| a == "*") {
                        star.push(rule);
                    }
                }
                _ => {}
            }
        }
        Self {
            rules: ours.unwrap_or(star),
        }
    }

    /// Whether `path` (starting with `/`) may be fetched. On a tie in length, `Allow` wins.
    pub fn allows(&self, path: &str) -> bool {
        self.rules
            .iter()
            .filter(|(_, pattern)| matches(pattern, path))
            .max_by_key(|(allow, pattern)| (pattern.len(), *allow))
            .is_none_or(|(allow, _)| *allow)
    }
}

/// RFC 9309 matching: `pattern` matches from the start of `path`; `*` matches any run of
/// characters, and a final `$` anchors the end. Without `$`, the rule is a prefix.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = path.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    if parts.is_empty() {
        return !anchored || rest.is_empty();
    }
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        if last && anchored {
            // The final piece must end the path (after any run the `*` before it took).
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_disallow_allows_everything() {
        let r = Robots::parse("User-agent: *\nDisallow:\n", "pmos-docs");
        assert!(r.allows("/docs/anything.md"));
    }

    #[test]
    fn longest_match_wins_and_our_group_replaces_star() {
        let text = "User-agent: *\nDisallow: /\n\nUser-agent: pmos-docs\nDisallow: /docs/private\nAllow: /docs/private/ok\n";
        let r = Robots::parse(text, "pmos-docs");
        assert!(r.allows("/docs/page.md"), "our group, not `*`");
        assert!(!r.allows("/docs/private/x.md"));
        assert!(r.allows("/docs/private/ok.md"));

        let other = Robots::parse(text, "someone-else");
        assert!(!other.allows("/docs/page.md"));
    }

    #[test]
    fn wildcards_and_end_anchors() {
        let text = "User-agent: *\nDisallow: /*.pdf$\nDisallow: /*/internal/\nDisallow: /exact$\nAllow: /docs/*/internal/public\n";
        let r = Robots::parse(text, "pmos-docs");
        assert!(!r.allows("/files/guide.pdf"));
        assert!(r.allows("/files/guide.pdf.md"), "`$` anchors the end");
        assert!(!r.allows("/docs/dsm/internal/notes.md"));
        assert!(
            r.allows("/docs/dsm/internal/public/page.md"),
            "longer Allow wins"
        );
        assert!(!r.allows("/exact"));
        assert!(r.allows("/exact/more"));
        assert!(r.allows("/docs/page.md"));
    }

    #[test]
    fn matching_rules() {
        assert!(matches("/docs", "/docs/x"));
        assert!(matches("/*", "/anything"));
        assert!(matches("/a*b*c", "/a-x-b-y-c-z"));
        assert!(!matches("/a*b*c$", "/a-x-b-y-c-z"));
        assert!(matches("/a*b*c$", "/a-x-b-y-c"));
        assert!(matches("*.md$", "/docs/x.md"));
        assert!(!matches("/docs$", "/docs/"));
    }

    #[test]
    fn comments_and_multi_agent_groups() {
        let text = "# hi\nUser-agent: a\nUser-agent: *\nDisallow: /tmp # temp\n";
        let r = Robots::parse(text, "pmos-docs");
        assert!(!r.allows("/tmp/x"));
        assert!(r.allows("/docs/x"));
    }
}
