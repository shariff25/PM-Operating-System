# PM OS web client

**Status:** decided 2026-11-16. Phase Web 1 (foundations) has started, with plumbing only. Nothing here is live.

## What it is

A full web app for PM OS, alongside the Slack bot. It covers sign-in, roles, approvals, routines, the audit view, the connector list, and later the admin areas. Slack stays the place PMs ask bots for things, and this decision does not remove it.

The small `pmos-console` (four screens, see `docs/console.md`) stays as it is. The four-screen limit in D1 (`docs/bot-decisions.md`) is lifted for the web client only.

## Decisions

Dated, from the product owner.

| Date | Decision |
| --- | --- |
| 2026-11-14 | **No web workspace.** The Slack bot is the interface, with a small console. *(Superseded by 2026-11-16.)* |
| 2026-11-16 | **Reopened.** Build a full web client. |
| 2026-11-16 | **Stack:** a TypeScript single-page app in the pnpm workspace. The server side stays in Rust, as already decided ("backend in Rust; frontend in TypeScript"). |
| 2026-11-16 | **D1's four-screen limit is lifted for the web client only.** The old console stays small. |
| 2026-11-16 | **Sign-in:** local user ID and password first. Entra ID (Azure AD) later, behind the same provider interface. |
| 2026-11-16 | **Accounts and roles live in a PM OS user database.** It is the source of truth for people, and `team.yaml` follows it. |
| 2026-11-16 | **Roles:** Admin, Product Manager, Reviewer, Auditor (see below). |
| 2026-11-16 | **Reviewer recommends; the draft's owner confirms.** |
| 2026-11-16 | **Auditor reads audit metadata only.** |
| 2026-11-16 | **Audit retention is 90 days**, and configurable. |
| 2026-11-16 | **Policy changes** need a merged pull request **and** a second admin's approval in the app. Only then does a host-side executor apply them. |
| 2026-11-16 | **Harness and context changes:** the app opens the pull request. Merging stays in GitHub. |
| 2026-11-16 | **Agent eval tracing** (Phoenix or similar) is deferred. |

## Architecture (sketch, not final)

```
Browser: TypeScript single-page app (pnpm workspace)
   │  user sign-in (local now, Entra later, one provider interface)
   ▼
Web server (Rust)
   ├─ user database (PM OS source of truth for people and roles; team.yaml follows it)
   ├─ scoped calls to the bot runtime and tool server, each carrying a signed user assertion
   ├─ audit chain (every action recorded; retention 90 days)
   ├─ connector registry (which systems PM OS reads and writes)
   └─ pull requests for harness and context changes ──► GitHub (merge stays there)

Policy changes: merged pull request + second admin's approval in the app
   └─► host-side executor applies the change
```

## Roles

| Role | May | May not |
| --- | --- | --- |
| **Admin** | Manage people and roles. Approve policy changes; a second admin must approve, so one admin cannot apply a change alone. | Apply a policy change without a merged pull request and a second admin's approval. |
| **Product Manager** | Use Ask, Task, Approvals and Routines. Own drafts and confirm or reject them. | Not decided: which admin areas, if any, it may see. |
| **Reviewer** | Recommend on a draft. | Confirm a draft. The draft's owner confirms it. |
| **Auditor** | Read audit metadata. | Read task content or audit detail beyond metadata. |

Not decided yet: the full permission matrix (which role may do which action, per screen and per connector), and who besides admins may propose a policy change. These must be written before Web 1 is signed off.

## Audit and retention

- Audit records are kept for **90 days**. The period is configurable.
- The Auditor role sees metadata only.
- Not decided: whether the 90-day period also applies to the 30-day trace store in D6 (`docs/bot-decisions.md`). Until it is decided, D6's 30 days stands.
- Not decided: the audit chain's design (how records are linked and checked).

## Phases

Named Web 1 to Web 4, so they are not confused with the Phase 0 to 3 of `docs/implementation-plan.md`.

| Phase | Scope | Gate |
| --- | --- | --- |
| **Web 1: foundations** | Roles; local sign-in; scoped web client with signed user assertions; audit chain; connector registry. | Not yet written. |
| **Web 2: core web app** | Ask, Task, Approvals, Routines, Auditor view, PM connectors page. | Not yet written. |
| **Web 3: admin areas** | Policy Admin, harness and context browser, connector admin. | Not yet written. |
| **Web 4: extras** | Eval gate; skill preview; tracing (Phoenix or similar, deferred per the decision above); Microsoft 365 connectors; Entra sign-in; SIEM export. | Not yet written. |

Each gate needs written pass criteria before its phase starts.

## Not decided yet

- **How Slack reads the user list from the user database.** Today the bot reads `team.yaml`, and onboarding writes it (`docs/bot-decisions.md`, decision 8). The change to read from the user database is not designed.
- **The gateway config file.** Its format and where it lives are not decided.
- **Whether the context library is editable in the web client.** The decision was written with `context/` as the name. The repo's context tree is `content/`. Confirm which one is meant before deciding.
- **Pass bars** for each phase gate.
- **Entra ID requirements:** tenant setup, which claims are used, group-to-role mapping, and the sign-in fallback when Entra is down. Not yet written.
- **The full permission matrix** and the audit chain design (see above).

## Links

- Decisions with dates: this doc. Earlier decisions: `docs/bot-decisions.md`, `docs/implementation-plan.md`.
- Current console: `docs/console.md`.
