//! Marketplace bots in Slack (`docs/bot-marketplace-spec.md` §9): Chief, Sprout and Echo are
//! personas the PM's own bot runs as, chosen per ask.
//!
//! - **Addressing:** a DM, or the text after an `@PM OS` tag, that begins with an offered
//!   persona's display name or key, any case, then `:` or `,` (`Chief: what slipped?`,
//!   `sprout, size the annual plan`) asks that persona, with the rest as the text
//!   ([`split`]). Only offered personas from the runtime's catalog (`GET /v1/personas`)
//!   match, so ordinary sentences (`Chiefly speaking`, `Note: …`) aren't captured, and
//!   `Nudge:` names Nudge, which switches a thread back to it.
//! - **Shown:** a persona's messages start with its name; a Preview persona's first message in
//!   a thread also says [`PREVIEW_LABEL`]; an approval card says "Drafted by Chief".
//! - **Untrusted text:** a persona's display name and title are content, so they are always
//!   rendered with [`crate::blocks::name`]: no mention, link or formatting can come from them.
//!   The key is checked as a key before it is used anywhere.

use std::sync::{Arc, RwLock};
use std::time::Instant;

use pmos_contract::bots::valid_name;
use pmos_contract::personas::{check_display_name, ALWAYS_ON, MAX_DISPLAY_NAME, MAX_TITLE};
use serde_json::Value;

/// Shown once per thread under a Preview persona's first message there.
pub const PREVIEW_LABEL: &str = "Preview: not yet benchmarked";
/// A persona key the catalog doesn't (or no longer) list: never guessed from the key.
pub const NO_LONGER_OFFERED: &str = "A bot no longer offered";
/// A persona the Slack bot can't name because neither the tool server's item nor any catalog
/// read says who it is. Shown with the Preview line: nothing says it was benchmarked
/// (gate-fix-4, M1: the line is never dropped for want of a catalog).
pub const UNNAMED_BOT: &str = "A marketplace bot";

/// A stand-in for a persona whose catalog entry couldn't be read: [`UNNAMED_BOT`], a Preview.
pub fn unnamed(key: &str) -> Persona {
    Persona {
        key: key.to_string(),
        display_name: UNNAMED_BOT.to_string(),
        title: String::new(),
        always_on: false,
        preview: true,
    }
}

/// One offered persona, from `GET /v1/personas`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    pub key: String,
    /// Untrusted: render with [`Persona::shown`].
    pub display_name: String,
    /// Untrusted: render with [`Persona::shown_title`].
    pub title: String,
    pub always_on: bool,
    pub preview: bool,
}

impl Persona {
    /// The display name, safe for `mrkdwn`.
    pub fn shown(&self) -> String {
        crate::blocks::name(&self.display_name, MAX_DISPLAY_NAME)
    }

    /// The title, safe for `mrkdwn`.
    pub fn shown_title(&self) -> String {
        crate::blocks::name(&self.title, MAX_TITLE)
    }

    pub fn is_nudge(&self) -> bool {
        self.key == ALWAYS_ON
    }
}

/// A key the runtime's `persona` field may hold: `^[a-z0-9]+(-[a-z0-9]+)*$`, at most 32.
pub fn key_ok(key: &str) -> bool {
    valid_name(key)
}

