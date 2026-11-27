# Connector maps

A connector map tells a gate how to read one MCP server's tool calls as actions: which
operation a tool is, whether it reads, writes or destroys, whether it can be put back,
whether it reaches people outside, and which resource it acts on. A call to a tool with no
map entry, or whose definition has changed since the map was made, is recorded as
`unknown` and can never be allowed where anything is enforced.

Each directory holds one server at one version:

| File | What it is |
|---|---|
| `listing.json` | The server's own answer to `tools/list`, kept as given. It fixes each tool's digest. |
| `review.json` | What a person decided about each tool, with a note where the choice is not the obvious one, and `source`: what was reviewed and how the listing was got. |
| `<server>.json` | The map: `gov map build` joins the two. Never edited by hand. |
| `<server>.samples.json` | Sample calls and the action each is to be read as; every tool has at least one. |

## Commands

```
gov map build --listing listing.json --review review.json --out <server>.json
gov map drift --listing <a later listing> --map <server>.json [--review review.json]
```

`build` refuses unless every tool the server lists is reviewed or set aside by name with a
reason, and refuses a review of a tool the server does not list. `drift` exits non-zero and
names the tools a later listing has added, removed or changed in any field (a rewritten
description is a change: that is where an instruction to the agent would be put).

A gate's operator address (`/healthz`) shows `unmapped_by_tool`: how many calls the map
could not read, by the operation each was recorded under. `drift` prints that operation
beside each tool it names, which is how a count is matched to a tool.

To use a map, copy `<server>.json` and `<server>.samples.json` into a policy directory's
`connectors/`; `gov policy check` runs the samples again before anything is signed.

## What a map here does and does not claim

- **It was reviewed against the server's listing, not against the live system**, unless its
  `source` says otherwise. None of the three maps here has been run against the real system.
- **It is for one version of one server.** Another version is another listing: run `drift`.
- **It reads a call's arguments; it cannot see what the system does next** (a workflow rule,
  a notification scheme). Where a tool can do several things in one call, it is mapped as
  the widest of them.
- **Some tools name no single resource** (a free-text search, a list of issues). They are
  mapped to a resource of their own (`jira/search`, `jira/batch`), so a grant on one project
  never reaches them; a grant for one of them is a grant across the whole system.
- **A resource is built from string arguments that are plain identifiers.** A number (a
  GitHub issue or pull request) is not part of it, so GitHub calls are held to their
  repository and no finer; a name with a character outside an identifier's is recorded as
  `unknown`, not guessed at.
- **A tool with several methods is one tool to a map**, mapped as the widest thing any of
  its methods does.
- Each map is read by the security reviewer before it backs anything but observe mode.

## Maps

| Directory | Server | Tools |
|---|---|---|
| `jira/` | `mcp-atlassian` 0.24.8, Jira tools | 63 mapped, 0 set aside |
| `github/` | `github-mcp-server` v1.14.0, every toolset | 91 mapped, 0 set aside |
| `slack/` | `@modelcontextprotocol/server-slack` 2025.4.25 | 8 mapped, 0 set aside |
