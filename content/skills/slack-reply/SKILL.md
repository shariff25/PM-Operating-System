---
name: slack-reply
description: Find Slack messages since the last run that ask the PM something or need their answer, and draft a grounded reply to each for the PM to approve. Use from the PM's reply-assistant routine, or when the PM asks to catch up on Slack questions.
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "default"
  data-class: "restricted"
  tools: "slack.search slack.read_thread slack.post_reply docs.search docs.read jira.search jira.read confluence.search confluence.read context.search context.read"
  surfaces: "slack"
  ships-in: "pilot"
  track: "Everyday"
---

# Slack reply assistant

Draft replies to the questions people asked your PM in Slack. You draft; the PM decides. A
reply posts only when the PM approves it, as the PM, marked "Drafted with PM OS".

## Steps
1. **Find candidates.** Search Slack (`slack.search`) for messages since the last run that
   mention the PM or are in their direct messages. If you weren't told when the last run was,
   search the past day.
2. **Read each thread** (`slack.read_thread`) and skip it when:
   - the PM already replied after the question;
   - it isn't a question or request for the PM (someone else is asked, or it's a statement);
   - it's an FYI, an announcement, a bot message or a reaction-only thread.
3. **Find the answer** in the team's sources, not from memory: the context library
   (`context.search`, `context.read`), Confluence and Jira, and the company's public docs for
   how-to and configuration questions about the products. Read the most relevant two to
   five results in full.
4. **Draft one reply per thread** with `slack.post_reply`, in the thread:
   - Short: two to four sentences, in the PM's voice, first person.
   - Lead with the direct answer, then cite every factual claim as a link.
   - If the sources don't answer it, say so plainly and suggest who might know or where to
     look. Never guess a date, a number, a customer name or a commitment.
   - Don't promise anything on the PM's behalf (dates, scope, pricing, escalations). Offer to
     follow up instead.
   - If the tool says the thread was already drafted, move on; never draft twice.
5. **Report back** in one or two lines: how many drafts are waiting for approval, and which
   threads you skipped and why ("already answered", "not a question for you").

## When the request starts with `[tagged in Slack]`
The PM tagged you in a channel thread. The first line names it:
`[tagged in Slack] channel=<C…> thread_ts=<ts> message_ts=<ts>`; the rest is their ask.
1. **Skip the search and the filtering.** The PM asked, and the thread is the one named.
2. **Read it:** `slack.read_thread {channel, ts: thread_ts}`.
3. **Decide what was asked.** If the PM wants something for themselves (a summary, a status,
   where a document is), answer them here and draft nothing. Otherwise the ask is the
   question to answer, or instructions for the reply: draft it.
4. **Ground and cite** as in steps 3 and 4. If the sources don't answer it, the draft says so.
5. **Draft once:** `slack.post_reply {channel, thread_ts, message_ts, text}`, with the ids
   exactly as given.
6. If the tool answers `already_drafted`, say a draft for that message is already waiting.

## Do not
- Paste long passages from Restricted sources into a reply. PM OS blocks a draft that uses
  Restricted sources from posting where people outside the product team can read it; write
  the reply so it doesn't need them.
- Reply to messages from outside Slack Connect channels with anything internal unless the PM
  asked for it.
- Copy the question's text into your report; refer to the thread by channel and asker.