/// The personas of a `GET /v1/personas` answer (`{personas: [...]}` or a bare list). An entry
/// whose key isn't a key, or whose display name is blank, is left out: it can't be asked and
/// is shown as [`NO_LONGER_OFFERED`].
pub fn parse(json: &Value) -> Vec<Persona> {
    let list = match json {
        Value::Array(items) => items.as_slice(),
        _ => json["personas"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    };
    list.iter()
        .filter_map(|p| {
            let key = p["key"].as_str().filter(|k| key_ok(k))?;
            let display_name = p["display_name"]
                .as_str()
                .filter(|d| !d.trim().is_empty())?;
            Some(Persona {
                key: key.to_string(),
                display_name: display_name.to_string(),
                title: p["title"].as_str().unwrap_or_default().to_string(),
                always_on: p["always_on"] == true,
                preview: p["preview"] == true,
            })
        })
        .collect()
}

/// `Chief: what slipped?` → (Chief, `what slipped?`): the text begins with an offered
/// persona's display name or key, any case, immediately followed by `:` or `,`. The longest
/// name wins. A display name the lint wouldn't accept (anything but ASCII letters, digits and
/// single spaces) is never matched; the key still is.
pub fn split<'a, 'p>(text: &'a str, catalog: &'p [Persona]) -> Option<(&'p Persona, &'a str)> {
    let text = text.trim_start();
    let mut best: Option<(&Persona, usize)> = None;
    for p in catalog {
        let mut names = vec![p.key.as_str()];
        if check_display_name(&p.display_name).is_ok() {
            names.push(p.display_name.as_str());
        }
        for name in names {
            let n = name.len();
            let head_matches = text
                .get(..n)
                .is_some_and(|head| head.eq_ignore_ascii_case(name));
            let then = text.get(n..).and_then(|r| r.chars().next());
            if head_matches && matches!(then, Some(':' | ',')) && best.is_none_or(|(_, m)| n > m) {
                best = Some((p, n));
            }
        }
    }
    best.map(|(p, n)| (p, text[n + 1..].trim()))
}

/// One read of the catalog: when, and what it listed.
pub type Read = (Instant, Arc<Vec<Persona>>);

/// The catalog the Slack bot last read, shared with the tool server client so an approval
/// card can say which bot drafted it ([`Catalog::drafted_by`]).
#[derive(Clone, Default)]
pub struct Catalog {
    inner: Arc<RwLock<Option<Read>>>,
}

impl Catalog {
    /// The catalog as last read, and when; `None` before the first read.
    pub fn current(&self) -> Option<Read> {
        self.inner.read().expect("catalog lock").clone()
    }

    pub fn store(&self, personas: Vec<Persona>) -> Arc<Vec<Persona>> {
        let list = Arc::new(personas);
        *self.inner.write().expect("catalog lock") = Some((Instant::now(), list.clone()));
        list
    }

    /// The persona with this key, if the catalog lists it.
    pub fn get(&self, key: &str) -> Option<Persona> {
        let (_, list) = self.current()?;
        list.iter().find(|p| p.key == key).cloned()
    }

    /// Who drafted an approval item, for its card: `None` for Nudge (`null`, `"nudge"`) and
    /// before the catalog was ever read; the persona's display name (already safe `mrkdwn`)
    /// when listed; [`NO_LONGER_OFFERED`] for any other key.
    pub fn drafted_by(&self, persona: &Value) -> Option<String> {
        let key = persona.as_str().filter(|k| *k != ALWAYS_ON)?;
        let (_, list) = self.current()?;
        if !key_ok(key) {
            return Some(NO_LONGER_OFFERED.to_string());
        }
        Some(
            list.iter()
                .find(|p| p.key == key)
                .map(Persona::shown)
                .unwrap_or_else(|| NO_LONGER_OFFERED.to_string()),
        )
    }
}

/// The field the Slack bot adds to an approval item for its card: who drafted it, as safe
/// `mrkdwn`. Never taken as sent (any copy in the tool server's answer is dropped first).
pub const DRAFTED_BY_FIELD: &str = "_slack_drafted_by";
/// With [`DRAFTED_BY_FIELD`]: `true` when that bot is a Preview, so the card and the App Home
/// row say "Preview: not yet benchmarked" (gate-fix-4, M1).
pub const DRAFTED_PREVIEW_FIELD: &str = "_slack_drafted_preview";

/// Who drafted `item`, from the **tool server's item** (`persona_name`, `persona_preview`,
/// gate-fix-4): the name (safe `mrkdwn`) and whether it is a Preview. `None` for Nudge. A
/// `persona_name` of `null` is a bot no longer offered. An item from an older tool server,
/// without the fields, falls back to the catalog the Slack bot read; with none read, the bot
/// is [`UNNAMED_BOT`] and a Preview (fail safe: the label is never dropped for want of a
/// catalog).
pub fn drafter(item: &Value, catalog: &Catalog) -> Option<(String, bool)> {
    let key = item["persona"].as_str().filter(|k| *k != ALWAYS_ON)?;
    if let Some(name) = item.get("persona_name") {
        return Some(match name.as_str().filter(|n| !n.trim().is_empty()) {
            Some(n) => (
                // Room for the tool server's " (bot)" on a name like a colleague's.
                crate::blocks::name(n, MAX_DISPLAY_NAME + 6),
                item["persona_preview"] != false,
            ),
            None => (NO_LONGER_OFFERED.to_string(), false),
        });
    }
    if catalog.current().is_none() {
        return Some((UNNAMED_BOT.to_string(), true));
    }
    let found = key_ok(key).then(|| catalog.get(key)).flatten();
    Some(match found {
        Some(p) => (p.shown(), p.preview),
        None => (NO_LONGER_OFFERED.to_string(), false),
    })
}

/// Put [`DRAFTED_BY_FIELD`] and [`DRAFTED_PREVIEW_FIELD`] on `item` ([`drafter`]), or take
/// them off.
pub fn annotate(item: &mut Value, catalog: &Catalog) {
    let who = drafter(item, catalog);
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    obj.remove(DRAFTED_BY_FIELD);
    obj.remove(DRAFTED_PREVIEW_FIELD);
    if let Some((name, preview)) = who {
        obj.insert(DRAFTED_BY_FIELD.into(), Value::String(name));
        obj.insert(DRAFTED_PREVIEW_FIELD.into(), Value::Bool(preview));
    }
}

/// How a message says which bot is speaking: the name it starts with, and the Preview line
/// when it is the persona's first message in the thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speaker {
    /// Safe `mrkdwn`.
    pub name: String,
    /// Safe `mrkdwn`: "Chief of Staff · Preview: not yet benchmarked".
    pub preview: Option<String>,
}

