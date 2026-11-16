//! Source markers in drafts: how a bot says which source a sentence came from, and how the
//! markers are read, resolved and removed. See `docs/web-api.md` (Drafts, Citations).
//!
//! **The bot's form.** Every item a read tool returns carries a `ref` (`src-3f9a1c`, made by
//! the tool server from the item's id; see `crates/toolserver/src/sources.rs`). A sentence in
//! a draft that relies on an item ends with a marker naming it: `[[src-3f9a1c]]`, or several
//! at once, `[[src-3f9a1c]][[src-0b77d2]]` (also accepted: `[[src-3f9a1c, src-0b77d2]]`).
//!
//! **The display form.** `pmos-web` rewrites each marker it can resolve to a source the task
//! really read as `[[s3]]`, where `s3` is that source's `id` in the draft's `sources`.
//!
//! **What counts as a marker** ([`parse`]), outside fenced code blocks and inline code spans
//! (text there is code, never a citation, and is left exactly as written):
//! - `[[`, optional spaces, `src-` (any case), then anything but `[`, `]` and a line break,
//!   then `]]`, on one line, at most [`MAX_MARKER`] bytes in all. Its tokens (split on
//!   spaces, `,` and `;`) that are `src-` and 1 to 32 letters or digits are refs (lowercased);
//!   any other token is malformed and names nothing.
//! - The display form itself, `[[s` and 1 to 3 digits `]]`. In stored text it was never put
//!   there by `pmos-web`, so it names nothing: it is removed like a malformed marker, so a
//!   reader of the display form can trust every `[[sN]]` it sees.
//!
//! Everything else is text: `[[roadmap]]`, `[[src-` with no closing `]]`, `[src-1]`.
//!
//! **Removing** ([`strip`], and [`render`] for a marker group that resolves to nothing)
//! leaves clean text: the spaces before a marker go, so `claim [[src-a1]].` becomes
//! `claim.`, a marker between two words leaves one space, and a marker at the start of a
//! line takes the spaces after it with it.

/// The longest marker, `[[` to `]]` inclusive, in bytes.
pub const MAX_MARKER: usize = 200;
/// The longest ref after `src-`.
pub const MAX_REF: usize = 32;
/// What every ref starts with.
pub const REF_PREFIX: &str = "src-";

/// One marker as written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Marker {
    /// The refs it names, lowercased, in order, without repeats.
    pub refs: Vec<String>,
    /// Tokens that aren't refs, and display-form markers, which name nothing.
    pub malformed: usize,
}

/// A run of a body: text (code included, verbatim) or a marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Marker(Marker),
}

/// What [`render`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    pub text: String,
    /// Refs the resolver resolved, counted once per marker group.
    pub kept: usize,
    /// Refs it didn't, plus malformed tokens and display-form markers.
    pub dropped: usize,
}

/// Is `token` a ref: `src-` and 1 to [`MAX_REF`] ASCII letters or digits?
pub fn is_ref(token: &str) -> bool {
    token.len() > REF_PREFIX.len()
        && token.len() <= REF_PREFIX.len() + MAX_REF
        && token[..REF_PREFIX.len()].eq_ignore_ascii_case(REF_PREFIX)
        && token[REF_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric())
}

/// The marker starting at the beginning of `s` (which starts with `[[`) and its length.
fn marker_at(s: &str) -> Option<(Marker, usize)> {
    let b = s.as_bytes();
    if !s.starts_with("[[") {
        return None;
    }
    // The display form: `[[s` 1–3 digits `]]`.
    if b.get(2) == Some(&b's') {
        let digits = b[3..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=3).contains(&digits) && s[3 + digits..].starts_with("]]") {
            return Some((
                Marker {
                    refs: Vec::new(),
                    malformed: 1,
                },
                3 + digits + 2,
            ));
        }
    }
    let inner_start = 2 + s[2..].len() - s[2..].trim_start_matches([' ', '\t']).len();
    let head = s.get(inner_start..inner_start + REF_PREFIX.len())?;
    if !head.eq_ignore_ascii_case(REF_PREFIX) {
        return None;
    }
    let close = s[inner_start..].find("]]")? + inner_start;
    let inner = &s[2..close];
    if close + 2 > MAX_MARKER || inner.contains(['[', ']', '\n', '\r']) {
        return None;
    }
    let mut marker = Marker::default();
    for token in inner.split([' ', '\t', ',', ';']).filter(|t| !t.is_empty()) {
        if is_ref(token) {
            let r = token.to_ascii_lowercase();
            if !marker.refs.contains(&r) {
                marker.refs.push(r);
            }
        } else {
            marker.malformed += 1;
        }
    }
    Some((marker, close + 2))
}

