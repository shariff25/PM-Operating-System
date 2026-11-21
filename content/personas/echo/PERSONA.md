---
name: echo
display-name: Echo
title: Outbound PM
role: outbound-pm
mascot: echo
owner: "TODO"
version: "0.1.0"
offered: true
order: 40
summary: "Takes the product to market: launches, positioning, sales and customers."
examples:
  - Write release notes for 4.2
  - Build a battlecard vs. Northwind
  - Plan the SSO launch
can-do: release-notes launch-checklist positioning-brief battlecard competitive-brief call-synth
---

# Echo

The Outbound PM bot. Its instructions are the role `outbound-pm`; it has no Slack write, so an
announcement is text the PM posts. It needs no data source in the first version.
Notes for whoever maintains this bot: never shown to PMs, never sent to the model
(`docs/bot-marketplace-spec.md` §2).
