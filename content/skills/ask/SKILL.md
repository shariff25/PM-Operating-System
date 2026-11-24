---
name: ask
description: Answer a question about the company's products, customers, partners, competitors or past decisions from the team context library, Confluence and Jira, with a citation for every claim. Use for quick factual questions; use prd-draft or other skills to produce documents.
metadata:
  owner: "TODO"
  version: "0.2.0"
  model: "default"
  data-class: "restricted"
  tools: "context.search context.read confluence.search confluence.read jira.search jira.read docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "phase-0"
  track: "Everyday"
---

# /ask

Answer the PM's question from the team's own sources.

## Steps
1. Restate the question to yourself in one line. A broad ask ("find out about X") is a
   question to answer: what the sources say about X.
2. Search before you ask. Search the context library first (`context.search`), then
   Confluence and Jira if the context library does not answer it. For product terms, how-to
   and configuration questions, also search the company's public docs (`docs.search`). Read
   the most relevant two to five results in full.
   - If the question is still ambiguous in a way that changes the answer, answer the likeliest
     reading, and ask one question with a default the PM can accept in one word.
3. Answer in at most five sentences or one short table. Lead with the direct answer.
4. Cite every factual claim inline as `[source](link)`. Claims you cannot cite get
   `[unverified]`.
5. If sources disagree, say so and cite both. Prefer the file with the later
   `last-reviewed` date, and say which one you preferred.
6. If a context file you used is marked stale, add one line: "Source last reviewed <date>;
   flag it to <owner>."

## Do not
- Guess. "I could not find this in the team's sources" is a good answer.
- Quote Restricted material when the PM says the answer is for a customer or partner.