/// A fence line (up to three spaces, then three or more `` ` `` or `~`): its character and
/// length.
fn fence(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let c = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = rest.chars().take_while(|x| *x == c).count();
    // An opening backtick fence's info string can't contain a backtick.
    (n >= 3 && !(c == '`' && rest[n..].contains('`'))).then_some((c, n))
}

/// Add `text[from..to]` to `out` as code or prose, widening the last range when it is the
/// same kind.
fn push_block(out: &mut Vec<(bool, usize, usize)>, code: bool, from: usize, to: usize) {
    if to <= from {
        return;
    }
    match out.last_mut() {
        Some((c, _, end)) if *c == code => *end = to,
        _ => out.push((code, from, to)),
    }
}

/// Split `text` into prose and fenced code: `(is_code, slice)`, in order, covering it all.
fn blocks(text: &str) -> Vec<(bool, &str)> {
    let mut ranges: Vec<(bool, usize, usize)> = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut start = 0;
    let mut pos = 0;
    let out = &mut ranges;
    for line in text.split_inclusive('\n') {
        let end = pos + line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        match open {
            None => {
                if let Some(f) = fence(bare) {
                    push_block(out, false, start, pos);
                    start = pos;
                    open = Some(f);
                }
            }
            Some((c, n)) => {
                let t = bare.trim_start_matches(' ');
                let closes = bare.len() - t.len() <= 3
                    && t.chars().take_while(|x| *x == c).count() >= n
                    && t.trim_start_matches(c).trim().is_empty();
                if closes {
                    push_block(out, true, start, end);
                    start = end;
                    open = None;
                }
            }
        }
        pos = end;
    }
    // An unclosed fence runs to the end, as in CommonMark.
    push_block(out, open.is_some(), start, text.len());
    ranges
        .into_iter()
        .map(|(code, from, to)| (code, &text[from..to]))
        .collect()
}

/// Where the inline code span opened by the backtick run at `at` ends (just past its closing
/// run), if it closes before a blank line.
fn code_span_end(s: &str, at: usize) -> Option<usize> {
    let b = s.as_bytes();
    let n = b[at..].iter().take_while(|c| **c == b'`').count();
    let mut i = at + n;
    while i < b.len() {
        if b[i] == b'\n' {
            // A blank line ends the paragraph, and with it any span.
            let rest = &s[i + 1..];
            let line = rest.split('\n').next().unwrap_or_default();
            if line.trim().is_empty() {
                return None;
            }
        }
        if b[i] == b'`' {
            let m = b[i..].iter().take_while(|c| **c == b'`').count();
            if m == n {
                return Some(i + m);
            }
            i += m;
            continue;
        }
        i += 1;
    }
    None
}

