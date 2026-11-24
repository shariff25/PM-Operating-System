// Admin: policy, content and harness. Types match docs/web-api.md exactly.

/** A finding from the content lint, a policy rule or a spec check. */
export interface Issue {
  severity: 'error' | 'warning';
  code: string;
  /** A repository path, or a spec path such as `spec.bots.docs.scope`. */
  path: string;
  line: number | null;
  message: string;
}

export interface Revision {
  commit: string | null;
  source: 'config' | 'git' | null;
}

/** Whether the default branch makes GitHub require someone else's review. Best effort. */
export interface Protection {
  branch: string | null;
  required_reviews: number | null;
  /** True only when reviews are required and GitHub showed admins bound and nobody able to bypass (or none are required). */
  verified: boolean;
  /** Whether the rule binds admins too; null when GitHub's answer didn't say. Missing from an older server. */
  admins_enforced?: boolean | null;
  /** Whether anyone may merge without the review; null when GitHub's answer didn't say. Missing from an older server. */
  bypass_allowed?: boolean | null;
  github: 'linked' | 'not_linked' | 'unavailable' | null;
}

export interface SpecBot {
  role: string;
  scope: string;
  owner: string;
}

/** The policy inputs an admin may change: bots, roles and skills. Lists are in file order. */
export interface PolicySpec {
  bots: Record<string, SpecBot>;
  roles: Record<string, { skills: string[] }>;
  skills: Record<string, { tools: string[] }>;
}

/** Any part of the spec; what is left out does not change. */
export interface SpecPatch {
  bots?: Record<string, Partial<SpecBot>>;
  roles?: Record<string, { skills?: string[] }>;
  skills?: Record<string, { tools?: string[] }>;
}

export interface FixedService {
  service: string;
  rule: string;
  host: string;
  port: number;
  /** The one address the host may resolve to: its own pinned `/32` (decision O6). */
  allowed_ips: string[];
  protocol: string;
  routes: string[] | null;
  credential_profile: string | null;
}

export interface PolicyFixed {
  services: FixedService[];
  mcp_path: string;
  mcp_methods: string[];
  mcp_versions: string[];
  worker_binary: string;
  workspace: string;
  content_mount: string;
  shared_content: string[];
  system_read_only: string[];
  system_read_write: string[];
  run_as: string;
  landlock: string;
  source: string;
}

export interface NetworkAllowance {
  rule: string;
  /** `null` is a destination PM OS does not know: a hand edit. */
  service: 'toolserver' | 'gateway' | 'runtime' | null;
  host: string | null;
  port: number | null;
  ports: number[];
  allowed_ips: string[];
  /** The service is known and `allowed_ips` is exactly its own pinned address, as generated. */
  pinned: boolean;
  protocol: string | null;
  enforcement: string | null;
  tls: string | null;
  access: string | null;
  binaries: string[];
  credential: string | null;
  routes: string[];
  mcp_methods: string[];
  mcp_tools: string[];
  mcp_versions: string[];
}

export interface Allowances {
  network: NetworkAllowance[];
  network_middlewares: string[];
  filesystem: { include_workdir: boolean; read_only: string[]; read_write: string[] };
  landlock: string | null;
  process: { run_as_user: string | null; run_as_group: string | null };
  providers: string[] | null;
  uploads: string[] | null;
  summary: string[];
}

export interface PolicyBot {
  bot: string;
  role: string;
  scope: string;
  sandbox: string;
  skills: string[];
  tools: string[];
  master: string[];
  policy_file: string;
  launch_spec_file: string;
  allowances_from: 'checked_in' | 'generated';
  allowances: Allowances | null;
  /** The hash a host proof must have proved to be current. */
  policy_sha256?: string | null;
  /** OpenShell's prover on this bot's running sandbox (docs/openshell-prover-and-sandbox-audit-spec.md §A.5). */
  prover?: ProverRow;
  /** The sandbox's latest denials OpenShell recorded (spec §B.5), newest first; untrusted text. */
  sandbox_denials?: SandboxDenial[];
}

