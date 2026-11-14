---
title: Reference architecture for the product-team agent platform
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# 2026-10: Reference architecture for the product-team agent platform

**Decision.** The product team runs its agents on the team's DGX Spark (GB10) with Slack, web
and CLI surfaces, an agent platform, an MCP tool server, a model gateway and vLLM on the
Spark. PM OS is the first product built on it.

**Out of scope.** Confidential computing for this platform: GB10 does not support NVIDIA CC
mode.

**Follow-on decisions (2026-11, PM OS build plan).**
- Open-weight models only; no frontier API for any data class.
- Team-service harness: LangChain Deep Agents (TypeScript). Personal agents: Hermes on
  NemoClaw.
- Backend services in Rust; web workspace in TypeScript.
- Context layer: markdown in git, indexed in Postgres, served only through the tool server.
- Systems of record: Jira and Confluence. Every connector, and every Slack reply, acts under
  the PM's own token.

**Sources.** PM OS PRD; `docs/implementation-plan.md` in this repo.
