---
name: prioritize
description: Rank a list of features, requests, Jira issues or ideas against a stated goal and segment, in the thread, as a table with who each item is for, reach, impact, confidence, effort and the reason for its place, plus what falls below the line and what to cut. Use when the PM asks "what should we do first", "rank these", "prioritise the backlog" or "what do we cut". No draft is created. Not for choosing between two or three options in depth (use decision-doc) or setting direction (use strategy-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
  track: "Strategy and decisions"
---

# /prioritize

Rank a list against one goal, and say what falls below the line. The answer is a table in
the thread. No draft is created.

## Inputs
- The items (required): a list in the thread, a Jira epic or filter, or a Confluence page.
- The goal: the metric or outcome, the segment and the period. Ask if missing.

## Steps
1. Read `/content/playbooks/strategy.md` (section 6, sequencing and prioritising) and
   `/content/playbooks/product-sense.md` (step 3, ranking pains).
2. Gather evidence for each item, and keep the list of sources:
   - the issue, its links and any estimate (`jira.read`, `jira.search`);
   - who asked and how often, by account (`confluence.search`, `jira.search`);
   - the current strategy or goals (`context.search` in `decisions`, `confluence.search`).
3. Ask up to three questions whose answers would change the order. If no goal and segment
   were given, that is the first question: what are we prioritising for, and for whom? Wait,
   unless the PM said "rank anyway"; then state the goal you assumed.
4. Answer in this shape:
   - **One line first:** the top three and the reason, labelled as your recommendation.
   - **The table**, in rank order:

     | Rank | Item | Who it is for | Reach | Impact on the goal | Confidence | Effort | Why here |
     | --- | --- | --- | --- | --- | --- | --- | --- |

     Reach is a count with its source. Impact, confidence and effort are high, medium or
     low, each with the evidence in a few words. Confidence is low when the only evidence is
     one account or an opinion.
   - **Below the line:** the items you would not do this period, and why.
   - **Where I overrode the scores:** any item ranked against its scores, for example to
     learn something first or because it serves the wrong segment.
   - **What would change the order:** the one or two unknowns that matter.
5. Cite every factual claim; mark the rest `[unverified]`.

## Do not
- Invent effort estimates. Use the estimate in Jira, or write "not estimated" and name who
  should size it.
- Multiply made-up numbers into a precise score. Rank with reasons.
- Rank by who asked loudest or by contract size alone.
- Put everything above the line. If nothing is cut, it is a list, not a priority order.
- Create a draft, an issue or a message, or change any Jira issue.
