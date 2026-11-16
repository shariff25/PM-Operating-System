//! The words of the nudges, as data, so the owner can edit them without touching the engine.
//!
//! Each [`Line`] has an id (kept in a PM's state so they never see it twice), the kind of
//! nudge it belongs to, and two forms: `one` for a single item and `many` for several.
//!
//! **Placeholders**, replaced when the line is shown:
//! - `{Count}`: the number as a word with a capital ("Three"), or digits above twelve.
//! - `{count}`: the same, lower case.
//! - `{expires}`: when the soonest item expires, as "in 24 hours" or "in under an hour".
//!
//! A line with `expires: true` says something about expiry, so it is only used when every
//! item really does expire; for items that don't, the engine picks a line without it.
//! **Voice:** gentle, lightly funny, about the work and never about the person, a missed
//! deadline or a customer's data. Add a line by adding a row; remove one by deleting it (a
//! PM who already saw it simply never sees it again).

use chrono::Duration;

/// The kind id of the drafts-waiting nudge.
pub const DRAFTS_WAITING: &str = "drafts_waiting";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub id: &'static str,
    pub kind: &'static str,
    /// Mentions `{expires}`; only used when every item expires.
    pub expires: bool,
    pub one: &'static str,
    pub many: &'static str,
}

/// Every line, in the order they are tried.
pub const LINES: &[Line] = &[
    Line {
        id: "dw-expire",
        kind: DRAFTS_WAITING,
        expires: true,
        one: "One draft is waiting for you. It expires {expires}, and it knows it.",
        many: "{Count} drafts are waiting for you. They expire {expires}, and they know it.",
    },
    Line {
        id: "dw-patient",
        kind: DRAFTS_WAITING,
        expires: false,
        one: "One draft is sitting in your inbox, being very patient about it.",
        many: "{Count} drafts are sitting in your inbox, being very patient about it.",
    },
    Line {
        id: "dw-pacing",
        kind: DRAFTS_WAITING,
        expires: false,
        one: "One draft would like a quick look from you. It isn't rushing you, only pacing a little.",
        many: "{Count} drafts would like a quick look from you. They aren't rushing you, only pacing a little.",
    },
];

/// A number as a word up to twelve, digits above.
pub fn number_word(n: usize) -> String {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    WORDS
        .get(n)
        .map_or_else(|| n.to_string(), |w| (*w).to_string())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// "in 24 hours", "in 1 hour", "in under an hour".
pub fn expiry_phrase(left: Duration) -> String {
    match left.num_hours() {
        h if h < 1 => "in under an hour".into(),
        1 => "in 1 hour".into(),
        h => format!("in {h} hours"),
    }
}

/// The line's text for `count` items, expiring in `expires_in` (if they do).
pub fn render(line: &Line, count: usize, expires_in: Option<Duration>) -> String {
    let template = if count == 1 { line.one } else { line.many };
    let word = number_word(count);
    template
        .replace("{Count}", &capitalize(&word))
        .replace("{count}", &word)
        .replace(
            "{expires}",
            &expires_in.map(expiry_phrase).unwrap_or_default(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for line in LINES {
            assert!(ids.insert(line.id), "duplicate id {}", line.id);
            assert_eq!(
                line.expires,
                line.one.contains("{expires}") && line.many.contains("{expires}"),
                "{}: `expires` must match the placeholder in both forms",
                line.id
            );
            for form in [line.one, line.many] {
                assert!(form.ends_with('.'), "{}", line.id);
            }
        }
    }

    #[test]
    fn the_first_line_reads_as_written() {
        let line = &LINES[0];
        assert_eq!(
            render(line, 3, Some(Duration::hours(24))),
            "Three drafts are waiting for you. They expire in 24 hours, and they know it."
        );
        assert_eq!(
            render(line, 1, Some(Duration::minutes(90))),
            "One draft is waiting for you. It expires in 1 hour, and it knows it."
        );
        assert_eq!(
            render(line, 2, Some(Duration::minutes(20))),
            "Two drafts are waiting for you. They expire in under an hour, and they know it."
        );
    }

    #[test]
    fn counts_are_words_then_digits() {
        assert_eq!(number_word(12), "twelve");
        assert_eq!(number_word(13), "13");
        assert!(render(&LINES[1], 40, None).starts_with("40 drafts"));
        assert!(render(&LINES[2], 1, None).starts_with("One draft would"));
    }
}