/// Split prose (no fenced code) into pieces.
fn parse_prose<'a>(s: &'a str, out: &mut Vec<Piece<'a>>) {
    let b = s.as_bytes();
    let mut text_from = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => {
                let n = b[i..].iter().take_while(|c| **c == b'`').count();
                i = code_span_end(s, i).unwrap_or(i + n);
            }
            b'[' => match marker_at(&s[i..]) {
                Some((marker, len)) => {
                    if i > text_from {
                        out.push(Piece::Text(&s[text_from..i]));
                    }
                    out.push(Piece::Marker(marker));
                    i += len;
                    text_from = i;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    if text_from < s.len() {
        out.push(Piece::Text(&s[text_from..]));
    }
}

/// `text` as text and markers, in order. Code (fenced blocks and inline spans) is always
/// text.
pub fn parse(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    for (code, slice) in blocks(text) {
        if code {
            out.push(Piece::Text(slice));
        } else {
            parse_prose(slice, &mut out);
        }
    }
    out
}

/// Does `text` have a marker (outside code)?
pub fn has_markers(text: &str) -> bool {
    text.contains("[[") && parse(text).iter().any(|p| matches!(p, Piece::Marker(_)))
}

/// The refs `text`'s markers name, in order of first use, without repeats.
pub fn refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in parse(text) {
        if let Piece::Marker(m) = piece {
            for r in m.refs {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
    }
    out
}

fn h_space(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Characters a marker may sit right before with no space: closing punctuation and the
/// closing half of emphasis.
fn closes(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '\''
            | '"'
            | '’'
            | '”'
            | '…'
            | '*'
            | '_'
            | '~'
    )
}

/// `text` with each marker group (markers separated by nothing or by spaces) replaced by what
/// `resolve` gives for its refs, joined, or removed when it gives nothing. `resolve` is
/// called once per ref per group; a ref it answers `None` is dropped.
pub fn render(text: &str, mut resolve: impl FnMut(&str) -> Option<String>) -> Rendered {
    if !text.contains("[[") {
        return Rendered {
            text: text.to_string(),
            ..Rendered::default()
        };
    }
    let pieces = parse(text);
    let mut out = Rendered {
        text: String::with_capacity(text.len()),
        ..Rendered::default()
    };
    let mut i = 0;
    while i < pieces.len() {
        let Piece::Marker(_) = &pieces[i] else {
            if let Piece::Text(t) = pieces[i] {
                out.text.push_str(t);
            }
            i += 1;
            continue;
        };
        // The group: markers, with only spaces between them.
        let mut group: Vec<&Marker> = Vec::new();
        let mut j = i;
        while j < pieces.len() {
            match &pieces[j] {
                Piece::Marker(m) => {
                    group.push(m);
                    j += 1;
                }
                Piece::Text(t)
                    if t.chars().all(h_space)
                        && matches!(pieces.get(j + 1), Some(Piece::Marker(_))) =>
                {
                    j += 1
                }
                _ => break,
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        let mut replacement = String::new();
        for m in &group {
            out.dropped += m.malformed;
            for r in &m.refs {
                if seen.contains(&r.as_str()) {
                    continue;
                }
                seen.push(r);
                match resolve(r) {
                    Some(s) => {
                        out.kept += 1;
                        replacement.push_str(&s);
                    }
                    None => out.dropped += 1,
                }
            }
        }
        // Join it to the text around it.
        let line_start = {
            let t = out.text.trim_end_matches(h_space);
            t.is_empty() || t.ends_with('\n')
        };
        if !line_start {
            let keep = out.text.trim_end_matches(h_space).len();
            out.text.truncate(keep);
        }
        let mut next = match pieces.get(j) {
            Some(Piece::Text(t)) => *t,
            _ => "",
        };
        if replacement.is_empty() && line_start {
            next = next.trim_start_matches(h_space);
        }
        out.text.push_str(&replacement);
        let joins_word = next
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace() && !closes(c));
        let after_word = out.text.chars().last().is_some_and(|c| !c.is_whitespace());
        if joins_word && after_word {
            out.text.push(' ');
        }
        out.text.push_str(next);
        i = if matches!(pieces.get(j), Some(Piece::Text(_))) {
            j + 1
        } else {
            j
        };
    }
    out
}

/// `text` with every marker removed, cleanly. Text with none is returned as it is.
pub fn strip(text: &str) -> String {
    render(text, |_| None).text
}

/// `value` with [`strip`] applied to every string in it, at any depth.
pub fn strip_json(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(strip(s)),
        Value::Array(items) => Value::Array(items.iter().map(strip_json).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), strip_json(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(text: &str) -> String {
        // Resolve `src-a1` to `[[s1]]` and `src-b2` to `[[s2]]`; anything else is unknown.
        render(text, |r| match r {
            "src-a1" => Some("[[s1]]".into()),
            "src-b2" => Some("[[s2]]".into()),
            _ => None,
        })
        .text
    }

    #[test]
    fn text_without_markers_is_untouched() {
        for t in [
            "",
            "Plain text.",
            "A [link](https://x.example) and [[roadmap]] and [src-a1] and [[ src ]].",
            "  indented\n\ttabbed  \n",
        ] {
            assert_eq!(strip(t), t);
            assert_eq!(show(t), t);
            assert!(!has_markers(t));
        }
    }

    #[test]
    fn strips_cleanly() {
        assert_eq!(strip("Grace is 7 days [[src-a1]]."), "Grace is 7 days.");
        assert_eq!(strip("Grace is 7 days[[src-a1]]."), "Grace is 7 days.");
        assert_eq!(strip("one [[src-a1]] two"), "one two");
        assert_eq!(strip("one[[src-a1]]two"), "one two");
        assert_eq!(strip("end [[src-a1]]"), "end");
        assert_eq!(strip("end [[src-a1]]\nnext"), "end\nnext");
        assert_eq!(strip("[[src-a1]] Starts a line"), "Starts a line");
        assert_eq!(strip("x\n[[src-a1]] Starts a line"), "x\nStarts a line");
        assert_eq!(strip("(see [[src-a1]])"), "(see)");
        assert_eq!(
            strip("**Bold claim [[src-a1]]**, then"),
            "**Bold claim**, then"
        );
        assert_eq!(strip("| a [[src-a1]] | b |"), "| a | b |");
        assert_eq!(strip("Ünïcode [[src-a1]] — ok"), "Ünïcode — ok");
    }

    #[test]
    fn adjacent_markers_are_one_group() {
        assert_eq!(strip("Claim [[src-a1]][[src-b2]]."), "Claim.");
        assert_eq!(strip("Claim [[src-a1]] [[src-b2]] next"), "Claim next");
        assert_eq!(show("Claim [[src-a1]] [[src-b2]]."), "Claim[[s1]][[s2]].");
        assert_eq!(
            show("Claim [[src-a1, src-b2]] next"),
            "Claim[[s1]][[s2]] next"
        );
        assert_eq!(show("Claim [[src-a1]][[src-a1]]."), "Claim[[s1]].");
        assert_eq!(show("Claim [[src-b2]][[src-zz]]."), "Claim[[s2]].");
    }

    #[test]
    fn renders_markers_in_lists_and_headings() {
        let body = "## Risks [[src-a1]]\n\n- Churn rises [[src-b2]]\n  - nested [[src-a1]]\n1. Ordered [[src-zz]].\n";
        assert_eq!(
            show(body),
            "## Risks[[s1]]\n\n- Churn rises[[s2]]\n  - nested[[s1]]\n1. Ordered.\n"
        );
        assert_eq!(
            strip(body),
            "## Risks\n\n- Churn rises\n  - nested\n1. Ordered.\n"
        );
        assert_eq!(strip("- [[src-a1]] item"), "- item");
    }

    #[test]
    fn code_is_never_a_citation() {
        let span = "Write `[[src-a1]]` to cite [[src-b2]].";
        assert_eq!(show(span), "Write `[[src-a1]]` to cite[[s2]].");
        assert_eq!(strip(span), "Write `[[src-a1]]` to cite.");
        assert_eq!(refs(span), vec!["src-b2"]);
        let double = "A ``code with ` and [[src-a1]]`` span";
        assert_eq!(strip(double), double);
        let fenced = "Text [[src-a1]]\n```md\nclaim [[src-a1]]\n```\nafter [[src-b2]]\n";
        assert_eq!(
            show(fenced),
            "Text[[s1]]\n```md\nclaim [[src-a1]]\n```\nafter[[s2]]\n"
        );
        let tilde = "~~~\n[[src-a1]]\n~~~~\n[[src-a1]] out";
        assert_eq!(strip(tilde), "~~~\n[[src-a1]]\n~~~~\nout");
        // An unclosed fence runs to the end.
        let open = "x\n```\n[[src-a1]]";
        assert_eq!(strip(open), open);
        // An unclosed backtick is literal, so what follows is prose.
        assert_eq!(strip("a ` b [[src-a1]]."), "a ` b.");
        // A span does not cross a blank line.
        assert_eq!(strip("a `b\n\nc [[src-a1]] d`"), "a `b\n\nc d`");
        // An escaped backtick opens nothing.
        assert_eq!(strip("a \\` b [[src-a1]] `c`"), "a \\` b `c`");
    }

    #[test]
    fn malformed_and_display_markers_name_nothing() {
        let r = render("x [[src-a1, nope]] y", |r| Some(format!("<{r}>")));
        assert_eq!((r.text.as_str(), r.kept, r.dropped), ("x<src-a1> y", 1, 1));
        // Removed, naming nothing.
        for t in [
            "x [[src-]] y",
            "x [[src-a!b]] y",
            "x [[SRC-]] y",
            "x [[s3]] y",
        ] {
            assert_eq!(strip(t), "x y", "{t}");
            assert!(refs(t).is_empty(), "{t}");
        }
        assert_eq!(show("x [[s12]] y"), "x y");
        // Not markers at all: left as written.
        for t in [
            "x [[src-a1 y",
            "x [[src-a1]\n] y",
            "x [[s1234]] y",
            "x [[sa]] y",
            "x [src-a1] y",
        ] {
            assert_eq!(strip(t), t, "{t}");
        }
        // A stray `[` before a marker stays; the marker is the brackets that close.
        assert_eq!(strip("x [[[src-a1]]"), "x [");
        let long = format!("x [[src-a1 {}]] y", "a".repeat(MAX_MARKER));
        assert_eq!(strip(&long), long);
        // Case is folded.
        assert_eq!(refs("x [[SRC-A1]]"), vec!["src-a1"]);
        assert!(is_ref("src-3f9a1c") && !is_ref("src-") && !is_ref("s3"));
        assert!(!is_ref(&format!("src-{}", "a".repeat(MAX_REF + 1))));
    }

    #[test]
    fn unknown_ids_are_dropped_and_counted() {
        let r = render("A [[src-zz]]. B [[src-a1]][[s4]].", |r| {
            (r == "src-a1").then(|| "[[s1]]".to_string())
        });
        assert_eq!(r.text, "A. B[[s1]].");
        assert_eq!((r.kept, r.dropped), (1, 2));
    }

    #[test]
    fn strips_every_string_in_json() {
        let v = serde_json::json!({
            "title": "T [[src-a1]]", "body": "B [[src-a1]].", "n": 1,
            "list": ["c [[src-b2]]"]
        });
        assert_eq!(
            strip_json(&v),
            serde_json::json!({"title": "T", "body": "B.", "n": 1, "list": ["c"]})
        );
    }

    #[test]
    fn crlf_and_line_breaks_survive() {
        assert_eq!(strip("a [[src-a1]]\r\nb"), "a\r\nb");
        assert_eq!(strip("a  \n[[src-a1]]\n"), "a  \n\n");
    }
}
