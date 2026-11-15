---
title: Company bets
owner: "TODO"
version: "0.1.0"
---

# Company bets

A Principal PM chooses and tends the few bets that define the business. A bet is a belief
the company spends real money on, with a way to know within a year or two whether it was
right.

## 1. What a bet is
- **A belief with a metric:** "Teams above 20 seats will pay for self-serve identity",
  measured by add-on attach rate.
- **A riskiest assumption:** the one thing that, if false, makes the rest irrelevant.
- **A test** cheaper than the bet, and a **review date** when the evidence is read.
- A bet without a review date is a commitment by default. Write the date down when you open
  it, and keep it in the bets register (section 9).

## 2. Vision
- Write it as a customer narrative: a named kind of customer, 2 to 3 years from now, doing
  their job differently because of the product. Not a list of features.
- Then **what must be true**: two or three conditions in the market, the business model or
  the technology. Each one gets a **signpost**: the evidence, and the date, that would show
  it is failing.
- Say what the company won't be. A vision every competitor could sign is a slogan.
- Give the path as two or three steps, each one a bet that teaches something before the next.

## 3. Investment cases
- **Scenarios, not a point:** low, base and high, each with its stated assumptions (attach
  rate, price, seats, cost to serve, people).
- **Payback and net present value always come from the `stats.cashflow` tool.** Quote
  `payback_years` and `npv` exactly as returned; never do the arithmetic yourself.
- Say which input moves the answer most. That input is the riskiest assumption.
- Inputs with no source carry `[unverified]`. A case built on unverified inputs says so in
  its first lines.
- Name the alternative use of the money: what the same people and dollars would fund.

## 4. Pricing as a product decision
- **The value metric** is what the price scales with: seats, secrets, workspaces, requests.
  It should grow as the customer gets more value, and be easy to predict on an invoice.
- **Fences** separate packages: a feature, a limit, a support level. A good fence is one
  that the customers who value more cross on their own.
- **Who pays more and who pays less:** name the segments, how many accounts, and by how
  much. A pricing change with no losers has not been looked at closely.
- **Migration:** what existing customers pay, when, and with what notice. Grandfathering has
  a cost and an end date.
- Never state a competitor's price without a source and a date.

## 5. Build, buy or partner
- **Build** what is the reason customers pick you, and what you must control to keep a
  promise (for a secrets product: the login path).
- **Buy** what is commodity and slow to build. **Partner** for reach, or for what moves too
  fast to own.
- **What becomes a platform:** a capability two or more products need in the same form.
  Until two need it, it is a product feature with a clean interface.
- Write it with engineering. A platform strategy without the CTO's view of what scale
  breaks is a wish.

## 6. What is hard to reverse at company scale
- Prices customers have paid, a public API, a data model, a partner in a critical path,
  promises in contracts, a brand position, a reorganisation.
- For each: what undoing it costs, and how to make it more reversible (a pilot price, a
  versioned API, a contract term, an abstraction layer, a sunset date).
- Two-way decisions go fast. One-way decisions get a written case and the executives.

## 7. Writing for executives
- **The ask first,** in one sentence: approve, fund, stop or choose. Then why now.
- One page. The business effect as a range with its assumptions; the options with what each
  costs; the one risk that would make them say no.
- End with the next step and its date. Everything else goes in a linked appendix.

## 8. Signposts and killing bets
- Read the evidence on the review date, not when it feels convenient.
- **Confirm** only when the metric or the test result supports it. **Refute** on the same
  standard. **Drop** when the question no longer matters.
- Kill on evidence, early. The sunk cost argues loudest at the review.

## 9. Keeping a bets register
- One entry per bet, in the PM's own memory, kept by the `bets.*` tools: the bet, the
  metric, the riskiest assumption, the test, the review date, dated evidence with sources,
  and each decision.
- The monthly review proposes updates for bets due within 7 days. Each change is approved.
- A status changes only with the evidence that justifies it, recorded in the same update.

## Worked example: identity as the Business plan (fictional)
- **The bet:** teams above 20 seats will pay for self-serve identity, so it becomes the fence
  between Team and a new Business plan.
- **Metric:** add-on attach among the 1,240 Team workspaces above 20 seats. **Riskiest
  assumption:** 15% to 30% attach at $1,200 a year. **Test:** the pre-sale to 50
  waiting-list workspaces. **Review by:** 2026-12-18.
- **Investment case:** $180,000 upfront; three scenarios from `stats.cashflow` at a 10%
  discount rate pay back in 2.4 (low), 1.3 (base) and 0.8 years (high).
- **Hard to reverse:** a Business plan price once customers pay it. Make it reversible by
  launching as an add-on first; fold it into a plan only after a year of attach data.
- **Signpost:** if attach is under 10% by 2027-09-30, identity stays an add-on and the
  Business plan is dropped.