export interface SandboxDenial {
  at: string;
  action: string;
  detail: string | null;
  task_id: string | null;
  rule: string | null;
  reason: string | null;
  /** What an auditor sees instead of the host and path. */
  dest_class: string | null;
  dest_host?: string | null;
  dest_port: number | null;
  http_method?: string | null;
  http_path?: string | null;
}

export interface SandboxAuditState {
  available: boolean;
  configured?: boolean;
  receiving?: boolean;
  silent_since?: string | null;
  last_gap_at?: string | null;
}

/**
 * What the screen shows for one bot's host proof. Only `proved` passes; `stale`, `missing` and
 * `unknown` are never shown as proved.
 */
export type ProverState =
  | 'proved'
  | 'stale'
  | 'exceeds'
  | 'unsupported'
  | 'inconclusive'
  | 'error'
  | 'not_available'
  | 'version_mismatch'
  | 'missing'
  | 'unknown';

export interface ProverRow {
  state: ProverState;
  passed: boolean;
  status?: string;
  stale?: boolean;
  stale_reason?: 'policy_changed' | 'policy_loaded_after' | null;
  at?: string | null;
  reason_code?: string | null;
  reason?: string | null;
  counterexample?: string | null;
  coverage?: string[] | null;
  prover_version?: string | null;
  host_user?: string | null;
}

export interface ProverNotes {
  not_proved: string[];
  domains: string[];
  view: string;
  not_run_here: string;
}

/** One approved tool of an MCP server, and what its last `tools/list` found for it. */
export interface McpTool {
  /** PM OS's own registry name, such as `jira.search`. */
  registry: string;
  /** The server's own name for it (its words, shown as text). */
  upstream: string;
  class: 'read' | 'write';
  /** `sha256:<hex>` of the approved definition. */
  digest: string;
  /** `changed` and `missing` are revoked: unusable until an Admin approves the new definition. */
  state: 'approved' | 'changed' | 'missing' | 'unchecked';
  approved_by: string | null;
}

/** One MCP server the tool server reaches (docs/mcp-servers-spec.md §5). Metadata only. */
export interface McpServerView {
  id: string;
  /** `pmos`: PM OS runs it (self-hosted or built by PM OS); `system_vendor`: the vendor's hosted server. */
  operator: 'pmos' | 'system_vendor' | 'third_party';
  host: string;
  hosts: string[] | null;
  auth: 'oauth_per_user' | 'service_token' | 'none';
  scopes: string[] | null;
  data_class: 'public' | 'internal' | 'restricted';
  /** Every system its record lists. */
  systems: string[];
  /** The systems this tool server routes to it; `null` from an older tool server. */
  serving: string[] | null;
  egress: string[] | null;
  approved_by: string | null;
  pull: string | null;
  refused_requests: number | null;
  tools: McpTool[];
  last_check: {
    /** The last check that passed; a failure after it keeps this time. */
    checked_at: string | null;
    /** `ok`, or why the last connection failed (text, not trusted). `null`: never checked. */
    outcome: string | null;
    server_name: string | null;
    server_version: string | null;
  };
}

export interface McpView {
  /** `false`: the tool server didn't answer. */
  available: boolean;
  servers: McpServerView[];
}

export interface PolicyFile {
  path: string;
  state: 'same' | 'differs' | 'missing' | 'orphan' | 'unknown';
  generated: string | null;
  checked_in: string | null;
}

export interface PolicyView {
  revision: Revision;
  spec: PolicySpec;
  spec_sources: { bots: string; roles: string; skills: string };
  fixed: PolicyFixed;
  bots: PolicyBot[];
  files: PolicyFile[];
  drift: { checked_in_available: boolean; in_sync: boolean | null; violations: Issue[] };
  generate_errors: Issue[];
  proposals_configured: boolean;
  protection: Protection;
  /** The prover's fixed notes: what a proof never covers, and that it doesn't run here. */
  prover_notes?: ProverNotes;
  /** The sandbox-audit collector's state, for the denials list. */
  sandbox_audit?: SandboxAuditState;
  /** The MCP servers the tool server reaches; missing from an older server. */
  mcp?: McpView;
}

