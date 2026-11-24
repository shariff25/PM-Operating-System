# Provider records for the approved hosted model

The owner-only rules for the record an entry of `crates/gateway/approved-hosted.yaml` pins
(`docs/openshell-any-host-spec.md` §3.6, "The provider record lives outside the repository", and
§3.9, conditions 1 to 3). No provider is named here, and no record is kept in this repository:
a record names the vendor, its endpoint and its terms, and no committed file may identify the
vendor (the owner's decision of 2026-11-23).

## Where the record lives

In a store the owner controls and this repository doesn't reach. **Default (the spec's
recommendation, until the owner decides otherwise):** a private repository owned by the owner's
own account, separate from PM OS's, so the record has a dated history and the owner shares read
access by person. Who may read it: the owner; the reviewer session for the H13 review, through
read access the owner gives for it; anyone else only if the owner names them.

## What a record holds

1. The owner's approval of the provider and the model: who approved, when, the provider, the base
   URL and the model ids in full, and for which data (all classes, as decision D1 says, unless the
   owner narrows it; a narrower class is the entry's `max_data_class`). This is a step of its own,
   separate from D1.
2. Whether the owner **controls** the endpoint (runs or contracts it, and decides who can read it)
   or **trusts it under an agreement** (named, with its date).
3. The provider's retention and training terms, each confirmed by the owner: how long prompts and
   answers are kept, whether they are used for training, and where they are processed. The
   provider keeps them **no longer than PM OS does itself**: Slack text no longer than 24 hours,
   other prompt text no longer than 30 days, nothing longer than 90 days. If the terms are longer,
   no entry is added.
4. Whether the host's outbound proxy re-signs TLS (then the entry says `tls_interception:
   allowed`, and the record says the owner accepts the proxy into the data path).
5. Either the owner's approval of the amended wording of the team's platform decision, or the
   owner's acceptance that its old text stays while real data goes to the hosted model (§3.9,
   condition 8).

Never in a record: a key or any other secret.

## How the entry ties to it

- `record_sha256`: the SHA-256 of the record file's exact bytes (`deploy/pmos-admin hosted digest
  --record <file>`). The record is final before the pull request that adds the entry is opened;
  a later change is a new digest, and so a new reviewed change to the list.
- `url_sha256` and `model_sha256`: `deploy/pmos-admin hosted digest` and `hosted digest --model`,
  each reading one value from standard input (never an argument), normalised exactly as the
  gateway does.
- `pull`: the number of the pull request that adds the entry; the owner's approving review on it
  is checked at H13.

## Who may change the list and this folder

The owner alone: the owner's handle is recorded in `.github/owner-handle`, and `.github/CODEOWNERS`
gives `/.github/CODEOWNERS`, `/.github/owner-handle`, `/crates/gateway/approved-hosted.yaml` and
`/docs/model-providers/` to that handle only, after every other line that could match them
(`policies/README.md`, "Bot instructions (second review)"). Neither file exists yet: they wait on
the owner, and until they do the list's own test fails on any entry.
