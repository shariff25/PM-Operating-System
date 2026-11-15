---
name: status-update
description: Write a one-page status update for stakeholders from Jira and Confluence since the last update, with the bottom line first (on track, at risk or off track, and why), what shipped, what is in progress, risks with an owner and a date, the decisions needed from the reader and what is next, as a Confluence draft the PM approves. Use when the PM asks for a status update, a weekly or monthly update, or a summary of progress for a sponsor or leadership. Not for customer-facing release notes (use release-notes), for a go/no-go before a launch (use launch-checklist) or for notes from one meeting (use meeting-notes).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "default"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /status-update

Give the reader the answer first: are we on track, and what do you need from them. One page.

## Inputs
- The project or area (required): an epic, a Jira project or label, or a Confluence page.
- Optional: the date of the last update, the audience, a target Confluence space.

## Steps
1. Read `/content/templates/status-update.md` and `/content/playbooks/bar.md`. Find the last
   status update (`confluence.search`) so you report only what changed since.
2. Gather, and keep the list of sources:
   - issues done, started or blocked since that date, and the epic's progress (`jira.search`,
     `jira.read`);
   - the plan, dates and open risks on the project's pages (`confluence.search`,
     `confluence.read`, `context.search` in `decisions`);
   - the owner and due date of each blocker.
3. Ask up to three questions whose answers would change the update: who reads it and what
   they decide, the date of the last update, and the target date or metric the status is
   measured against. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - bottom line first: on track, at risk or off track, the reason in one sentence, and the
     one thing the reader must do, labelled as the PM's read;
   - Shipped as outcomes with links, not a ticket list;
   - In progress: only what matters, each with an owner, a date and a state;
   - every risk with an owner, a date and the cost of leaving it;
   - Decisions needed: only decisions that need this reader, each with who, by when and a
     recommendation. If there are none, write "None".
5. Check the draft against `/content/playbooks/bar.md`, in the form that fits an update: the
   status is supported by dates and evidence, the riskiest assumption is named with its
   cheapest test, and nothing unsourced is unflagged. Fix what fails, once. What the sources
   can't settle is marked `[unverified]` under "Risks and blockers" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Status: <project> · <period>`,
   in the space the PM named or their default. Then reply with the bottom line in one line,
   what is waiting for approval, and the top open risk.

## Do not
- List activity. A reader wants outcomes, risks and asks.
- Say "on track" without a date or a measure behind it.
- Put an FYI under "Decisions needed". If the reader need not act, it isn't one.
- Leave a risk without an owner and a date.
- Invent progress, dates or owners. A gap is reported as a gap.
