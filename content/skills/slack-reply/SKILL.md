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
   (`context.search`, `context.read`), Confluence and Jira, and the public support docs for
   how-to and configuration questions about Acme products. Read the most relevant two to
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

## Do not
- Paste long passages from Restricted sources into a reply. PM OS blocks a draft that uses
  Restricted sources from posting where people outside the product team can read it; write
  the reply so it doesn't need them.
- Reply to messages from outside Slack Connect channels with anything internal unless the PM
  asked for it.
- Copy the question's text into your report; refer to the thread by channel and asker.
