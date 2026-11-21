# Sandbox policies

Every PM OS bot runs in its own NVIDIA OpenShell sandbox (decision D3). This folder holds what
that sandbox is allowed to do. The format, the rules and how to apply a policy on the Spark are
in [`docs/sandbox-policies.md`](../docs/sandbox-policies.md).

| Path | What | Written by |
| --- | --- | --- |
| `bots/<bot>.yaml` | The bot's OpenShell sandbox policy | `pmos policy generate` |
| `bots/<bot>.sandbox.yaml` | Its launch spec: the only providers to attach and content to upload | `pmos policy generate` |
| `providers/*.yaml` | The two OpenShell provider profiles every bot's credentials come from | `pmos policy generate` |
| `redteam/` | The red-team checklist and the isolation script to run on the Spark | by hand |

## Don't edit the generated files

A bot's policy comes from its definition (`content/bots/<bot>/BOT.md`) and its role
(`content/roles/<role>/ROLE.md`). A personal bot of `pm-assistant` also gets, read-only, the
skill folders of every persona in the bot marketplace's catalog
(`content/personas/<key>/PERSONA.md` and its role); nothing else in its policy depends on the
catalog (`docs/sandbox-policies.md`, "Personas"). To change what a bot can do, change those,
then:

```sh
cargo run -q -p pmos -- policy generate   # writes policies/
cargo run -q -p pmos -- policy check      # what CI runs
```

Commit the content change and the regenerated files together. CI runs `pmos policy check`, which
fails if any file differs from what `generate` writes (`policy-stale`) or breaks a rule. A hand
edit that adds egress to slack.com fails `net-egress`; one that makes content writable fails
`fs-writable`; one that binds another bot's credentials fails `cred-binding`.

## Review rule (POL-2)

**A change to a bot's network policy needs approval from a second platform-owner PM, who isn't
its author.** Platform owners are the active `admin` users in the PM OS user database
(`docs/runbooks/users.md`).

- **What counts as a network change:** any change to `network_policies` in `bots/*.yaml`, or to
  the code that produces it: the hosts, ports, routes, MCP methods and binaries in
  `crates/policy/src/lib.rs`, and `crates/policy/src/generate.rs`. A new bot or a role with new
  tools also changes what `tools/call` allows, so it counts.
- **Who approves:** a platform-owner PM other than the author. If the author is a platform owner,
  that's a second one.
- **A persona** (`content/personas/`) can only add read-only skill folders to the `pm-*`
  policies, never a tool or a host (the lint refuses a persona whose tools exceed
  `pm-assistant`'s), so it is a filesystem change, below.
- **Filesystem, credential and process changes** go through the content repo's usual
  pull-request review, plus a platform owner. They also need the sandbox recreated on the Spark
  (OpenShell applies them only at startup).
- **Widening never goes in by hand.** Change the code or the content, regenerate, and let review
  see the diff in `policies/`.

The repository should enforce this with branch protection requiring a review from the platform
owners on `policies/**` and `crates/policy/**`. That setting lives in GitHub, not here.

## Bot instructions (second review)

**A change to what a marketplace bot is told needs a second platform owner's review, as a
network change does,** although it changes no policy file (the security review of the bot
marketplace, `docs/bot-marketplace-spec.md` §11). These are the files
(`instruction_paths` in `crates/contract/src/personas.rs`):

- `content/master/AGENTS.md`: the team's standing context, which comes first for every bot;
- `content/personas/**`: the catalog (a new bot, a Preview label, Echo's internal-review line);
- `content/roles/<role>/ROLE.md` for every role a persona runs as, the personal role
  `pm-assistant` included (its "Untrusted data" and "Hard limits" sections are copied verbatim
  into every persona role, and `pmos lint` refuses a copy that differs).

What the repository does by itself: `pmos lint` refuses a persona role whose standing sections
differ from the personal role's, an unlisted write, a reserved name and an offered bot that is
neither benchmarked nor a Preview; a content proposal opened from the web app says **"This
changes what a marketplace bot is told"** in its body and lists these files.

What only GitHub can enforce, and **the owner must set** on `PMOS_CONTENT_BRANCH` (the default
branch), in addition to the protection in `docs/policy-admin.md` ("Nobody approves their own
change"):

1. Add this file as `.github/CODEOWNERS` on the default branch, with `@<org>/<team>` replaced by
   a GitHub team (or the handles) of the platform owners, the active `admin` users:

   ```text
   # Bot marketplace instructions: a second platform owner reviews (policies/README.md).
   /content/master/AGENTS.md                 @<org>/<team>
   /content/personas/                        @<org>/<team>
   /content/roles/pm-assistant/ROLE.md       @<org>/<team>
   /content/roles/chief-of-staff/ROLE.md     @<org>/<team>
   /content/roles/growth-pm/ROLE.md          @<org>/<team>
   /content/roles/outbound-pm/ROLE.md        @<org>/<team>
   # Sandbox policies and their generator (POL-2).
   /policies/                                @<org>/<team>
   /crates/policy/                           @<org>/<team>
   ```

   A new persona whose role is not listed above needs its `ROLE.md` line added in the same pull
   request (the lint test `codeowners_lines_cover_every_bot_instruction_file` checks this list
   against the catalog).
2. In the branch protection rule or ruleset: **Require review from Code Owners**, with at
   least **one** approving review; **Dismiss stale pull request approvals when new commits are
   pushed**; **Require approval of the most recent reviewable push**; and no bypass list. GitHub
   never counts the author's own approval, so the reviewer is a second platform owner.

The file isn't committed here because the owners' GitHub handles aren't in the repository
(they are in the user database), and a CODEOWNERS line with no valid owner protects nothing.

## Applying on the Spark

Network rules apply to a running sandbox with
`openshell policy set pmos-<bot> --policy policies/bots/<bot>.yaml --wait`. Filesystem, Landlock
and process changes need the sandbox recreated. After either, run
[`redteam/isolation.sh`](redteam/isolation.sh). Details are in `docs/sandbox-policies.md`.
