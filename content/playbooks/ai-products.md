---
title: AI products
owner: "TODO"
version: "0.1.0"
---

# AI products

A model-backed feature is wrong some of the time, and its behaviour changes when the model,
the prompt or the data changes. So specify behaviour by example, make evals the acceptance
criteria, and design for the moment the model is wrong.

## 1. Specify the intended behaviour
- Write what the feature should do, what it must never do, and what it does when unsure:
  ask, decline or hand over to a person.
- Specify by example. For each main case give an input, a good output and a bad output, and
  say why the bad one is bad. Include edge cases: empty input, ambiguous requests, requests
  outside scope.
- The examples are the spec. If two people would grade an output differently, the spec is
  not finished.
- State what the model can see and what it can do. Every tool or piece of data it can reach
  widens what can go wrong.

## 2. Evals are the acceptance criteria
Write the eval plan before the feature is built. It replaces "it looked good in the demo".
- **Datasets:** typical cases from real use, edge cases, and adversarial cases. Say where
  each came from, how many, and who labelled them. Keep a held-out set nobody tunes on.
- **Graders:** code where the answer can be checked exactly (a schema, a simulator, a match).
  A model grader with a written rubric where judgement is needed, checked against human
  labels before it is trusted. People for a calibration sample and for disputes.
- **Thresholds:** set before the first run. Separate hard gates (never grants more access
  than asked) from targets (95% judged correct). One failure of a hard gate blocks launch.
- **Slices:** report by segment, language, input length and risk category. An average of 95%
  can hide a slice at 70%.
- **Regression set:** every failure found in testing or production becomes a case. Run it on
  every change of prompt, model or retrieval source.
- Name who reviews failures, how often, and what they can decide.

## 3. Failure modes
For each, say how likely, how bad, how it is detected and what the user sees.
- **Hallucination:** states something false or invents a name, number or source. Ground
  answers in retrieved data, show sources, and check outputs against real records.
- **Wrong refusal:** declines a legitimate request. Measure it; a safe feature nobody can use
  has failed.
- **Over-reach:** does more than was asked, or acts where it should have proposed.
- **Prompt injection:** text in a document, ticket or field name is followed as an
  instruction. Treat all content as data, and keep write actions behind approval.
- **Drift:** quality changes after a model update, a prompt edit or a shift in the inputs.
  The regression set and production monitoring catch it.

## 4. Misuse and safety
- Ask who would abuse it and what the worst output is. Design for that person, not only the
  intended user.
- The feature must not let a user do through the model what they may not do directly. It
  acts with the user's permissions, never more.
- Say what data goes to the model, where it is processed, what is kept and for how long.

## 5. Cost and latency budgets
- Set a budget per request and per active user per month, and a latency target at p50 and
  p95. Say what the user sees while they wait.
- Say what happens over budget: a smaller model, a shorter context, a cap, or a queue.
- Check the unit economics: cost per successful task against what the task is worth.

## 6. Human oversight and fallbacks
- Match oversight to the cost of an error and how easily it is undone: suggest; draft for
  approval; act with undo; act alone. Start one step more cautious than you think you need.
- Show the reviewer what they need to judge: what will change, and why.
- Approval fatigue is a failure mode. If people approve everything, the review is not
  oversight.
- Always have a path that works without the model: when it is down, unsure or over budget.

## 7. Staged rollout with monitoring
- Stages: internal, design partners, a small percentage, everyone. Each gate is the eval
  thresholds plus production signals: acceptance rate, edit rate, reverts, user reports.
- Have a kill switch that someone on call can use, and say who.
- Re-run the evals on every model or prompt change before it reaches users.

## 8. When the model is wrong
- The user can see why (sources, the proposed change), correct it, undo it and report it.
- The team triages reports within a set time, adds the case to the regression set and fixes
  the cause, not the instance.
- A harmful error is an incident, with an owner and a review.

## Worked example: policy assistant (Acme Vault, fictional)
- **Job.** An engineer needs to give a service access to a few secrets, and does not know
  the policy language. Today they copy a broad policy from another service.
- **Behaviour.** Turns a plain request into a least-privilege policy draft. Never applies it.
  Asks when the request is ambiguous. Refuses wildcard access at the root.
- **Good and bad.** "Let payments read its database password in production." Good: read on
  that one path for that one service. Bad: read on everything under production.
- **Evals.** 400 request and policy pairs plus 120 adversarial ones. A policy simulator
  checks that the draft grants exactly the intended access. Hard gate: zero drafts broader
  than asked on the adversarial set. Target: 95% exact on typical requests, at least 90% in
  every slice.
- **Failure modes.** Invented secret paths: only paths in the workspace inventory are
  allowed. Injection through secret descriptions: descriptions are data. Wrong refusals:
  capped at 3% of legitimate requests.
- **Oversight.** Every draft is a diff with the simulator's summary ("grants 1 service read
  on 1 path"), approved by a policy admin. The form-based policy builder remains.
- **Budgets.** p95 under 6 seconds; a cost ceiling per request, with a smaller model as the
  fallback.
- **Rollout.** Internal, then ten design partners, then 10% of workspaces. Gate on the evals
  and on policies reverted within 24 hours of approval staying under 2%.
- **When wrong.** One-click revert, a report button, and the case joins the regression set.
