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
(`content/roles/<role>/ROLE.md`). To change what a bot can do, change those, then:

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
- **Filesystem, credential and process changes** go through the content repo's usual
  pull-request review, plus a platform owner. They also need the sandbox recreated on the Spark
  (OpenShell applies them only at startup).
- **Widening never goes in by hand.** Change the code or the content, regenerate, and let review
  see the diff in `policies/`.

The repository should enforce this with branch protection requiring a review from the platform
owners on `policies/**` and `crates/policy/**`. That setting lives in GitHub, not here.

## Applying on the Spark

Network rules apply to a running sandbox with
`openshell policy set pmos-<bot> --policy policies/bots/<bot>.yaml --wait`. Filesystem, Landlock
and process changes need the sandbox recreated. After either, run
[`redteam/isolation.sh`](redteam/isolation.sh). Details are in `docs/sandbox-policies.md`.
