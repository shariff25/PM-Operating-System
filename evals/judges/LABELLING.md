# Labelling a judge: instructions for the person labelling

Each judge here asks one yes-or-no question about a bot's answer (its `question` is in
`<judge>.yaml`). Your labels tell us how often the judge agrees with a person. Until a judge has
at least 100 labels and agrees with them at least 80% of the time both ways, it only advises.

**Where.** `<judge>.labels.jsonl`, one example per line. A line waiting for you looks like:

```json
{"input":"The request: …","output":"…the bot's answer…","label":null,"from":"structure-prd-draft run 2","judge_said":"yes"}
```

**What to do, for each line with `"label":null`:**
1. Read the judge's `question` in `<judge>.yaml`.
2. Read `input` (what the person asked, and what the bot read) and `output` (what the bot
   answered or drafted).
3. Answer the question yourself, from those two alone. Replace `null` with:
   - `true` if your answer is **yes** (the output passes),
   - `false` if your answer is **no**,
   - `"unknown"` if what is shown is not enough to decide. Use it honestly; it is not counted
     either way.
4. Leave everything else on the line as it is.

**Rules.**
- Decide for yourself. **Don't look at `judge_said`**: it only records what the judge said, so
  the pack mixes yes and no answers. Copying it would make the calibration worthless.
- The examples come from runs on fake fixture data. Slack tool results were left out of
  `input` on purpose (labels hold no Slack text); if the question can't be answered without
  them, use `"unknown"`.
- Never paste real Slack messages, customer data or tokens into a line. Fix a line by
  paraphrasing, or delete it.
- One person labels a line. If two people label the same pack, keep both files apart and
  compare them before committing.

**When you're done:** commit the file. Then someone with the gateway runs
`pmos-evals judge calibrate <judge>` and commits `<judge>.calibration.json`
(`docs/evals.md`, "Labelling a judge").