export interface FileDiff {
  path: string;
  change: 'added' | 'modified' | 'deleted';
  /** A unified diff with three lines of context. */
  diff: string;
}

export interface AllowanceChange {
  bot: string;
  kind: string;
  item: string;
  before: string | null;
  after: string | null;
  direction: 'wider' | 'narrower' | 'changed';
  widening: boolean;
  description: string;
}

/** The answer of both validate routes. */
export interface Validation {
  valid: boolean;
  errors: Issue[];
  warnings: Issue[];
  content_changes: FileDiff[];
  policy_changes: FileDiff[];
  allowance_changes: AllowanceChange[];
  widening: boolean;
  summary: { wider: number; narrower: number; changed: number };
}

export interface ProposalResult {
  number: number;
  url: string;
  author: string | null;
  branch: string;
  base: { branch: string; commit: string };
  files: string[];
  widening: boolean;
  allowance_changes: AllowanceChange[];
  stale_check: 'passed' | 'skipped';
  labels_added: boolean;
}

export interface Proposal {
  number: number;
  url: string;
  title: string;
  branch: string;
  base: string;
  status: string;
  proposed_by: string | null;
  base_commit: string | null;
  author: string | null;
  labels: string[];
  created_at: string | null;
  updated_at: string | null;
}

export interface ProposalList {
  repo: string | null;
  github: 'linked' | 'not_linked';
  message: string | null;
  proposals: Proposal[];
}

export interface ContentLint {
  status: 'ok' | 'warning' | 'error';
  errors: number;
  warnings: number;
  issues: Issue[];
}

export interface ContentFile {
  path: string;
  kind: string;
  name: string | null;
  title: string | null;
  version: string | null;
  owner: string | null;
  sensitivity: string | null;
  last_reviewed: string | null;
  size: number;
  editable: boolean;
  readable: boolean;
  lint: ContentLint;
}

export interface ContentRole {
  name: string;
  version: string | null;
  owner: string | null;
  model: string | null;
  data_class: string | null;
  skills: string[];
  bots: string[];
}

export interface ContentSkill {
  name: string;
  version: string | null;
  owner: string | null;
  model: string | null;
  data_class: string | null;
  tools: string[];
  used_by: string[];
}

export interface ContentView {
  revision: Revision;
  root: string;
  lint: { errors: number; warnings: number; files_checked: number; checked_at: string; other: Issue[] };
  files: ContentFile[];
  roles: ContentRole[];
  skills: ContentSkill[];
  bots: { name: string; role: string; scope: string; owner: string }[];
  proposals_configured: boolean;
  protection: Protection;
}

export interface ContentFileText {
  path: string;
  kind: string;
  size: number;
  sha256: string;
  text: string;
  front_matter: Record<string, unknown> | null;
  front_matter_error: string | null;
  editable: boolean;
}

/** One file in a content proposal: its whole new text, or a delete. */
export type ContentChange = { path: string; text: string; base_sha256?: string } | { path: string; delete: true };

export interface HarnessBot {
  name: string;
  role: string;
  scope: string;
  owner: string;
  model_role: string | null;
  gateway_alias: string | null;
  gateway_model: string | null;
  skills: string[];
  tools: string[];
  loaded_by_runtime: boolean | null;
}

export interface Harness {
  bots: HarnessBot[];
  model_roles: { role: string; alias: string; model: string; upstream: string; workarounds: string[] }[] | null;
  gateway_config: 'ok' | 'not_configured' | 'unreadable';
  runtime: { reachable: boolean; bots: string[] | null };
  content_revision: Revision;
}
