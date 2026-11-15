---
title: Strategy
owner: "TODO"
version: "0.1.0"
---

# Strategy

A strategy is a diagnosis, one guiding choice and a few actions that reinforce each other. A
list of goals is not a strategy; neither is a list of everything we could build.

## 1. Diagnosis
- Say what is going on, in one paragraph, with the evidence. Find the crux: the one obstacle
  that, once removed, makes the rest easier.
- A good diagnosis is uncomfortable and specific: "small teams leave before their first
  secret is read", not "we need to improve onboarding".
- Include what is true about us: where we are strong, what we cannot do this year.

## 2. Guiding choice
- One sentence that picks a direction and so rules others out: "Win <who> on <what>, by
  <how>, instead of <the tempting alternative>."
- Test it: could a sensible competitor choose the opposite? If nobody would, it is a slogan
  ("be the most trusted vault").
- **Where to play:** which customers, which jobs, which channel. **How to win:** the reason
  they pick us over the workaround and the competitor, that we can sustain.

## 3. Coherent actions
- Three to five actions that follow from the choice and make each other stronger. If one
  could be dropped with no effect on the others, question whether it belongs.
- Each has an owner, a date and the measure it moves.
- Name the resources moving: which team stops what to do this. A strategy that moves nothing
  changes nothing.

## 4. What we won't do
- List the good ideas you are declining, and why. Bad ideas need no listing.
- Each line should cost something: a segment not served, a request refused, a deal walked
  away from. If no one would object, it is not a real non-goal.

## 5. Reversible and one-way decisions
- **Reversible** (a default, a UI, an experiment): decide fast with partial evidence, let the
  owner decide, set a review date.
- **One-way** (pricing people have paid, a public API, a data model, a partner in the login
  path, a promise to a customer): slow down, write the options, get the right people in.
- For each big decision say which kind it is, what undoing it would cost, and how to make a
  one-way decision more reversible: a pilot, a flag, a contract term, an abstraction layer.

## 6. Sequencing and prioritising
- Order by what must be learned first, not by what is easiest. Put the action that tests the
  riskiest assumption at the front.
- To rank a list, score each item on reach (how many in the chosen segment), impact on the
  target metric, confidence (the evidence behind it) and effort. Then check the order against
  the strategy: a high score that serves the wrong segment loses.
- Say what is below the line and stays there. Ten priorities is no priority.
- Scores are an aid, not the answer. State where you overrode them, and why.

## 7. Kill criteria and the pre-mortem
- Set, in advance, the result that stops or changes the plan: a metric, a threshold, a date.
  "If activation is under 35% on 2027-03-31, we stop the integrations work."
- Write them when you are calm. Sunk cost argues loudest at the review.
- Pre-mortem: assume it is a year on and the plan failed. List the most likely reasons, rank
  by likelihood × damage, and for the top three give the early warning sign, the cheapest
  test now, and the mitigation.
- Include one reason outside the team's control (a competitor move, a dependency, a rule
  change) and one caused by success (load, support volume, cannibalised revenue).

## Traps
- **Goals as strategy.** "Grow self-serve revenue 70%" is a target. The strategy is the
  choice that makes it reachable.
- **No choice.** Everything is a priority, every segment is served, nothing is declined.
- **Incoherent actions.** Each team's wish list under a shared heading.
- **Strategy by competitor.** Matching a rival's features cedes the choice to them.
- **Unfalsifiable.** No metric, no date, no kill criterion, so it can never be wrong.

## Worked example: Acme Vault for small teams (fictional)
- **Diagnosis.** Enterprise growth is capped by sales capacity. About 7,800 small-team
  workspaces sign up each month, and 31% read a secret from an application in their first
  week; 2.1% pay within 90 days. Teams that activate pay far more often. The crux is the time
  between sign-up and the first secret read in production, not missing features.
- **Guiding choice.** Win engineering teams of 5 to 50 on the fastest path to a first
  production secret, self-serve, instead of adding sales people or chasing solo developers.
- **Actions.** 1. Onboarding that ends at a secret read from a real service. 2. Integrations
  for the three most used deploy pipelines. 3. Self-serve single sign-on on the Team plan.
  Each raises the share of sign-ups that reach value without talking to us.
- **Won't do.** A hobby tier. Sales outreach below a set contract size. Self-hosted features
  for the Team plan.
- **One-way decisions.** Team-plan pricing, once announced. A third party in the login path.
  Both get a decision document; the onboarding changes do not.
- **Sequence.** Onboarding first: it tests whether activation drives payment, the riskiest
  assumption. Integrations and single sign-on follow only if it does.
- **Kill criterion.** If first-week activation is under 35% on 2027-03-31 after the
  onboarding work ships, stop and revisit the diagnosis.
