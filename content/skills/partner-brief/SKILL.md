---
name: partner-brief
description: Write a partner brief that says whether to partner, what each side concretely gets, who the partner is, the evidence of mutual customers, the smallest joint thing to test, the risks and the open questions, as a Confluence draft the PM approves. Use when the PM asks "should we partner with <company>", for a partnership one-pager, or to prepare for a partner meeting. Not for how we compare to a competitor (use competitive-brief), for choosing between our own options (use decision-doc) or for planning a trial with one customer (use poc-plan).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
---

# /partner-brief

Decide whether a partnership is worth testing, and what the smallest test is. Concrete gains
for both sides, not "synergy".

## Inputs
- The partner and the product or segment (required).
- Optional: what the partner proposed, the meeting date, a target Confluence space.

## Steps
1. Read `/content/playbooks/strategy.md` (alternatives and reversible decisions),
   `/content/playbooks/customer-insight.md` and `/content/templates/partner-brief.md`.
2. Gather, and keep the list of sources:
   - the partner (`context.search` in `partners`, `competitors`, `confluence.search`) and
     their public pages with the date read;
   - customers we share, counted by account (`jira.search`, `confluence.search`), and our
     docs on any integration (`docs.search`).
   Note which material came from a partner NDA; it is restricted.
3. Ask up to three questions whose answers would change the brief: what problem our
   customers have that the partner solves, what we are prepared to give, and by when we must
   answer. If the request is "partner on <solution>" with no customer problem, that is the
   first question. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation first: partner, test first, or decline, with the outcome (a metric,
     a number, a date) and a guardrail;
   - who they are, from dated sources; what each side gets and gives up, concretely;
   - evidence of mutual customers, then the smallest joint thing to test, its length, owners
     and the result that would make us go further or stop;
   - one alternative, including doing nothing, and why it lost.
5. Check the draft against `/content/playbooks/bar.md`, in the form that fits a decision.
   Fix what fails, once. The riskiest assumption and its cheapest test go under "Risks";
   what the sources can't settle, and every `[unverified]` item, under "Open questions".
6. Propose the draft with `confluence.create_draft`, titled `Partner brief: <partner> ·
   <product>`, in the space the PM named or their default. Then reply with the recommendation
   in one line, what is waiting for approval, and the top open question.

## Do not
- Write "synergy" without saying what each side gets.
- Claim mutual customers without counting them and citing a source.
- Put partner NDA material into anything external. Mark it, and keep this draft internal.
- Propose a big integration before the smallest test.
- Invent partner facts, revenue figures or customer overlap. A gap is an open question.
