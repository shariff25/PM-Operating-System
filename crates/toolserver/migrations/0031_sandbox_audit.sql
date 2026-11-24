-- OpenShell's own sandbox decisions in the audit log (docs/openshell-prover-and-sandbox-audit-spec.md
-- §B.3, slice B1). They are rows of audit_events, so the one 90-day purge (migration 0013) is the
-- only thing that deletes them and the append-only triggers refuse UPDATE, DELETE and TRUNCATE as
-- for every other event. Every new column is nullable: other events leave them empty.
-- (Number 0031 on this branch; another branch adds tool-server migrations too, so the conductor
-- may renumber it on merge.)
--
-- Columns beyond the spec's list: `policy_hash` (what a `sandbox.policy_loaded` event loaded, §B.2)
-- and `os_correlation_uid` (OpenShell's metadata.correlation_uid, which §B.3 says joins its own
-- records). Every value is allowlisted metadata the collector and the tool server both cleaned:
-- printable ASCII, length-capped; never a command line, message, query string, header or body.

ALTER TABLE audit_events
    ADD COLUMN sandbox            text   CHECK (char_length(sandbox) <= 64),
    ADD COLUMN sandbox_uid        text   CHECK (char_length(sandbox_uid) <= 128),
    ADD COLUMN decision           text   CHECK (decision IN ('allow', 'deny', 'finding')),
    ADD COLUMN rule               text   CHECK (char_length(rule) <= 128),
    ADD COLUMN reason             text   CHECK (reason IN ('no_matching_policy', 'always_blocked',
                                             'not_in_allowed_ips', 'l7_deny', 'cross_sandbox',
                                             'finding', 'other')),
    ADD COLUMN dest_host          text   CHECK (char_length(dest_host) <= 253),
    ADD COLUMN dest_ip            text   CHECK (char_length(dest_ip) <= 45),
    ADD COLUMN dest_port          integer CHECK (dest_port BETWEEN 0 AND 65535),
    ADD COLUMN http_method        text   CHECK (char_length(http_method) <= 16),
    ADD COLUMN http_path          text   CHECK (char_length(http_path) <= 256
                                             AND position('?' IN http_path) = 0
                                             AND position('#' IN http_path) = 0),
    ADD COLUMN process            text   CHECK (char_length(process) <= 128),
    ADD COLUMN allow_count        bigint CHECK (allow_count >= 0),
    ADD COLUMN os_event_uid       text   CHECK (char_length(os_event_uid) <= 128),
    ADD COLUMN os_correlation_uid text   CHECK (char_length(os_correlation_uid) <= 128),
    ADD COLUMN os_version         text   CHECK (char_length(os_version) <= 64),
    ADD COLUMN policy_hash        text   CHECK (char_length(policy_hash) <= 128),
    ADD COLUMN collector_run      text   CHECK (collector_run ~ '^[A-Za-z0-9_-]{8,64}$'),
    ADD COLUMN collector_seq      bigint CHECK (collector_seq >= 1);

-- The collector records as its own client (`sandbox_audit`); nothing else may write a sandbox row.
ALTER TABLE audit_events DROP CONSTRAINT IF EXISTS audit_events_client_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_client_check
    CHECK (client IN ('mcp', 'orchestrator', 'bot', 'web', 'console', 'system', 'host',
                      'sandbox_audit'));
ALTER TABLE audit_events ADD CONSTRAINT audit_events_sandbox_client
    CHECK ((tool LIKE 'sandbox.%') = (client = 'sandbox_audit'));

-- A deny or finding OpenShell delivers twice (its export is at least once) is stored once.
CREATE UNIQUE INDEX audit_events_sandbox_dedup ON audit_events (os_event_uid)
    WHERE os_event_uid IS NOT NULL AND tool IN ('sandbox.deny', 'sandbox.finding');

-- The overview, Policy Admin's latest denials and the tripwire: by bot and time.
CREATE INDEX audit_events_sandbox ON audit_events (bot, at DESC, id DESC) WHERE client = 'sandbox_audit';
CREATE INDEX audit_events_sandbox_at ON audit_events (at DESC, id DESC) WHERE client = 'sandbox_audit';

-- Each start of the collector (`collector_run`, random per start): the last sequence number stored,
-- when it was last heard from, and an open `collector_down` (silent for 5 minutes). Mutable state,
-- not evidence: what it detects is recorded as audit events.
CREATE TABLE sandbox_collector_runs (
    collector_run   text PRIMARY KEY CHECK (collector_run ~ '^[A-Za-z0-9_-]{8,64}$'),
    first_at        timestamptz NOT NULL,
    last_seen_at    timestamptz NOT NULL,
    last_seq        bigint NOT NULL DEFAULT 0 CHECK (last_seq >= 0),
    down_open_since timestamptz,
    superseded      boolean NOT NULL DEFAULT false
);
CREATE INDEX sandbox_collector_runs_seen ON sandbox_collector_runs (last_seen_at DESC);
