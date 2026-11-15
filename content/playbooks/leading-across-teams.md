---
title: Leading across teams
owner: "TODO"
version: "0.1.0"
---

# Leading across teams

A Staff PM's product is the portfolio: which teams work on what, in what order, and what
nobody works on. The craft is choosing, sequencing and stopping, then getting the choice
made by people you do not manage.

## 1. Portfolio thinking: fund, sequence, stop
- Every item in a portfolio gets one of three verbs: **fund** (people and a date),
  **sequence** (after something named), or **stop** (and who is told).
- **The cost of a yes is the next best use of the same people.** "Two squads for a quarter"
  is not a cost until you say what those squads would otherwise ship.
- Fund few things fully rather than many things partly. Half a squad on four bets ships
  none of them.
- Stopping is a decision, not a drift. Write down what stops, tell the teams and the
  customers who asked, and close or relabel the Jira issues.

## 2. Reading a portfolio
- Read it from counts and dependencies, not from status meetings. Call `jira.rollup` and
  quote its numbers: issues by project or epic, how many are to do, in progress, done,
  overdue and unassigned.
- **Signals worth a line:** a project with many to do and few in progress (starved); overdue
  work with no assignee (orphaned); a group where everything is in progress (too much
  started); a fix version whose count keeps rising (scope creep).
- Counts say where to look, not why. Read the epics behind the outliers before you judge.
- If the rollup is `truncated`, say so and narrow the query. Never extrapolate a total.

## 3. Dependencies and conflicts
- List who is blocked by whom from the rollup's `links`: the issue, its team, the issue it
  blocks and that issue's state.
- **The critical chain** is the longest run of blocked-by links to the outcome. Its slowest
  link sets the date for everyone. Put your attention there, not on the busiest team.
- **A conflict** is two teams needing the same people, the same component or the same
  release window. Name both sides and the decision that settles it.
- A link that leaves the query (`to_group` is `null`) points at a team outside your view.
  Find its owner before you promise a date.

## 4. OKRs
- **One objective:** an outcome for a customer or the business, in words. "Small teams reach
  value without talking to us", not "ship onboarding v2".
- **At most four key results,** each a number with a baseline, a target, a date and a
  source. Outputs (launches, features, documents) are not key results; they are how you
  hope to move one.
- A key result without a baseline is a guess. If nobody has measured it, the first piece of
  work is measuring it.
- Write what you won't chase: the metrics you will let sit, so teams don't optimise them.

## 5. Business reviews
- **Results against plan first:** each committed number, what it was, what it is, the gap.
- **Then the why:** the one or two causes, with evidence. "We shipped late" is a fact; the
  cause is what made it late.
- **Then the change:** what the team does differently next period, and what it stops.
- No activity lists. "Held 14 customer calls" belongs in the evidence for a why, or nowhere.

## 6. Getting a decision made without authority
- **Who decides:** one role. If two people think they decide, settle that first.
- **Who must agree:** the leads whose teams pay, and anyone who can veto (security, finance,
  a platform owner).
- **Each one's likely objection,** stated as their best case, and your answer with evidence.
  Name roles and what their team loses; never guess at a person's motives.
- **The order:** the veto holders and the most affected teams first, one to one, before
  any group meeting. Nobody should hear their team loses people in a meeting.
- **The fallback:** what you do if the decider says no, or if agreement isn't reached by a
  date. A smaller version, a pilot, or a stop.

## 7. Coaching another PM
- Teach, don't rewrite. Your edit fixes one document; their judgement fixes the next ten.
- Pick the three changes that matter most, against `bar.md`. Ask each as a question first
  ("what would make the VP say no?"), then offer a suggestion.
- Say one thing done well that is worth repeating, specifically.
- Give feedback to the PM who asked for it. They decide what reaches the author.

## Worked example: Acme Vault, H1 2027 (fictional)
- **The rollup** of `VLT`, `IDN` and `RPT` by project: identity has 34 issues, 22 to do and
  4 overdue; reporting has 51, with 30 in progress across 6 engineers. Too much started on
  reporting, a starved identity backlog.
- **The links:** 6 identity issues are blocked by the platform team's session service
  (`PLT`), which is outside the query. That is the critical chain for single sign-on.
- **The choice:** fund identity with one reporting squad for two quarters; sequence single
  sign-on after the session service change lands; stop enterprise reporting v2 until Q3.
- **The cost of the yes:** reporting v2, which two Enterprise renewals asked for.
- **Who must agree:** the reporting lead (loses a squad), the platform lead (owns the
  blocker), the Enterprise account director (two renewals). The VP Product decides.
- **The order:** platform lead first (without the session change nothing moves), then the
  reporting lead, then the account director with a plan for the two renewals.
- **The fallback:** if the reporting lead's objection holds, identity gets three engineers
  from integrations for one quarter and single sign-on slips to Q3.

## Traps
- **Funding everything.** A portfolio with no stops is a wish list.
- **Status theatre.** Green slides and no counts.
- **Outputs as key results.** "Launch X by June" measures effort, not effect.
- **Surprise in the room.** A lead hears their team loses people in a group meeting.
- **Rewriting a peer's document.** It fixes the page and teaches nothing.