impl Speaker {
    /// The speaker for a notification's `persona`: `None` for Nudge (absent, `null` or
    /// `"nudge"`). A key the catalog doesn't list is [`NO_LONGER_OFFERED`].
    pub fn of(persona: &Value, catalog: &[Persona]) -> Option<(Self, Option<Persona>)> {
        let key = persona.as_str().filter(|k| *k != ALWAYS_ON)?;
        let found = catalog
            .iter()
            .find(|p| p.key == key && key_ok(key))
            .cloned();
        let name = found
            .as_ref()
            .map(Persona::shown)
            .unwrap_or_else(|| NO_LONGER_OFFERED.to_string());
        Some((
            Self {
                name,
                preview: None,
            },
            found,
        ))
    }

    /// The Preview line for `p`: its title (if any) and [`PREVIEW_LABEL`].
    pub fn preview_line(p: &Persona) -> String {
        let title = p.shown_title();
        if title.trim().is_empty() {
            PREVIEW_LABEL.to_string()
        } else {
            format!("{title} · {PREVIEW_LABEL}")
        }
    }
}

/// `message` as said by `speaker`: the fallback text and the first section start with
/// "*Name:* " (a context block is put first when the message has no section first), and the
/// Preview line, if any, is a context block at the end.
pub fn attribute((text, blocks): (String, Value), speaker: &Speaker) -> (String, Value) {
    let mut blocks = blocks.as_array().cloned().unwrap_or_default();
    let lead = format!("*{}:*", speaker.name);
    match blocks.first_mut() {
        Some(first) if first["type"] == "section" && first["text"]["type"] == "mrkdwn" => {
            let body = first["text"]["text"].as_str().unwrap_or_default();
            first["text"]["text"] = Value::String(format!("{lead} {body}"));
        }
        _ => blocks.insert(0, context(&lead)),
    }
    if let Some(line) = &speaker.preview {
        blocks.push(context(line));
    }
    (format!("{}: {text}", speaker.name), Value::Array(blocks))
}

