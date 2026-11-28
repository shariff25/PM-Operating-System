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

## The two rules every review is held to

**Outward.** A tool is outward when a call to it does any of these:

1. puts free text or a file the agent chose where people will see it or be sent it (a
   comment, a description, a message, a title);
2. tells a person something (an assignment, a watcher added, a review asked for);
3. widens who can see or do something (a public gist, a fork, a ruleset's bypass list);
4. starts something outside the system or under other instructions (a workflow, another
   agent, a customer's request).

A read is never outward. A write that only removes something, or only moves it, is not.
Where one tool can do several things, it is outward if any of them is. A test holds every
write that takes free text to rule 1.

**Put back.** A tool is marked `inverse` or `restore` only when the same server lists the
tool that takes the call back, and the review names it in `undo_with`: the opposite tool
for `inverse`, the tool that writes the old value again for `restore`. A `restore` also
names, in `read_with`, the tool that reads the value as it was before: without one there
is nothing to write back. `build` refuses a review that marks one and names none, names a
tool that is not in the map, names a read to undo with, or names anything but a read to
read with. A server with no such tool gets `none`, however easy the undo is in the system's own
screens: a reversal class is a promise that a gate can undo the action through this
connector. Slack's server has none at all.

## What a map here does and does not claim

- **It was reviewed against the server's listing, not against the live system**, unless its
  `source` says otherwise. None of the three maps here has been run against the real system.
- **It is for one version of one server.** Another version is another listing: run `drift`.
- **It reads a call's arguments; it cannot see what the system does next** (a workflow rule,
  a notification scheme). Where a tool can do several things in one call, it is mapped as
  the widest of them.
- **A thing called by a former name is placed where it no longer is.** A moved Jira issue
  still answers to its old key, and a renamed or transferred GitHub repository to its old
  name; a map reads the project or repository from the name in the call. A grant on
  project PAY therefore reaches an issue that has moved out of it, and a refusal on a
  repository is passed by calling it by a name it used to have. No map can see this.
  Before anything is enforced, a gate must compare the name it was given with the one the
  system answers under, and refuse when they differ (review finding GR-161; not designed
  yet).
- **A target named in a second argument is not in the resource.** `jira_move_issue` (the
  project moved to), `jira_create_issue_link` and `jira_add_issues_to_sprint` (issues of
  other projects), `sub_issue_write`, and `fork_repository` (the account or organization
  written into): a grant on the first-named place is enough for each. A rule on a second
  argument waits for the policy slice.
- **A listing can depend on how the server is set.** Each review's `source` says which
  settings its listing was taken under. A server set otherwise (read-only, fewer toolsets,
  another edition) may list other definitions, and every tool whose definition differs is
  recorded as `unknown`: that fails safe, and makes the map useless until `drift` has been
  run against the real server and the map reviewed again.
- **Some tools name no single resource** (a free-text search, a list of issues). They are
  mapped to a resource of their own (`jira/search`, `jira/batch`), so a grant on one project
  never reaches them; a grant for one of them is a grant across the whole system.
- **A resource is built from string arguments that are plain identifiers.** A number (a
  GitHub issue or pull request) is not part of it, so GitHub calls are held to an area of
  their repository (`code`, `issues`, `pulls`, `actions`, `security`, `discussions`,
  `settings`) and no finer; a name with a character outside an identifier's is recorded as
  `unknown`, not guessed at.
- **A tool with several methods is one tool to a map**, mapped as the widest thing any of
  its methods does.
- **No map here says how confidential anything is.** A review may carry `labels`,
  `clearances` and, on a tool, `results` (see `docs/access-governance-spec.md`, CG-4), and
  `build` passes them into the map; which project is confidential is each deployment's to
  say. Until it does, every resource is taken for restricted, cleared for public only, and
  every tool's results for untrusted. Nothing acts on labels yet.
- Each map is read by the security reviewer before it backs anything but observe mode.
  After the reviewer's first read (2026-11-27) the three were changed as it asked; the
  changed maps are not yet read again, so **none may back shadow or enforce mode**.

## Maps

| Directory | Server | Tools |
|---|---|---|
| `jira/` | `mcp-atlassian` 0.24.8, Jira tools | 63 mapped, 0 set aside |
| `github/` | `github-mcp-server` v1.14.0, every toolset | 91 mapped, 0 set aside |
| `slack/` | `@modelcontextprotocol/server-slack` 2025.4.25 | 8 mapped, 0 set aside |
