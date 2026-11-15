---
name: launch-checklist
description: Write a go/no-go launch checklist with the launch decision first as a recommendation, a readiness line for each area (product, docs, support, sales, legal and compliance, security, monitoring, rollback) with an owner, a date, a state and evidence, measurable go/no-go criteria, a rollback with a trigger and an owner, and the communications, as a Confluence draft the PM approves. Use when the PM asks for a launch checklist, a readiness review or "are we ready to ship <feature>". Not for a progress update to stakeholders (use status-update), for customer release notes (use release-notes) or for a POC with one customer (use poc-plan).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
---

# /launch-checklist

Answer one question for the decider: go, go with conditions, or no-go, and why. Every area
has an owner, a date and evidence.

## Inputs
- The launch (required): a feature, an epic or a PRD, and the target date.
- Optional: the decider, the launch criteria in the PRD, a target Confluence space.

## Steps
1. Read `/content/playbooks/strategy.md` (reversible and one-way decisions),
   `/content/playbooks/metrics-and-experiments.md` (guardrails) and
   `/content/templates/launch-checklist.md`.
2. Gather, and keep the list of sources:
   - the PRD's launch criteria and rollout plan (`confluence.search`, `confluence.read`);
   - the state of each epic and its open bugs, by owner (`jira.search`, `jira.read`);
   - docs coverage (`docs.search`) and past launches (`context.search` in `decisions`).
3. Ask up to three questions whose answers would change the call: who decides and by when,
   what must be true to launch, and what is fixed (a date, a customer). Wait, unless the PM
   said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the launch decision first, labelled as a recommendation, with the two or three facts
     behind it;
   - a readiness row for product, docs, support, sales, legal and compliance, security,
     monitoring and rollback, each with an owner, a date, a state (ready, at risk or not
     ready) and the evidence; a row with no evidence is "not ready";
   - go/no-go criteria with a threshold, how it is measured and a guardrail;
   - a rollback with its trigger, the owner who calls it and how long it takes; then
     communications and the remaining work in date order.
5. Check the draft against `/content/playbooks/bar.md`, in the form that fits a decision.
   Fix what fails, once. The riskiest assumption and its cheapest test, and every
   `[unverified]` item, go under "Open items" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Launch checklist: <product> ·
   <date>`, in the space the PM named or their default. Then reply with the recommendation in
   one line, what is waiting for approval, and the area most at risk.

## Do not
- Mark an area ready on someone's word. Cite the evidence or mark it "not ready".
- Leave a row without an owner and a date.
- Write a rollback without a trigger and an owner.
- Put the decision after the background. It comes first.
- Invent owners, dates or test results. A gap is an open item.
