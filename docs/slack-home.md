# The PM OS Home in Slack

**Open PM OS in Slack and pick the Home tab:** what's waiting for you, what PM OS can do, and
what it's connected to, in one place. Three buttons at the top switch between them: **Inbox**,
**Skills** and **Accounts**. The build spec is `docs/slack-mentions-and-home-spec.md` §4.

**Your level is shown at the top of the Skills section,** as the line "You're set to the
*Staff* level. Change it by sending me `level …`." (`docs/levels.md`). If PM OS can't read your
level, the line is left out.

You need to be on the product team (`team.yaml`). Anyone else sees "PM OS is for the product
team. DM me to ask for access."

## Inbox

**Every draft waiting for your decision:** Jira issues, Confluence pages, Slack replies and
messages, from any of your bots. That includes drafts from tagging PM OS in a thread, which
also start in your DM with "You tagged me in a thread in #channel. I'm drafting a reply here.
Nothing posts there until you say so." The top line says how many ("3 drafts waiting for
you"), or "Nothing is waiting for you." with only a **Refresh** button.

**Each row shows:**
- what it is and where it goes;
- which bot drafted it, and how long ago;
- the start of the text (200 characters).

The 10 oldest are shown first, because Slack drafts expire after 24 hours. With more, a line
says "and N more; they appear as you clear these."

**To decide:**
1. **Tick** the drafts you want to decide.
2. Tap **Approve selected** (it asks once: each draft is created or posted as you) or
   **Reject selected**.
3. A notice at the top counts how they went, one line per outcome, such as "2 approved or
   posted." and "1 needs your confirmation: it goes to a channel shared outside the company.
   Confirm on its card in our DM." Tap **Refresh** any time.

| The outcome line says | What it means | What to do |
| --- | --- | --- |
| **approved** or **posted** | Created in Jira or Confluence, or posted in Slack as you | Nothing |
| **rejected** | Discarded; nothing was sent | Nothing |
| **blocked** | Built from restricted sources, and people in the channel aren't on the product team | Reject it, or answer in the thread yourself |
| **needs your confirmation** | It goes to a channel shared outside the company | Confirm on its card in our DM |
| **expired**, or **already decided** | Past its 24 hours, or decided elsewhere (on its card, or by another tap) | Nothing |
| **failed** | PM OS couldn't reach the tool server, or the write failed (the line gives the error) | Try again, or ask a platform owner |
| "Tick the drafts first." | You tapped a button with nothing ticked | Tick at least one |

**Edit and Snooze stay on the draft's card in your DM** with PM OS. The Home only approves
and rejects; the footer says so: "To edit or snooze a draft, use its card in our DM."
Rejecting from the Home records no reason; to say why, reject on the card.

**What the Slack bot can see and change.** The bot is the approval screen, so the tool server
limits what its token can do:
- **A draft is read for the PM it belongs to.** Opening, editing or deciding a card reads the
  draft as you (`GET /internal/approvals/{id}?user=<you>`); a card that isn't yours answers
  "Only the PM this approval is for can decide it. Nothing was changed."
- **An edit changes what a draft says, never where it goes.** The editor offers the text,
  title, summary, description and body only, never the channel, thread, page, space,
  project, issue type or epic. If the tool server still finds a changed target (`422`),
  the card says so and keeps its buttons.
- **The triage digest** reads the feedback ledger without approval ids: each row links to
  its task by `run_id`.
- **New members** are added as PMs. Admins are added by a pull request to `team.yaml`.

## Skills

**What PM OS can do for you in Slack,** grouped by track:

| Track | For |
| --- | --- |
| Everyday | Questions, docs, meeting notes, Slack replies |
| Product sense | Critiques and problem framing |
| Strategy and decisions | Strategy and decision docs, prioritizing, pre-mortems |
| Metrics and experiments | Metric trees, experiment design and readouts |
| Spec and review | PRDs and the review panel |
| Customer and market | Call synthesis, competitive briefs, sizing, PoC plans, partner briefs |
| AI products | AI feature specs and eval plans |
| Delivery and launch | Release notes, status updates, launch checklists, Jira drafts, advisories |
| Leading across teams | Portfolio reviews, roadmap trade-offs, OKRs, business reviews, alignment plans, coaching, the bets register |
| Company bets | Vision docs, investment cases, pricing proposals, platform strategy, exec briefs |

Each line is a skill's name and what it's for. You see the skills your personal bot can run;
without a personal bot, the Docs bot's. If PM OS can't load the bot's details, it lists every
Slack skill, with a note saying so.

**To use one:** DM PM OS and ask in your own words, or name the skill first:
`strategy-doc: our plan for small teams`. What each skill needs from you is in
`docs/pm-craft.md`.

## Accounts

**What PM OS reads and writes as you.** Each row says **Connected** or **Not connected**, and
what it's used for.

| Account | PM OS uses it to |
| --- | --- |
| Jira and Confluence (one Atlassian link) | Read issues and pages as you, and create the drafts you approve |
| Slack | Read threads and search as you, and post the replies you approve |
| GitHub (when your team uses it) | Keep your bot's notes in your private memory repo |

**To connect:** tap **Connect** and sign in. The link lasts 10 minutes; tap **Refresh** for
new ones.

**To disconnect:** tap **Disconnect** and confirm. PM OS deletes its copy of that account's
tokens at once. Until you connect again:

| Disconnected | What stops working |
| --- | --- |
| Atlassian | Reading Jira and Confluence, and posting Jira and Confluence drafts |
| Slack | The reply assistant, tagging and thread reading |
| GitHub | Your bot's notes in your memory repo |

Drafts already waiting stay in your Inbox, but one that needs the account fails when you
approve it.

**Memory:** your memory repo's name, when you have set one (`docs/onboarding-spec.md`).
