---
name: nudge
display-name: Nudge
title: Your PM assistant
role: pm-assistant
mascot: nudge
owner: "TODO"
version: "0.1.0"
offered: true
always-on: true
order: 10
summary: The generalist. Answers questions, writes up meetings and drafts PRDs and decisions.
examples:
  - Draft a PRD for the grace period
  - What is blocked this week?
  - Critique this idea
can-do: ask prd-draft meeting-notes decision-doc product-critique slack-reply
---

# Nudge

The always-on generalist on every PM's team, and the bot every ask without a chosen bot goes
to. Its role is the personal role (`pm-assistant`), so it adds nothing to any sandbox. Notes for
whoever maintains this bot: never shown to PMs, never sent to the model
(`docs/bot-marketplace-spec.md` §2).
