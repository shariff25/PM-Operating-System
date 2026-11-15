# docs-qa: questions about your own docs

This category measures the Docs bot: a PM asks a question, the bot searches the company's
public docs (the site `pmos-docs` indexes, `PMOS_DOCS_BASE_URL`), answers from one page and
cites it. PM OS ships it as a **template**. The four tasks in `tasks/` are examples that run
against the fake docs connector's fixtures (`crates/toolserver/src/connectors/fake.rs`): two
Acme Vault pages at `https://docs.example.com/docs/vault-saml-sso` and
`https://docs.example.com/docs/vault-key-rotation`. Replace them with questions about your own
docs, then give the category a `pass_bar` once its scores are stable.

## Write a task

One file per task, `tasks/<id>.yaml`, with `id` matching the file name (lowercase and hyphens).

```yaml
# Checked against https://docs.example.com/docs/vault-saml-sso on 2026-11-15.
# The page says: Acme Vault supports SAML 2.0 single sign-on.
id: vault-saml-sso
ask: "Does Acme Vault support SAML single sign-on, and which SAML version?"
expect:
  must_cite:
    - https://docs.example.com/docs/vault-saml-sso
  must_include:
    - "SAML 2.0"
  must_not_include:
    - "<think>"
  approvals: 0
```

- **One fact per task.** Ask something one page answers, and put the fact the answer must state
  in `must_include` (a port, a limit, a version, a term the page uses). Short, distinctive
  strings work best; matching ignores case, dashes and curly apostrophes.
- **Cite the page.** `must_cite` lists the page URL as the bot links it: the page address
  without `.md`, no trailing slash. Query strings, fragments, `www.` and a `.md` suffix in the
  answer still match.
- **Check it against the live docs.** Open the page, confirm the fact is there, and note the
  URL, the date and the sentence in a comment at the top of the file. Re-check when the page
  changes; a stale expectation fails a correct bot.
- **Ask like a PM.** Paraphrase, and make the question self-contained: no Slack text, nothing
  the bot can't find from the ask alone (`evals/case-card.md` has the rules for cases that come
  from real conversations).
- **Spread the questions** across your products and the API reference, so one area doesn't
  dominate the score. Tasks that cite the same page are clustered together for the standard
  error (set `cluster:` to group them differently).
- **The Docs bot only reads,** so every task has `approvals: 0`, and `must_not_include: ["<think>"]`
  catches reasoning leaking into the answer.

## Should-decline tasks

Include a few questions public docs can't answer, such as roadmap dates, one customer's setup
or deal pricing. Name them `decline-*`, give them `must_decline: true` and no `must_cite`, and tag
them `model/answered-beyond-sources`. They pass when the answer says its sources don't cover the
question (the `decline_phrases` in `evals/config.yaml`). Search the docs first (`docs.search`)
to make sure no page does answer it.

## Repeats and the bar

`category.yaml` sets `repeats: 3`: each task runs three times, so the report shows pass^3 and a
standard error, and the gate can tell a real drop from noise. Leave `pass_bar` unset while the
category only reports; add it (for example `pass_bar: 0.8`) once your own tasks are in and
the scores are steady.

## Running against your docs

The example tasks need the fake docs connector, which the tool server uses with
`PMOS_CONNECTORS=fake` and no `PMOS_DOCS_URL`. Tasks about your own site need the eval tool
server pointed at the real `pmos-docs` (`PMOS_DOCS_URL` and `PMOS_DOCS_TOKEN`), with
`PMOS_DOCS_BASE_URL` set on the docs server. Run `pmos-evals check` after every change; see
`docs/evals.md` for running the benchmark.
