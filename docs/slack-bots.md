# Asking Chief, Sprout or Echo in Slack

The marketplace bots (`docs/bot-marketplace-spec.md` §9) in the PM OS Slack app, as built in
slice S1 (`crates/slack/src/personas.rs`). Chief, Sprout and Echo are personas your own PM OS
bot runs as; Nudge is the always-on generalist. Pick your team in the web app (the Bots page);
Slack doesn't change it.

## Asking a bot

Start a DM to PM OS, or the text after an `@PM OS` tag in a channel, with a bot's name or key,
any case, then `:` or `,`:

- `Chief: what slipped since Monday?`
- `sprout, size the annual plan`
- `@PM OS Echo: draft a reply for the partner thread`

Anything else asks Nudge, as before. Only bots the Admin offers match (the runtime's catalog,
`GET /v1/personas`, read at most once a minute), so `Chiefly speaking, …` or `Note: …` are
ordinary text, and a bot that is no longer offered is ordinary text too. `Nudge:` names Nudge.
A team bot's own prefix (`docs: …`) still comes first.

The ask always goes to your own bot (`pm-<handle>`), with `persona` set to that bot's key. The
prefix is read only from your own DM or your own tag, never from a thread's text.

## Threads

A reply in a thread names no bot, so the runtime keeps the thread's: a reply in Chief's thread
stays with Chief. A reply that starts with another bot's name (`Sprout: …`) asks that bot, with
no history; `Nudge: …` switches the thread back to Nudge.

## What you see

- **Who is speaking.** Every progress, answer and failure message from a marketplace bot
  starts with its name ("*Chief:* …"), and so does the Slack notification text. Messages still
  post under your bot's name; no new Slack scope is used. Nudge's messages are unchanged. A
  bot the catalog no longer lists reads "A bot no longer offered": the name is never guessed
  from its key. There is no mascot in Slack.
- **Preview.** Chief, Sprout and Echo are Previews until their eval run passes. The first
  message a Preview bot posts in a thread ends with one line, "Chief of Staff · Preview: not
  yet benchmarked"; later messages in that thread don't repeat it (the progress message keeps
  it as it is edited).
- **Approval cards** say "Drafted by Chief" in their context line (Nudge's cards are as
  before), and every card of a Preview bot adds "· Preview: not yet benchmarked"; so does each
  App Home Inbox row, which names the bot ("Drafted by Chief") instead of the personal bot. The
  name and the Preview flag come from the tool server's item (`persona_name`,
  `persona_preview`; a bot named like a colleague reads "Ana (bot)"), never from the Slack
  bot's catalog copy. An item from an older tool server falls back to that copy; with no copy
  at all the card says "Drafted by A marketplace bot · Preview: not yet benchmarked" (fail safe:
  the line is never dropped for want of a catalog), and a thread's messages are said by "A
  marketplace bot" with the Preview line (gate-fix-4). A draft whose bot was withdrawn from the catalog is **held**: its card says so and
  offers only **Reject**, until the bot is offered again or an Admin releases it.

## When a bot can't take the ask

| Case | What you get | Task sent? |
| --- | --- | --- |
| The bot isn't on your team | The tool server's sentence in the thread, said by that bot: "Chief isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge." | Yes: the team is checked at claim, by the tool server (Slack can't read your team) |
| No personal bot yet | "Chief works through your own PM OS bot, which isn't set up yet. DM me `setup` to finish setting up." | No |
| Only the name (`Chief:`) | "Ask Chief something after “Chief:”." (in a tag: "Draft a reply to this thread.", as for a bare tag) | No |
| A routine (`Chief: every weekday at 9 …`) | "Routines set up in Slack run as Nudge for now …" | No |
| The runtime refuses the bot (`400 unknown_persona`, withdrawn since the catalog was read) | "Chief isn't offered in PM OS right now, so I didn't send that. Ask Nudge instead …" | Refused |

## Untrusted text

A bot's display name and title are content. Slack renders them with the display-name
escaping (`blocks::name`): `<`, `>` and `&` are escaped and Slack's formatting and link
characters become look-alikes, so no mention, link, bold or code can come from them. Only a
display name the lint accepts (ASCII letters, digits and single spaces) can be a prefix; the
key always can. A key that isn't key-shaped is never used.

## Operations

- No new configuration, scope or route on the Slack side. The Slack app calls the runtime's
  `GET /v1/personas` (allowed for the `slack` client); a runtime from before personas (`404`)
  gives an empty catalog, so everything works as before.
- Deploy the runtime with personas (R1) before relying on prefixes; Slack's side is safe in
  either order.
- The Preview line's "once per thread" is kept in memory: after a restart a thread may show it
  once more.
