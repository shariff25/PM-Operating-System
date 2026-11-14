//! Search text helpers shared by every PM OS index (the context library in the tool server,
//! the support docs in `pmos-docs`), so they tokenize and snippet the same way.

/// Lowercased words of two or more characters. Letters and digits only, so the output is
/// safe to join into a Postgres `tsquery` (no operators can survive).
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// The first non-empty line of `body` that mentions a query term, cut to 200 characters.
/// HTML comments and markdown heading markers don't count as content.
pub fn snippet(body: &str, query_terms: &[String]) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("<!--"))
        .find(|l| {
            let lower = l.to_lowercase();
            query_terms.iter().any(|q| lower.contains(q.as_str()))
        })
        .unwrap_or_default()
        .trim_start_matches(['#', '>', ' ']);
    let mut out: String = line.chars().take(200).collect();
    if line.chars().count() > 200 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_drop_operators_and_short_words() {
        assert_eq!(terms("Key & | ! (rotation):* a"), vec!["key", "rotation"]);
        assert!(terms("").is_empty());
    }

    #[test]
    fn snippet_finds_the_first_matching_line() {
        let body = "# Title\n\n<!-- note key -->\nIntro.\n## Key rotation\nRotate keys yearly.";
        assert_eq!(snippet(body, &terms("rotation")), "Key rotation");
        assert_eq!(snippet(body, &terms("yearly")), "Rotate keys yearly.");
        assert_eq!(snippet(body, &terms("zebra")), "");
        let long = format!("key {}", "x".repeat(300));
        assert!(snippet(&long, &terms("key")).ends_with('…'));
    }
}
