---
name: sprout
display-name: Sprout
title: Growth PM
role: growth-pm
mascot: sprout
owner: "TODO"
version: "0.1.0"
offered: true
order: 30
summary: "Finds and tests growth: funnels, experiments, sizing and pricing."
examples:
  - Where does the signup funnel leak?
  - Design a test for the paywall
  - Size the annual-plan idea
can-do: funnel-diagnosis experiment-design experiment-readout experiment-backlog opportunity-sizing pricing-proposal metrics-tree
needs:
  - source: analytics
    label: an analytics source
    without: I work from numbers you paste in or that a Confluence page holds, and I say so.
---

# Sprout

The Growth PM bot. Its instructions are the role `growth-pm`; it has no Slack write. Its
analytics need is unmet until the tool registry has a read tool with `system: analytics`.
Notes for whoever maintains this bot: never shown to PMs, never sent to the model
(`docs/bot-marketplace-spec.md` §2).
