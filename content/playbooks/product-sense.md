---
title: Product sense
owner: "TODO"
version: "0.2.0"
---

# Product sense

Product sense is choosing the right problem for the right user, then the smallest thing that
solves it. Work through the steps in order. Most weak proposals skip the first four.

## 1. Start from the user and their job
- Name a person in a situation, not a market: "a platform engineer who owns the secrets for
  40 services", not "mid-market companies".
- Write the job as progress they want: "When <situation>, I want to <motivation>, so that
  <outcome>." If the sentence names your feature, it is a solution, not a job.
- Separate the user from the buyer. The person who signs and the person who suffers are often
  different, and each has their own job.

## 2. Segment, then pick one
- Split users by what changes their needs: how they work, how often they hit the problem,
  what they use today. Company size and title rarely do on their own.
- Pick one segment to win first. Say why (size, pain, reach, fit with strategy) and name who
  you are choosing not to serve yet.
- Open every average. "Activation is 31%" can be 34% for teams and 27% for solo users, and
  those two numbers ask for different products.

## 3. Rank the pains
- List the pains inside the job. Rank by frequency × severity: how often it happens, and what
  it costs each time (hours, money, risk, an incident).
- A weekly irritation and a yearly disaster can both rank high. A rare irritation never does.
- Give the evidence for each: number of accounts, tickets, observed sessions. A pain with no
  evidence is a hypothesis; label it.

## 4. Find what they do today
- The real competitor is the workaround: the spreadsheet, the script, the colleague they ask,
  or doing nothing.
- The workaround sets the bar. Your solution must beat it by enough to justify switching.
- What the workaround costs them is the best evidence of what solving the problem is worth.
  If they do nothing and nothing bad happens, question whether the pain is real.

## 5. State the insight
- One sentence on why the problem persists that the request does not already say: "They skip
  X not because of A, but because of B."
- A real insight rules some solutions out. If it rules nothing out, it restates the problem.

## 6. Generate three solution shapes
- Three options that differ in kind, not size: remove the step, automate it, or make it safe
  to do by hand. Small, medium and large versions of one idea count as one option.
- Always include the solution that was asked for, and the cheapest thing that could work: a
  default, a setting, a doc, a service done by hand.

## 7. Pick, with the tradeoffs explicit
- Pick one. For the pick and each loser say: which pain it removes, for which segment, what
  it costs, what it gives up.
- State the bet: "We believe <segment> will <change in behaviour> if <solution>, because
  <insight>."
- Ask what the customer does differently the week after it ships. If nothing, it is a nicety.

## 8. Find the smallest version that tests the bet
- Cut to the part that proves or disproves the bet with real users. It tests the riskiest
  assumption; it is not a thinner copy of the whole feature.
- Set the result that would make you stop: a number, a date and how it is measured.

## 9. Say what you would cut
- For each item ask: if this were missing, would the bet still be tested? If yes, cut it.
- First to go: anything added for one account, for completeness, or "while we're in there".

## Traps
- **Solution first.** The request names a feature; the problem is "users lack the feature".
  Find who has the problem and what they do today in the sources before anything else; ask
  the PM only if the sources don't say, as one question with a default.
- **Averages hiding segments.** One number for everyone, when the segments move differently.
- **Building for the loudest customer.** One large account asking four times is one account.
  Check the ask against the segment you chose.
- **Asking users what to build.** Users are reliable about pains and past behaviour, and
  unreliable about solutions and what they would do in future.
- **No behaviour change.** The feature ships, usage shows up, and nobody does their job
  differently.

## Worked example: "build a rotation reminders dashboard" (Acme Vault, fictional)
- **User and job.** A platform engineer who owns secrets for about 40 services. "When a
  credential has to change, I want to change it everywhere at once, so that nothing breaks."
- **Segments.** (a) Platform teams that deploy through a pipeline. (b) Security leads who
  must prove rotation to an auditor. (c) Small teams that deploy by hand. Pick (a): they do
  the rotating, and (b) gets its proof only if (a) rotates. Not serving (c) yet.
- **Pains, ranked.** 1. A rotation causes an outage because one service still holds the old
  value: rare, but an incident each time. 2. Not knowing which secrets are overdue: quarterly,
  moderate. 3. Copying new values by hand: weekly, mild.
- **Today.** A spreadsheet of which service uses which secret, a chat announcement, and a
  rotation at a quiet hour. Many teams do not rotate at all. The competitor is not rotating.
- **Insight.** Teams skip rotation not because they forget, but because they cannot see who
  still reads the old value. A reminder attacks forgetting, which is not the problem.
- **Three shapes.** 1. Reminders dashboard (the ask). 2. Show which clients still read the old
  version, and keep old and new valid for an overlap window. 3. Fully automatic rotation for
  supported databases.
- **Pick: 2.** It removes pain 1 for segment (a). It costs more than 1 and automates nothing.
  Shape 1 is cheap and changes no behaviour. Shape 3 covers few backends and asks customers
  to trust us with write access to their databases on day one.
- **Bet.** Platform teams will rotate more often if they can see that nothing still reads the
  old value.
- **Smallest test.** Database passwords only: last read per client per version, and a 24-hour
  overlap, with ten design partners. The bet holds if rotations per workspace double within
  a quarter and rotation-linked incidents do not rise.
- **Cut.** The dashboard, email reminders, a policy engine, every other secret type.