fn context(text: &str) -> Value {
    serde_json::json!({"type": "context", "elements": [{"type": "mrkdwn", "text": text}]})
}

/// The sentence for a `400` the runtime gave an ask that names `shown` (safe `mrkdwn`).
pub fn refusal(code: &str, shown: &str) -> String {
    match code {
        "unknown_persona" => format!(
            "{shown} isn't offered in PM OS right now, so I didn't send that. Ask Nudge instead \
             (just leave out “{shown}:”)."
        ),
        "persona_needs_personal_bot" => NEEDS_PERSONAL_BOT.replace("{name}", shown),
        "persona_not_on_team" => format!(
            "{shown} isn't on your team, so I didn't send that. Add {shown} on the Bots page in \
             PM OS (if you just did, try again in a couple of minutes), or ask Nudge (just leave \
             out “{shown}:”)."
        ),
        _ => format!("{shown} can't take that right now. Try again, or ask Nudge."),
    }
}

/// A persona ask from someone without a personal bot: `{name}` is the persona.
pub const NEEDS_PERSONAL_BOT: &str =
    "{name} works through your own PM OS bot, which isn't set up yet. DM me `setup` to finish \
     setting up.";

/// A persona prefix with nothing after it.
pub fn empty_ask(shown: &str) -> String {
    format!("Ask {shown} something after “{shown}:”.")
}

