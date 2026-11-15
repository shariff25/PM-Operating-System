---
title: Levels
owner: "TODO"
version: "0.1.0"
---

# Levels

A level sets the altitude of the work: how wide the scope is, how far ahead it looks, who
reads it and what the recommendation is about. The system prompt names your PM's level:
Senior (the default), Staff or Principal. Work at that level unless the request says
otherwise. The bar in `/content/playbooks/bar.md` applies at every level; the level adds
sections and checks on top of it.

| | Senior | Staff | Principal |
| --- | --- | --- | --- |
| Scope | One product area | Several teams or a product line | The company and its market |
| Horizon | This quarter and next | 4 to 6 quarters | 2 to 3 years |
| Audience | The team and its stakeholders | Directors and peer leads | Executives and the board |
| The recommendation is about | What to build | Where to invest and what to stop | Which bets define the business |
| Evidence expected | Customers and usage | Across teams, with cost | Market, business model and competitors |
| Extra sections | none | Cross-team effects | Cross-team effects, What must be true |
| Reviewers added by default | none | `executive`, `engineering-lead` | `cfo`, `cto`, `ceo` |

## What each level means in practice

### Senior
- **Scope:** one product area and the squads that build it. Name the users of that area and
  the problem in front of them.
- **Horizon:** this quarter and next. Dates are weeks and sprints; anything later is a
  follow-up, not a plan.
- **Audience:** the team, its engineering and design leads, and the stakeholders who sign
  off. They know the product; they need the problem, the pick and the scope.
- **The recommendation is about** what to build, and what to cut from it. It ends in
  requirements, an experiment or a decision the team can act on next sprint.
- **Evidence:** customer calls counted by account, usage, support tickets, the backlog.

### Staff
- **Scope:** several teams or a whole product line. The unit is the team, not the feature.
  Say which teams the work touches and which ones it takes people from.
- **Horizon:** 4 to 6 quarters. Dates are quarters. Sequence matters more than scope: what
  must land first so the next team is not blocked.
- **Audience:** directors and peer leads. They do not know each team's detail; they need
  the choice, its cost in people and what stops.
- **The recommendation is about** where to invest and what to stop. A Staff document that
  funds everything and stops nothing has not made a choice.
- **Evidence:** counts and dependencies across teams (`jira.rollup`), cost in people and
  quarters, and the customer evidence for each investment. A status meeting is not evidence.

### Principal
- **Scope:** the company and its market. The unit is the business: a segment, a product
  line, a pricing model, a platform.
- **Horizon:** 2 to 3 years. Dates are halves and years. Say what must be true for the bet
  to pay off and the signpost that would show it is failing.
- **Audience:** executives and the board. They read one page. The ask comes first, then the
  business effect as a range, then what is hard to undo.
- **The recommendation is about** which bets define the business: what the company is
  known for, how it makes money, and what it stops doing to afford it.
- **Evidence:** the market, the business model and competitors, with the numbers from tools
  (`stats.cashflow` for payback and net present value), never by hand.

## One request at three altitudes
The request: "single sign-on for small teams" on Acme Vault (fictional).

| | Senior | Staff | Principal |
| --- | --- | --- | --- |
| The recommendation | Build OpenID Connect sign-in for Team workspaces above 20 seats; pre-sell to 50 waiting-list workspaces first | Fund identity for two quarters by moving one squad off enterprise reporting v2; sequence it after the pipeline integrations; stop the hobby tier | Make identity the paid line between Team and Enterprise: a $1,200-a-year add-on now, folded into a new Business plan by 2028 if attach passes 15% |
| The evidence it leans on | 6 of 9 Team-plan accounts called in October asked for it; 17 workspaces with leavers still active | The rollup of the identity, reporting and integrations projects: open issues, overdue work and who blocks whom; the people each option takes | 1,240 Team workspaces above 20 seats; a $200,000-a-year base case (range $120,000 to $310,000); payback and NPV from `stats.cashflow`; how Initech packages identity, with a source |
| Written for | The identity squad, its engineering lead, the VP Product who decides | The product and engineering directors, and the reporting and integrations leads who lose people | The CEO, CFO and CTO, and the board deck that follows |
| Ends with | Requirements and a pre-sale test | `## Cross-team effects` | `## Cross-team effects` and `## What must be true` |

The same facts can appear at every level. What changes is the question the document
answers: what to build, where to put the people, or what the business becomes.

## Extra sections
They come after the template's own headings, in this order. A document from `strategy-doc`,
`decision-doc` or `prd-draft` gets them; the other Staff and Principal templates already
carry the same content under their own headings.

- **Staff and Principal: `## Cross-team effects`.** Which teams pay (people, quarters, a
  delayed project), what each stops or delays, and who must agree before it starts. One
  line per team. Name teams and roles, not people's motives.
- **Principal only: `## What must be true`.** The two or three conditions the bet depends on
  over 2 to 3 years, each with the signpost that would show it is failing and when you
  would expect to see it.

## Extra bar checks
Run them with the seven checks in `bar.md`, in the same pass.

- **Staff:** the document says what stops or isn't funded, and names the teams affected.
  - **Passes when:** at least one named project, team or request is stopped, delayed or left
    unfunded, with the team that loses it.
  - **Fails when:** every option is funded, or the cost is "some engineering time".
- **Principal:** the business effect (revenue, cost or margin) is a range with its
  assumptions, and the document says what is hard to reverse at company scale.
  - **Passes when:** the effect is a low and a high with the assumptions that drive each,
    and one paragraph names what customers, contracts, prices or architecture would be
    hard to undo.
  - **Fails when:** the effect is a single number or an adjective, or nothing is called
    hard to reverse.

## Default reviewers by level
The `review` skill adds these to its choice by document type, always with `senior-pm`, and
still runs at most 4.

| Level | Added by default |
| --- | --- |
| Senior | none |
| Staff | `executive`, `engineering-lead` |
| Principal | `cfo`, `cto`, `ceo` |

At Principal, `senior-pm` plus the three leaves no room: say which document-type reviewers
were left out and offer to run them next.

## When the request sets its own audience
The request wins for that request only. "Write this for the exec team" is Principal
audience: the ask first, one page, the business effect as a range. "Just the squad view" is
Senior scope. Keep the PM's level for everything else, and say in one line which altitude
you used when it differs from their level.