/// A persona prefix on a routine request: routines from Slack are Nudge's for now.
pub fn routine_not_yet(shown: &str) -> String {
    format!(
        "Routines set up in Slack run as Nudge for now, so I didn't set this one up for \
         {shown}. Send it again without “{shown}:” to make it Nudge's."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Vec<Persona> {
        parse(&json!({"personas": [
            {"key": "nudge", "display_name": "Nudge", "title": "Your PM assistant", "always_on": true, "preview": false},
            {"key": "chief", "display_name": "Chief", "title": "Chief of Staff", "always_on": false, "preview": true},
            {"key": "sprout", "display_name": "Sprout", "title": "Growth PM", "preview": true},
            {"key": "Bad Key", "display_name": "Bad"},
            {"key": "blank", "display_name": "  "},
        ]}))
    }

    #[test]
    fn the_catalog_keeps_only_well_formed_entries() {
        let c = catalog();
        assert_eq!(
            c.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            ["nudge", "chief", "sprout"]
        );
        assert!(c[1].preview && !c[0].preview && c[0].always_on);
        assert_eq!(parse(&json!(null)), Vec::new());
        assert_eq!(
            parse(&json!([{"key": "echo", "display_name": "Echo"}])).len(),
            1
        );
    }

    #[test]
    fn a_prefix_names_an_offered_persona() {
        let c = catalog();
        let key = |t: &str| split(t, &c).map(|(p, rest)| (p.key.clone(), rest.to_string()));
        assert_eq!(
            key("Chief: what slipped since Monday?"),
            Some(("chief".into(), "what slipped since Monday?".into()))
        );
        assert_eq!(
            key("chief, prep me"),
            Some(("chief".into(), "prep me".into()))
        );
        assert_eq!(key("CHIEF:x"), Some(("chief".into(), "x".into())));
        assert_eq!(
            key("  sprout,   size it "),
            Some(("sprout".into(), "size it".into()))
        );
        assert_eq!(key("Nudge: hi"), Some(("nudge".into(), "hi".into())));
        assert_eq!(key("chief:"), Some(("chief".into(), String::new())));
        // Ordinary sentences aren't captured.
        assert_eq!(key("Chiefly speaking, this slipped"), None);
        assert_eq!(key("chief what slipped"), None);
        assert_eq!(key("Chief : spaced"), None);
        assert_eq!(key("Note: a thing"), None);
        assert_eq!(key("echo: not offered here"), None, "not in this catalog");
        assert_eq!(key(""), None);
        // Non-ASCII text never panics on a byte boundary.
        assert_eq!(key("Ché: x"), None);
        assert_eq!(key("é"), None);
    }

    #[test]
    fn a_display_name_with_other_characters_is_never_a_prefix() {
        let c = parse(&json!([{"key": "boss", "display_name": "<!channel> Boss"}]));
        assert!(split("<!channel> Boss: hi", &c).is_none());
        assert_eq!(
            split("boss: hi", &c).map(|(p, _)| p.key.as_str()),
            Some("boss")
        );
    }

    #[test]
    fn the_longest_name_wins() {
        let c = parse(&json!([
            {"key": "chief", "display_name": "Chief"},
            {"key": "chief-ops", "display_name": "Chief Ops"},
        ]));
        assert_eq!(
            split("Chief Ops: go", &c).map(|(p, _)| p.key.as_str()),
            Some("chief-ops")
        );
        assert_eq!(
            split("chief-ops, go", &c).map(|(p, _)| p.key.as_str()),
            Some("chief-ops")
        );
        assert_eq!(
            split("chief: go", &c).map(|(p, _)| p.key.as_str()),
            Some("chief")
        );
    }

    #[test]
    fn hostile_catalog_text_is_inert() {
        let hostile = parse(&json!([{
            "key": "chief",
            "display_name": "<!channel> *Boss* <@U0BOB> <https://evil.example|click> `x` ~y~ _z_\nnext",
            "title": "<!here> admin.example.com/login @everyone",
            "preview": true,
        }]));
        let (speaker, found) = Speaker::of(&json!("chief"), &hostile).unwrap();
        let line = Speaker::preview_line(&found.unwrap());
        for shown in [&speaker.name, &line] {
            for bad in [
                "<!",
                "<@",
                "<http",
                "*",
                "`",
                "~",
                "_",
                "\n",
                "example.com",
                "@",
            ] {
                assert!(!shown.contains(bad), "{bad:?} in {shown:?}");
            }
        }
        let (text, blocks) = attribute(
            crate::cards::note("Done."),
            &Speaker {
                preview: Some(line),
                ..speaker
            },
        );
        let all = format!("{text} {blocks}");
        for bad in ["<!channel>", "<!here>", "<@U0BOB>", "<https://evil"] {
            assert!(!all.contains(bad), "{bad} in {all}");
        }
    }

    #[test]
    fn nudge_and_unknown_keys_are_shown_as_they_are() {
        let c = catalog();
        assert!(Speaker::of(&json!(null), &c).is_none());
        assert!(Speaker::of(&json!("nudge"), &c).is_none());
        assert!(Speaker::of(&Value::Null, &c).is_none());
        let (s, found) = Speaker::of(&json!("ghost"), &c).unwrap();
        assert_eq!(s.name, NO_LONGER_OFFERED);
        assert!(found.is_none());
        let (s, _) = Speaker::of(&json!("../x"), &c).unwrap();
        assert_eq!(s.name, NO_LONGER_OFFERED);
        let (s, found) = Speaker::of(&json!("chief"), &c).unwrap();
        assert_eq!(s.name, "Chief");
        assert_eq!(
            Speaker::preview_line(&found.unwrap()),
            "Chief of Staff · Preview: not yet benchmarked"
        );
    }

    #[test]
    fn attribution_starts_the_message_with_the_name() {
        let speaker = Speaker {
            name: "Chief".into(),
            preview: None,
        };
        let (text, blocks) = attribute(crate::cards::note("All done."), &speaker);
        assert_eq!(text, "Chief: All done.");
        assert_eq!(blocks[0]["text"]["text"], "*Chief:* All done.");
        assert_eq!(blocks.as_array().unwrap().len(), 1);
        let (_, blocks) = attribute(
            ("x".into(), json!([{"type": "actions", "elements": []}])),
            &Speaker {
                name: "Chief".into(),
                preview: Some("Preview: not yet benchmarked".into()),
            },
        );
        assert_eq!(blocks[0]["type"], "context");
        assert_eq!(blocks[0]["elements"][0]["text"], "*Chief:*");
        assert_eq!(
            blocks[2]["elements"][0]["text"],
            "Preview: not yet benchmarked"
        );
    }

    #[test]
    fn a_bot_refused_moments_ago_is_explained_without_a_task() {
        let s = refusal("persona_not_on_team", "Chief");
        assert!(s.starts_with("Chief isn't on your team"), "{s}");
        assert!(s.contains("Bots page"), "{s}");
        assert!(crate::runtime::PERSONA_CODES.contains(&"persona_not_on_team"));
    }

    #[test]
    fn the_drafter_comes_from_the_tool_servers_item_and_fails_safe() {
        let cat = Catalog::default();
        let item = |v: Value| drafter(&v, &cat);
        assert_eq!(item(json!({"persona": null})), None);
        assert_eq!(item(json!({"persona": "nudge"})), None);
        assert_eq!(
            item(json!({"persona": "chief", "persona_name": "Boss", "persona_preview": true})),
            Some(("Boss".into(), true))
        );
        assert_eq!(
            item(json!({"persona": "chief", "persona_name": "Boss", "persona_preview": false})),
            Some(("Boss".into(), false))
        );
        // Not a bool: a Preview (fail safe).
        assert_eq!(
            item(json!({"persona": "chief", "persona_name": "Boss"})),
            Some(("Boss".into(), true))
        );
        assert_eq!(
            item(json!({"persona": "chief", "persona_name": null})),
            Some((NO_LONGER_OFFERED.into(), false))
        );
        // Hostile text from the item is inert.
        let (n, _) = item(json!({"persona": "chief", "persona_name": "<!channel> *x*"})).unwrap();
        assert!(!n.contains("<!") && !n.contains('*'), "{n}");
        // An older tool server and no catalog read: unnamed, a Preview.
        assert_eq!(
            item(json!({"persona": "chief"})),
            Some((UNNAMED_BOT.into(), true))
        );
        // ... with a catalog read: the catalog's.
        cat.store(catalog());
        assert_eq!(
            drafter(&json!({"persona": "chief"}), &cat),
            Some(("Chief".into(), true))
        );
        assert_eq!(
            drafter(&json!({"persona": "ghost"}), &cat),
            Some((NO_LONGER_OFFERED.into(), false))
        );
    }

    #[test]
    fn drafted_by_needs_a_read_catalog() {
        let cat = Catalog::default();
        assert_eq!(
            cat.drafted_by(&json!("chief")),
            None,
            "never read: nothing guessed"
        );
        cat.store(catalog());
        assert_eq!(cat.drafted_by(&json!("chief")).as_deref(), Some("Chief"));
        assert_eq!(cat.drafted_by(&json!("nudge")), None);
        assert_eq!(cat.drafted_by(&json!(null)), None);
        assert_eq!(
            cat.drafted_by(&json!("ghost")).as_deref(),
            Some(NO_LONGER_OFFERED)
        );
        let mut item = json!({"id": "a", "persona": "chief", DRAFTED_BY_FIELD: "<!channel>"});
        annotate(&mut item, &cat);
        assert_eq!(item[DRAFTED_BY_FIELD], "Chief");
        let mut item = json!({"id": "a", "persona": null, DRAFTED_BY_FIELD: "<!channel>"});
        annotate(&mut item, &cat);
        assert!(
            item.get(DRAFTED_BY_FIELD).is_none(),
            "a sent copy is dropped"
        );
    }
}
