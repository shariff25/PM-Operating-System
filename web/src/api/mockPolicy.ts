// Sample data for Admin: policy, content and harness (docs/web-api.md). Fake names only.
// The content tree is generated from a small spec; validation really regenerates each bot's
// allowances from the changed tree and compares them, so widenings in the sample data are real.
import { MOCK_CATALOG } from './mockBots';
import { unifiedDiff } from '../admin/unified';
import { ApiError } from './types';
import type {
  AllowanceChange,
  Allowances,
  ContentChange,
  ContentFile,
  ContentFileText,
  ContentView,
  FileDiff,
  Harness,
  Issue,
  PolicyBot,
  PolicyFile,
  PolicySpec,
  PolicyView,
  Proposal,
  ProposalList,
  ProposalResult,
  Protection,
  ProverNotes,
  ProverRow,
  SandboxDenial,
  SpecPatch,
  Validation,
} from './policyTypes';

export type MockProtection = 'verified' | 'unverified' | 'no_reviews' | 'not_linked';

export interface MockPolicyOptions {
  /** Every proposal (policy and content) is refused with this error code, to show its handling. */
  proposalError?: string;
  /** What branch protection reads as. Default: verified, one review required. */
  protection?: MockProtection;
  /** Proposals are off at the tool server. */
  proposalsOff?: boolean;
}

const MIN = 60_000;
const COMMIT = '5bb6de4c9a0f7d21c3e8b4a6f1d09e7c2b5a8f30';
const BASE_COMMIT = '1111111111111111111111111111111111111111';
const AUDITOR_REDACTED = "A finding on a file your role can't open.";

// ---------- OpenShell's prover, as the host recorded it (spec §A.5) ----------

const ALL_DOMAINS = ['filesystem', 'landlock', 'network_l4', 'network_rest', 'process'];

/** One bot of each kind the screen must tell apart: a current proof, a stale one, a failure. */
const MOCK_PROVER: Record<string, ProverRow> = {
  docs: { state: 'proved', passed: true, status: 'proved', stale: false, stale_reason: null, at: '2026-11-24T09:00:00Z', coverage: ALL_DOMAINS, prover_version: '0.1.9', host_user: 'pmosop' },
  'pm-ana': { state: 'stale', passed: false, status: 'proved', stale: true, stale_reason: 'policy_changed', at: '2026-11-20T09:00:00Z', coverage: ALL_DOMAINS, prover_version: '0.1.9' },
  'pm-ben': { state: 'exceeds', passed: false, status: 'exceeds', stale: false, at: '2026-11-24T09:00:00Z', counterexample: 'network_l4 api.example.com:443', coverage: ALL_DOMAINS, prover_version: '0.1.9' },
  triage: { state: 'not_available', passed: false, status: 'not_available', stale: false, at: '2026-11-24T09:00:00Z', reason_code: 'no_binary', reason: 'no prover binary at the given path' },
};

/** OpenShell's record of what pm-ben's sandbox was refused (the hand edit's destination). */
const MOCK_DENIALS: SandboxDenial[] = [
  { at: '2026-11-24T08:40:00Z', action: 'sandbox.deny', detail: 'attributed', task_id: 'tsk_ben_1', rule: '-', reason: 'no_matching_policy', dest_class: 'public', dest_host: 'api.example.com', dest_port: 443, http_method: null, http_path: null },
  { at: '2026-11-24T08:41:00Z', action: 'sandbox.deny_summary', detail: 'held 12 denies past 100 a minute; attributed', task_id: 'tsk_ben_1', rule: '-', reason: null, dest_class: 'unknown', dest_port: null },
];

const MOCK_PROVER_NOTES: ProverNotes = {
  not_proved: [
    'which paths are readable (the boundary allows reading /; `fs-readable` covers it)',
    'the MCP methods and tool allowlist (`net-tools`)',
    'credential bindings (`cred-binding`, `cred-providers`)',
    'uploads and the launch spec (`fs-uploads`, `spec-*`)',
    'name resolution (the prover does not resolve hostnames)',
    'what a running sandbox enforces',
  ],
  domains: ['filesystem', 'network_l4', 'network_rest', 'process', 'landlock'],
  view: 'proved on the network view (MCP rules removed, which only widens)',
  not_run_here: 'The prover runs on the pull request (CI check `policy-prover`). Not run here.',
};

// ---------- The spec and the metadata the files carry ----------

const SPEC: PolicySpec = {
  bots: {
    docs: { role: 'docs-helper', scope: 'team', owner: 'TODO' },
    'pm-ana': { role: 'pm-assistant', scope: 'personal', owner: 'ana' },
    'pm-ben': { role: 'pm-assistant', scope: 'personal', owner: 'ben' },
    triage: { role: 'triage-helper', scope: 'team', owner: 'priya' },
  },
  roles: {
    'docs-helper': { skills: ['docs'] },
    'pm-assistant': { skills: ['ask', 'brief', 'meeting-notes', 'prd', 'reply'] },
    'triage-helper': { skills: ['triage', 'docs'] },
  },
  skills: {
    ask: { tools: ['docs.search', 'jira.search', 'confluence.search', 'slack.search'] },
    brief: { tools: ['jira.search', 'confluence.search', 'slack.read_thread'] },
    docs: { tools: ['docs.search', 'docs.read'] },
    'meeting-notes': { tools: ['confluence.create_draft'] },
    prd: { tools: ['confluence.search', 'confluence.create_draft', 'jira.search'] },
    reply: { tools: ['slack.read_thread', 'slack.post_reply'] },
    triage: { tools: ['jira.search', 'jira.update_issue'] },
  },
};

const ROLE_META: Record<string, { owner: string; model: string; data: string; title: string; body: string }> = {
  'docs-helper': { owner: 'TODO', model: 'default', data: 'public', title: 'Docs helper', body: 'Answers questions from the docs library, and says where each answer came from.' },
  'pm-assistant': { owner: 'ada', model: 'reasoning', data: 'internal', title: 'PM assistant', body: "Each PM's own assistant: briefs, PRDs, meeting notes and replies, always as drafts the PM decides." },
  'triage-helper': { owner: 'priya', model: 'extraction', data: 'internal', title: 'Triage helper', body: 'Reads new Jira issues, suggests a component and priority, and links the docs it used.' },
};

const SKILL_META: Record<string, { owner: string; version: string; model: string; data: string; description: string }> = {
  ask: { owner: 'ada', version: '0.3.0', model: 'reasoning', data: 'internal', description: 'Answer a question from Jira, Confluence, Slack and the docs.' },
  brief: { owner: 'ada', version: '0.4.1', model: 'reasoning', data: 'internal', description: "The PM's daily brief and follow-ups." },
  docs: { owner: 'TODO', version: '0.1.0', model: 'extraction', data: 'public', description: 'Search and read the docs library.' },
  'meeting-notes': { owner: 'ben', version: '0.2.0', model: 'default', data: 'internal', description: 'Turn a meeting into a Confluence page draft.' },
  prd: { owner: 'ada', version: '0.5.0', model: 'reasoning', data: 'internal', description: 'Draft a PRD from the thread, the roadmap and Jira.' },
  reply: { owner: 'ben', version: '0.2.2', model: 'default', data: 'internal', description: 'Draft a reply in a Slack thread.' },
  triage: { owner: 'priya', version: '0.1.3', model: 'extraction', data: 'internal', description: 'Suggest a component and priority for a new issue.' },
};

const BOT_BLURB: Record<string, string> = {
  docs: 'Answers questions about the product in #ask-docs.',
  'pm-ana': "Ana's own assistant.",
  'pm-ben': "Ben's own assistant.",
  triage: 'Looks at new issues in the PAY and CORE projects.',
};

const GATEWAY = {
  default: { model: 'qwen3.6-35b-a3b', upstream: 'qwen', workarounds: ['strip-think'] },
  extraction: { model: 'nemotron-3-nano-30b-a3b', upstream: 'nemotron', workarounds: [] as string[] },
  reasoning: { model: 'qwen3.6-35b-a3b', upstream: 'qwen', workarounds: ['strip-think'] },
} as Record<string, { model: string; upstream: string; workarounds: string[] }>;

// ---------- Files from the spec ----------

const quote = (s: string) => `"${s}"`;

function botMd(name: string, b: { role: string; scope: string; owner: string }): string {
  return `---\nname: ${name}\nrole: ${b.role}\nscope: ${b.scope}\nowner: ${quote(b.owner)}\nreads: []\n---\n\n# ${name}\n\n${BOT_BLURB[name] ?? 'A bot.'}\n`;
}

function roleMd(name: string, skills: string[]): string {
  const m = ROLE_META[name] ?? { owner: 'TODO', model: 'default', data: 'internal', title: name, body: '' };
  const list = skills.map((s) => `  - ${s}`).join('\n');
  return `---\nname: ${name}\nversion: "0.1.0"\nowner: ${quote(m.owner)}\nmodel: ${m.model}\ndata-class: ${m.data}\nskills:\n${list}\n---\n\n# ${m.title}\n\n${m.body}\n\n## How it works\n\n- Reads only what its skills name.\n- Every write is a draft a person decides.\n`;
}

function skillMd(name: string, tools: string[]): string {
  const m = SKILL_META[name] ?? { owner: 'TODO', version: '0.1.0', model: 'default', data: 'internal', description: name };
  return `---\nname: ${name}\ndescription: ${quote(m.description)}\nmetadata:\n  owner: ${quote(m.owner)}\n  version: ${quote(m.version)}\n  model: ${quote(m.model)}\n  data-class: ${quote(m.data)}\n  tools: ${quote(tools.join(' '))}\n  surfaces: "slack cli"\n  ships-in: "phase-0"\n---\n\n# ${name}\n\n${m.description}\n\n## Steps\n\n1. Read what the request points at.\n2. Search before asking the PM anything.\n3. Write the answer with a source marker for each claim.\n`;
}

const STATIC_FILES: Record<string, string> = {
  'content/README.md': '# Content\n\nRoles, bots, skills, playbooks, templates and the context library the bots read.\n\nEverything here is a proposal away from changing: open a pull request, and someone else reviews it.\n',
  'content/team.yaml': 'members:\n  - handle: ana\n    roles: [pm]\n  - handle: ben\n    roles: [pm, reviewer]\n  - handle: ada\n    roles: [admin]\n  - handle: audrey\n    roles: [auditor]\n',
  'content/tools.yaml': 'tools:\n  docs.search: { system: docs, write: false }\n  docs.read: { system: docs, write: false }\n  jira.search: { system: jira, write: false }\n  jira.update_issue: { system: jira, write: true }\n  confluence.search: { system: confluence, write: false }\n  confluence.create_draft: { system: confluence, write: true }\n  slack.search: { system: slack, write: false }\n  slack.read_thread: { system: slack, write: false }\n  slack.post_reply: { system: slack, write: true }\n',
  'content/master/AGENTS.md': '---\ntitle: "House rules"\nowner: "ada"\nversion: "1.2.0"\n---\n\n# House rules\n\n- Search before you ask.\n- Every write is a draft. A person decides.\n- Say where each fact came from.\n- Keep it short. PMs are busy.\n',
  'content/master/people/ana/AGENTS.md': '---\ntitle: "Ana\'s notes for her bot"\nowner: "ana"\n---\n\n# Ana\n\nPayments PM. Prefers bullet points, hates acronyms.\n',
  'content/master/people/ben/AGENTS.md': '---\ntitle: "Ben\'s notes for his bot"\nowner: "ben"\n---\n\n# Ben\n\nGrowth PM. Wants the numbers first.\n',
  'content/playbooks/launch-checklist.md': '---\ntitle: "Launch checklist"\nowner: "ada"\nversion: "0.3.0"\nlast_reviewed: "2026-10-02"\n---\n\n# Launch checklist\n\n1. The PRD is approved and linked from the epic.\n2. Support has the FAQ a week before.\n3. The feature flag has an owner and an off switch.\n4. The release note is drafted, and a person approved it.\n5. Metrics are on the dashboard before the first user sees it.\n\n## After launch\n\n- Watch the error budget for 48 hours.\n- Write down what surprised you.\n',
  'content/playbooks/incident-review.md': '---\ntitle: "Incident review"\nowner: "ben"\nversion: "0.1.0"\nlast_reviewed: "2026-03-11"\n---\n\n# Incident review\n\nBlameless, within five working days.\n\n- What happened, in order\n- What we will change\n',
  'content/context/glossary.md': '---\ntitle: "Glossary"\nowner: "ada"\nsensitivity: internal\nlast_reviewed: "2026-11-01"\n---\n\n# Glossary\n\n**Grace period**: the days a workspace keeps full access after a failed payment.\n',
  'content/context/products/billing.md': '---\ntitle: "Billing"\nowner: "ana"\nsensitivity: internal\nlast_reviewed: "2026-11-10"\n---\n\n# Billing\n\nPlans, invoices and the dunning flow.\n',
  'content/context/products/roadmap.md': '---\ntitle: "Roadmap"\nowner: "ada"\nsensitivity: restricted\nlast_reviewed: "2026-11-14"\n---\n\n# Roadmap\n\nNot for sharing outside the product team.\n',
  'content/templates/prd.md': '---\ntitle: "PRD"\nowner: "ada"\nversion: "1.0.0"\n---\n\n# {{title}}\n\n## Problem\n\n## Goals\n\n## Non-goals\n\n## Open questions\n',
  'content/templates/one-pager.md': '---\ntitle: "One-pager"\nowner: "TODO"\nversion: "0.1.0"\n---\n\n# {{title}}\n\nWhat, why, who, when.\n',
  'content/reviewers/security.md': '---\ntitle: "Security reviewer"\nowner: "ada"\nversion: "0.2.0"\n---\n\n# Security reviewer\n\nCheck every change that widens what a bot may reach.\n',
  'content/skills/prd/template.md': '# PRD skeleton\n\nThe skill fills these sections in order.\n',
  // The bot marketplace's catalog: one PERSONA.md per bot (the sample's Scout is withdrawn).
  ...Object.fromEntries(
    MOCK_CATALOG.map((c, i) => [
      `content/personas/${c.key}/PERSONA.md`,
      `---\nname: ${c.key}\ndisplay-name: ${c.display_name}\ntitle: ${c.title}\nrole: ${c.always_on ? 'pm-assistant' : c.key === 'chief' ? 'chief-of-staff' : `${c.key}-pm`}\nmascot: ${c.mascot === 'unknown' ? 'nudge' : c.mascot}\nowner: "ada"\nversion: "0.1.0"\noffered: ${c.offered}\n${c.always_on ? 'always-on: true\n' : ''}${c.preview ? 'preview: true\n' : ''}order: ${i * 10}\nsummary: ${c.summary}\n---\n\n# ${c.display_name}\n\nNotes for whoever maintains this bot.\n`,
    ]),
  ),
};

function kindOf(path: string): string {
  if (/^content\/roles\/[^/]+\/ROLE\.md$/.test(path)) return 'role';
  if (/^content\/bots\/[^/]+\/BOT\.md$/.test(path)) return 'bot';
  if (/^content\/personas\/[^/]+\/PERSONA\.md$/.test(path)) return 'persona';
  if (/^content\/skills\/[^/]+\/SKILL\.md$/.test(path)) return 'skill';
  if (path.startsWith('content/skills/')) return 'skill_file';
  if (path.startsWith('content/playbooks/')) return 'playbook';
  if (path.startsWith('content/templates/')) return 'template';
  if (path.startsWith('content/reviewers/')) return 'reviewer';
  if (path.startsWith('content/context/')) return 'context';
  if (path === 'content/master/AGENTS.md') return 'master';
  if (/^content\/master\/people\/[^/]+\/AGENTS\.md$/.test(path)) return 'personal_master';
  if (path === 'content/team.yaml') return 'team';
  if (path === 'content/tools.yaml') return 'tools';
  if (/README\.md$/.test(path)) return 'readme';
  return 'other';
}

function filesOf(spec: PolicySpec): Map<string, string> {
  const out = new Map<string, string>(Object.entries(STATIC_FILES));
  for (const [name, b] of Object.entries(spec.bots)) out.set(`content/bots/${name}/BOT.md`, botMd(name, b));
  for (const [name, r] of Object.entries(spec.roles)) out.set(`content/roles/${name}/ROLE.md`, roleMd(name, r.skills));
  for (const [name, s] of Object.entries(spec.skills)) out.set(`content/skills/${name}/SKILL.md`, skillMd(name, s.tools));
  return new Map([...out].sort(([a], [b]) => (a < b ? -1 : 1)));
}

// ---------- A small front matter reader (enough for the sample files) ----------

interface Parsed {
  fm: Record<string, unknown> | null;
  error: string | null;
  line: number | null;
}

export function parseFrontMatter(text: string): Parsed {
  const lines = text.split('\n');
  if (lines[0] !== '---') return { fm: null, error: 'no front matter', line: null };
  const end = lines.indexOf('---', 1);
  if (end < 0) return { fm: null, error: 'front matter is not closed with ---', line: lines.length };
  const fm: Record<string, unknown> = {};
  let listKey: string | null = null;
  let mapKey: string | null = null;
  const unq = (v: string) => v.trim().replace(/^"(.*)"$/, '$1');
  for (let i = 1; i < end; i++) {
    const raw = lines[i]!;
    if (!raw.trim()) continue;
    const item = /^\s+-\s+(.*)$/.exec(raw);
    if (item && listKey) {
      (fm[listKey] as string[]).push(unq(item[1]!));
      continue;
    }
    const nested = /^\s+([A-Za-z_-]+):\s*(.*)$/.exec(raw);
    if (nested && mapKey) {
      (fm[mapKey] as Record<string, string>)[nested[1]!] = unq(nested[2]!);
      continue;
    }
    const kv = /^([A-Za-z_-]+):\s*(.*)$/.exec(raw);
    if (!kv) return { fm: null, error: `invalid front matter: could not read line ${i + 1}`, line: i + 1 };
    const [, key, value] = kv as unknown as [string, string, string];
    listKey = null;
    mapKey = null;
    if (value === '') {
      // A list or a map follows; peek at the next line.
      if (/^\s+-\s/.test(lines[i + 1] ?? '')) {
        fm[key] = [];
        listKey = key;
      } else {
        fm[key] = {};
        mapKey = key;
      }
    } else if (value.startsWith('[')) {
      if (!value.endsWith(']')) {
        return { fm: null, error: `invalid front matter: did not find expected ',' or ']' at line ${i + 2} column 1, while parsing a flow sequence at line ${i + 1} column ${key.length + 3}`, line: i + 1 };
      }
      fm[key] = value.slice(1, -1).split(',').map((s) => unq(s)).filter(Boolean);
    } else {
      fm[key] = unq(value);
    }
  }
  return { fm, error: null, line: null };
}

/** The spec the tree defines, and the lint errors that keep a file out of it. */
function specFromFiles(files: Map<string, string>): { spec: PolicySpec; errors: Issue[] } {
  const spec: PolicySpec = { bots: {}, roles: {}, skills: {} };
  const errors: Issue[] = [];
  const err = (code: string, path: string, line: number | null, message: string) => errors.push({ severity: 'error', code, path, line, message });
  for (const [path, text] of files) {
    const kind = kindOf(path);
    if (kind !== 'bot' && kind !== 'role' && kind !== 'skill') continue;
    const name = path.split('/')[2]!;
    const p = parseFrontMatter(text);
    if (!p.fm) {
      err(`${kind}-parse`, path, p.line, p.error ?? 'front matter does not parse');
      continue;
    }
    if (kind === 'bot') {
      spec.bots[name] = { role: String(p.fm.role ?? ''), scope: String(p.fm.scope ?? ''), owner: String(p.fm.owner ?? 'TODO') };
    } else if (kind === 'role') {
      spec.roles[name] = { skills: Array.isArray(p.fm.skills) ? (p.fm.skills as string[]) : [] };
    } else {
      const meta = (p.fm.metadata ?? {}) as Record<string, string>;
      spec.skills[name] = { tools: String(meta.tools ?? '').split(/\s+/).filter(Boolean) };
    }
  }
  for (const [name, r] of Object.entries(spec.roles)) {
    for (const s of r.skills) if (!spec.skills[s]) err('role-skill-unknown', `content/roles/${name}/ROLE.md`, null, `skill \`${s}\` is not in skills/`);
  }
  for (const [name, b] of Object.entries(spec.bots)) {
    if (!spec.roles[b.role]) err('bot-role-unknown', `content/bots/${name}/BOT.md`, null, `role \`${b.role}\` is not in roles/`);
    if (b.scope !== 'team' && b.scope !== 'personal') err('bot-scope', `content/bots/${name}/BOT.md`, null, `scope must be team or personal, not \`${b.scope}\``);
  }
  return { spec, errors };
}

// ---------- Allowances, generated from the spec ----------

const SYSTEM_RO = ['/app', '/dev/urandom', '/etc', '/lib', '/proc', '/usr', '/var/log'];
const SYSTEM_RW = ['/dev/null', '/tmp'];
const WORKER = '/usr/local/bin/node';
const MCP_METHODS = ['initialize', 'notifications/initialized', 'tools/list', 'tools/call'];
const MCP_VERSIONS = ['2025-03-26', '2025-06-18', '2025-11-25'];
const RUNTIME_ROUTES = ['POST /v1/worker/claim', 'POST /v1/worker/tasks/*/complete', 'POST /v1/worker/tasks/*/events', 'POST /v1/worker/tasks/*/fail', 'POST /v1/worker/tasks/*/heartbeat', 'POST /v1/worker/tasks/*/trace'];

const mcpName = (tool: string) => tool.replace(/\./g, '_');

function toolsOf(spec: PolicySpec, bot: string): string[] {
  const role = spec.roles[spec.bots[bot]?.role ?? ''];
  const set = new Set<string>();
  for (const s of role?.skills ?? []) for (const t of spec.skills[s]?.tools ?? []) set.add(t);
  return [...set].sort();
}

function mastersOf(spec: PolicySpec, bot: string): string[] {
  const b = spec.bots[bot]!;
  return b.scope === 'personal' && b.owner !== 'TODO' ? ['master/AGENTS.md', `master/people/${b.owner}/AGENTS.md`] : ['master/AGENTS.md'];
}

function allowancesOf(spec: PolicySpec, bot: string): Allowances {
  const b = spec.bots[bot]!;
  const skills = spec.roles[b.role]?.skills ?? [];
  const tools = toolsOf(spec, bot).map(mcpName);
  const masters = mastersOf(spec, bot);
  const content = [...masters.map((m) => `/sandbox/${m}`), '/sandbox/playbooks', '/sandbox/reviewers', ...skills.map((s) => `/sandbox/skills/${s}`), '/sandbox/templates', '/sandbox/tools.yaml'];
  const read_only = [...new Set([...SYSTEM_RO, ...content])].sort();
  const read_write = ['/dev/null', '/sandbox/workspace', '/tmp'];
  const base = { ports: [], pinned: true, enforcement: 'enforce', tls: null, access: null, binaries: [WORKER] };
  const uploads = [...masters.map((m) => `${m} -> /sandbox/${m}`), ...skills.map((s) => `skills/${s} -> /sandbox/skills/${s}`), 'tools.yaml -> /sandbox/tools.yaml'];
  const contentReads = content.filter((p) => !['/sandbox/playbooks', '/sandbox/reviewers', '/sandbox/templates'].includes(p));
  return {
    network: [
      { ...base, rule: 'pmos_gateway', service: 'gateway', host: 'gateway.pmos.internal', port: 8740, allowed_ips: ['10.201.77.11/32'], protocol: 'rest', credential: `pmos-${bot}-gateway`, routes: ['POST /v1/chat/completions'], mcp_methods: [], mcp_tools: [], mcp_versions: [] },
      { ...base, rule: 'pmos_runtime', service: 'runtime', host: 'bots.pmos.internal', port: 8750, allowed_ips: ['10.201.77.12/32'], protocol: 'rest', credential: `pmos-${bot}-worker`, routes: RUNTIME_ROUTES, mcp_methods: [], mcp_tools: [], mcp_versions: [] },
      { ...base, rule: 'pmos_toolserver', service: 'toolserver', host: 'toolserver.pmos.internal', port: 8710, allowed_ips: ['10.201.77.10/32'], protocol: 'mcp', credential: null, routes: [], mcp_methods: MCP_METHODS, mcp_tools: tools, mcp_versions: MCP_VERSIONS },
    ],
    network_middlewares: [],
    filesystem: { include_workdir: false, read_only, read_write },
    landlock: 'hard_requirement',
    process: { run_as_user: 'sandbox', run_as_group: 'sandbox' },
    providers: [`pmos-${bot}-gateway`, `pmos-${bot}-worker`],
    uploads,
    summary: [
      `May connect to the model gateway at gateway.pmos.internal:8740 (rest, enforce) from ${WORKER}: POST /v1/chat/completions.`,
      `Uses the credential pmos-${bot}-gateway on gateway.pmos.internal:8740.`,
      `May connect to the bot runtime at bots.pmos.internal:8750 (rest, enforce) from ${WORKER}: ${RUNTIME_ROUTES.length} worker routes.`,
      `Uses the credential pmos-${bot}-worker on bots.pmos.internal:8750.`,
      tools.length ? `May call these tools: ${tools.join(', ')}.` : 'May call no tools.',
      `May read this content: ${contentReads.join(', ')}.`,
      `May write only: ${read_write.join(', ')}.`,
      'Runs as sandbox:sandbox; Landlock hard_requirement.',
    ],
  };
}

/** What a hand edit on pm-ben's checked-in file looks like: a destination PM OS does not know, logged only. */
function handEdited(a: Allowances): Allowances {
  const out = structuredClone(a);
  out.network.push({
    rule: 'extra_egress', service: null, host: 'api.example.com', port: 443, ports: [], allowed_ips: ['203.0.113.0/24'], pinned: false, protocol: null,
    enforcement: 'audit', tls: 'skip', access: null, binaries: [WORKER], credential: null, routes: [], mcp_methods: [], mcp_tools: [], mcp_versions: [],
  });
  out.summary.splice(4, 0, `May connect to api.example.com:443 (not inspected, audit) from ${WORKER}. This destination is not a PM OS service.`);
  return out;
}

function policyYaml(bot: string, a: Allowances): string {
  const lines = [
    '# Generated by `pmos policy generate`. Do not edit by hand.',
    'version: 1',
    'filesystem_policy:',
    `  include_workdir: ${a.filesystem.include_workdir}`,
    '  read_only:',
    ...a.filesystem.read_only.map((p) => `    - ${p}`),
    '  read_write:',
    ...a.filesystem.read_write.map((p) => `    - ${p}`),
    `landlock:\n  compatibility: ${a.landlock}`,
    'network_policies:',
  ];
  for (const n of a.network) {
    lines.push(`  ${n.rule}:`, `    name: ${n.rule}`, '    endpoints:', `      - host: ${n.host}`, `        port: ${n.port}`);
    if (n.allowed_ips.length) lines.push('        allowed_ips:', ...n.allowed_ips.map((ip) => `          - ${ip}`));
    lines.push(`        protocol: ${n.protocol}`, `        enforcement: ${n.enforcement}`);
    if (n.routes.length) lines.push('        rules:', ...n.routes.map((r) => `          - allow: { method: ${r.split(' ')[0]}, path: "${r.split(' ')[1]}" }`));
    if (n.mcp_tools.length || n.protocol === 'mcp') {
      lines.push('        mcp:', '          tools:', '            call:', '              any:', ...n.mcp_tools.map((t) => `                - ${t}`));
    }
    lines.push('    binaries:', ...n.binaries.map((b) => `      - path: ${b}`));
  }
  lines.push('process:', `  run_as_group: ${a.process.run_as_group}`, `  run_as_user: ${a.process.run_as_user}`, `# sandbox: pmos-${bot}`, '');
  return lines.join('\n');
}

function fileDiff(path: string, before: string | null, after: string | null): FileDiff {
  return { path, change: before === null ? 'added' : after === null ? 'deleted' : 'modified', diff: unifiedDiff(path, before, after) };
}

// ---------- Comparing what bots may do ----------

function itemsOf(a: Allowances): Record<string, Set<string>> {
  const tool = a.network.find((n) => n.service === 'toolserver');
  return {
    'network.mcp_tool': new Set(tool?.mcp_tools ?? []),
    'filesystem.read_only': new Set(a.filesystem.read_only),
    upload: new Set(a.uploads ?? []),
    'credential.provider': new Set(a.providers ?? []),
  };
}

const DESCRIBE: Record<string, (bot: string, item: string, wider: boolean) => string> = {
  'network.mcp_tool': (bot, item, w) => (w ? `${bot} may now call the tool ${item}` : `${bot} may no longer call the tool ${item}`),
  'filesystem.read_only': (bot, item, w) => (w ? `${bot} may now read ${item}` : `${bot} may no longer read ${item}`),
  upload: (bot, item, w) => (w ? `${bot} now gets ${item.split(' -> ')[0]} uploaded into its sandbox` : `${bot} no longer gets ${item.split(' -> ')[0]} uploaded`),
  'credential.provider': (bot, item, w) => (w ? `${bot} now has the credential ${item}` : `${bot} no longer has the credential ${item}`),
};

function allowanceDiff(before: PolicySpec, after: PolicySpec): AllowanceChange[] {
  const out: AllowanceChange[] = [];
  const bots = [...new Set([...Object.keys(before.bots), ...Object.keys(after.bots)])].sort();
  for (const bot of bots) {
    const a = before.bots[bot] ? itemsOf(allowancesOf(before, bot)) : null;
    const b = after.bots[bot] ? itemsOf(allowancesOf(after, bot)) : null;
    for (const kind of Object.keys(DESCRIBE)) {
      const was = a?.[kind] ?? new Set<string>();
      const now = b?.[kind] ?? new Set<string>();
      for (const item of now) if (!was.has(item)) out.push({ bot, kind, item, before: null, after: item, direction: 'wider', widening: true, description: DESCRIBE[kind]!(bot, item, true) });
      for (const item of was) if (!now.has(item)) out.push({ bot, kind, item, before: item, after: null, direction: 'narrower', widening: false, description: DESCRIBE[kind]!(bot, item, false) });
    }
    const sa = before.bots[bot]?.scope;
    const sb = after.bots[bot]?.scope;
    if (sa && sb && sa !== sb) {
      out.push({ bot, kind: 'process', item: '', before: `${sa} bot`, after: `${sb} bot`, direction: 'changed', widening: true, description: `${bot} changes from a ${sa} bot to a ${sb} bot` });
    }
  }
  return out;
}

function policyChanges(before: PolicySpec, after: PolicySpec): FileDiff[] {
  const out: FileDiff[] = [];
  const bots = [...new Set([...Object.keys(before.bots), ...Object.keys(after.bots)])].sort();
  for (const bot of bots) {
    const a = before.bots[bot] ? policyYaml(bot, allowancesOf(before, bot)) : null;
    const b = after.bots[bot] ? policyYaml(bot, allowancesOf(after, bot)) : null;
    if (a !== b) out.push(fileDiff(`policies/bots/${bot}.yaml`, a, b));
  }
  return out;
}

const READS_SYSTEMS = /^(jira|confluence|slack)\./;

function newWarnings(before: PolicySpec, after: PolicySpec, changedPaths: Set<string>): Issue[] {
  const out: Issue[] = [];
  for (const [bot, b] of Object.entries(after.bots)) {
    const path = `content/bots/${bot}/BOT.md`;
    const reads = toolsOf(after, bot).some((t) => READS_SYSTEMS.test(t));
    const readBefore = before.bots[bot] ? toolsOf(before, bot).some((t) => READS_SYSTEMS.test(t)) : false;
    if (b.scope === 'team' && reads && !readBefore) {
      out.push({ severity: 'warning', code: 'bot-reads-empty', path, line: null, message: 'this team bot can read Jira, Confluence or Slack but lists nothing under `reads`, so it will read nothing there' });
    }
    if (b.owner === 'TODO' && changedPaths.has(path)) out.push({ severity: 'warning', code: 'owner-unassigned', path, line: null, message: 'owner is not assigned yet' });
  }
  return out;
}

const EMPTY_SUMMARY = { wider: 0, narrower: 0, changed: 0 };

function validation(errors: Issue[], warnings: Issue[], content: FileDiff[], policy: FileDiff[], changes: AllowanceChange[]): Validation {
  const summary = { ...EMPTY_SUMMARY };
  for (const c of changes) summary[c.direction]++;
  return {
    valid: errors.length === 0,
    errors,
    warnings,
    content_changes: content,
    policy_changes: errors.length ? [] : policy,
    allowance_changes: errors.length ? [] : changes,
    widening: errors.length === 0 && changes.some((c) => c.widening),
    summary: errors.length ? { ...EMPTY_SUMMARY } : summary,
  };
}

// ---------- Spec patches ----------

const HANDLE = /^[a-z][a-z0-9-]{0,31}$/;
const NAME = /^[a-z][a-z0-9-]{0,63}$/;
const TOOL = /^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$/;
const BOT_FIELDS = new Set(['role', 'scope', 'owner']);

function checkPatchShape(patch: unknown): asserts patch is SpecPatch {
  const bad = () => new ApiError(400, 'bad_request', "That request wasn't in a shape I could use.");
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) throw bad();
  for (const [part, value] of Object.entries(patch)) {
    if (!['bots', 'roles', 'skills'].includes(part) || !value || typeof value !== 'object') throw bad();
    for (const v of Object.values(value as Record<string, unknown>)) {
      if (!v || typeof v !== 'object') throw bad();
      for (const f of Object.keys(v)) {
        const ok = part === 'bots' ? BOT_FIELDS.has(f) : part === 'roles' ? f === 'skills' : f === 'tools';
        if (!ok) throw bad();
      }
    }
  }
}

function specErrors(spec: PolicySpec, patch: SpecPatch): Issue[] {
  const out: Issue[] = [];
  const e = (code: string, path: string, message: string) => out.push({ severity: 'error', code, path, line: null, message });
  for (const [bot, p] of Object.entries(patch.bots ?? {})) {
    if (!spec.bots[bot]) { e('spec-unknown-bot', `spec.bots.${bot}`, `there is no bot \`${bot}\` in the content; add it as a content proposal`); continue; }
    if (p.role !== undefined && !spec.roles[p.role]) e('spec-unknown-role', `spec.bots.${bot}.role`, `there is no role \`${p.role}\``);
    if (p.scope !== undefined && p.scope !== 'team' && p.scope !== 'personal') e('spec-bad-value', `spec.bots.${bot}.scope`, 'scope must be `team` or `personal`');
    if (p.owner !== undefined && p.owner !== 'TODO' && !HANDLE.test(p.owner)) e('spec-bad-value', `spec.bots.${bot}.owner`, `\`${p.owner}\` is not a handle (lower case, no @)`);
  }
  for (const [role, p] of Object.entries(patch.roles ?? {})) {
    if (!spec.roles[role]) { e('spec-unknown-role', `spec.roles.${role}`, `there is no role \`${role}\``); continue; }
    const seen = new Set<string>();
    (p.skills ?? []).forEach((s, i) => {
      if (!NAME.test(s)) e('spec-bad-name', `spec.roles.${role}.skills[${i}]`, `\`${s}\` is not a valid skill name`);
      else if (!spec.skills[s]) e('spec-unknown-skill', `spec.roles.${role}.skills[${i}]`, `there is no skill \`${s}\`; add it as a content proposal`);
      if (seen.has(s)) e('spec-duplicate', `spec.roles.${role}.skills[${i}]`, `\`${s}\` is listed twice`);
      seen.add(s);
    });
    if ((p.skills ?? []).length > 100) e('spec-too-long', `spec.roles.${role}.skills`, 'a role lists at most 100 skills');
  }
  for (const [skill, p] of Object.entries(patch.skills ?? {})) {
    if (!spec.skills[skill]) { e('spec-unknown-skill', `spec.skills.${skill}`, `there is no skill \`${skill}\`; add it as a content proposal`); continue; }
    const seen = new Set<string>();
    (p.tools ?? []).forEach((t, i) => {
      if (!TOOL.test(t)) e('spec-bad-name', `spec.skills.${skill}.tools[${i}]`, `\`${t}\` is not a tool name such as docs.search`);
      if (seen.has(t)) e('spec-duplicate', `spec.skills.${skill}.tools[${i}]`, `\`${t}\` is listed twice`);
      seen.add(t);
    });
    if ((p.tools ?? []).length > 100) e('spec-too-long', `spec.skills.${skill}.tools`, 'a skill lists at most 100 tools');
  }
  return out;
}

function applyPatch(spec: PolicySpec, patch: SpecPatch): PolicySpec {
  const out = structuredClone(spec);
  for (const [bot, p] of Object.entries(patch.bots ?? {})) out.bots[bot] = { ...out.bots[bot]!, ...p };
  for (const [role, p] of Object.entries(patch.roles ?? {})) if (p.skills) out.roles[role] = { skills: [...p.skills] };
  for (const [skill, p] of Object.entries(patch.skills ?? {})) if (p.tools) out.skills[skill] = { tools: [...p.tools] };
  return out;
}

// ---------- Paths ----------

const PATH_PART = /^[A-Za-z0-9._-]+$/;
function checkPath(path: string) {
  const parts = path.split('/');
  if (!path || path.length > 200 || parts.length > 8 || parts.some((p) => !p || p === '.' || p === '..' || p.startsWith('.') || !PATH_PART.test(p))) {
    throw new ApiError(400, 'bad_path', 'That path is not a plain path in the content tree.');
  }
  if (parts[0] !== 'content' || path === 'content/team.yaml') throw new ApiError(400, 'path_not_allowed', 'Only files in the content tree can change here, and not the team list.');
}

/** A stand-in for the file's sha256: stable, 64 hex characters. */
function fakeSha(text: string): string {
  let h1 = 0x811c9dc5;
  let h2 = 0x1000193;
  for (let i = 0; i < text.length; i++) {
    h1 = Math.imul(h1 ^ text.charCodeAt(i), 16777619) >>> 0;
    h2 = Math.imul(h2 + text.charCodeAt(i), 2246822519) >>> 0;
  }
  return (h1.toString(16).padStart(8, '0') + h2.toString(16).padStart(8, '0')).repeat(4);
}

const sizeOf = (text: string) => new TextEncoder().encode(text).length;

// ---------- The mock ----------

export interface MockPolicy {
  policy(viewerMayOpenPrivate: boolean): PolicyView;
  validate(patch: unknown): Validation;
  propose(handle: string, input: { spec: unknown; title: string; reason: string }): ProposalResult;
  proposals(kind: 'policy' | 'content'): ProposalList;
  content(viewerMayOpenPrivate: boolean): ContentView;
  file(path: string, viewerMayOpenPrivate: boolean): ContentFileText;
  validateContent(changes: unknown): Validation;
  proposeContent(handle: string, input: { changes: unknown; title: string; reason: string }): ProposalResult;
  harness(): Harness;
}

function isPrivate(path: string, text: string | undefined): boolean {
  return kindOf(path) === 'personal_master' || /\nsensitivity: restricted\n/.test(text ?? '');
}

export function createMockPolicy(now: number, options: MockPolicyOptions): MockPolicy {
  const spec = structuredClone(SPEC);
  const files = filesOf(spec);
  const prot = options.protection ?? 'verified';
  const configured = !options.proposalsOff;
  const linked = prot !== 'not_linked';
  let nextNumber = 42;
  const lists: Record<'policy' | 'content', Proposal[]> = {
    policy: [
      {
        number: 41, url: 'https://github.com/acme/pmos/pull/41', title: 'PM OS policy: Let the triage bot update Jira issues',
        branch: 'pmos/proposal/policy/ada-20261115-142210-9be01f2a', base: 'main', status: 'open', proposed_by: 'ada', base_commit: BASE_COMMIT,
        author: 'ada-gh', labels: ['pmos-proposal', 'pmos-proposal:policy'],
        created_at: new Date(now - 2 * 24 * 60 * MIN).toISOString(), updated_at: new Date(now - 26 * 60 * MIN).toISOString(),
      },
    ],
    content: [
      {
        number: 39, url: 'https://github.com/acme/pmos/pull/39', title: 'PM OS content: Refresh the launch checklist',
        branch: 'pmos/proposal/content/ada-20261117-051140-4c2d7a10', base: 'main', status: 'draft', proposed_by: 'ada', base_commit: BASE_COMMIT,
        author: 'ada-gh', labels: ['pmos-proposal', 'pmos-proposal:content'],
        created_at: new Date(now - 5 * 60 * MIN).toISOString(), updated_at: new Date(now - 5 * 60 * MIN).toISOString(),
      },
      {
        number: 37, url: 'https://github.com/acme/pmos/pull/37', title: 'PM OS content: Give the PRD skill the one-pager template',
        branch: 'pmos/proposal/content/ben-20261112-093002-11aa02bc', base: 'main', status: 'open', proposed_by: null, base_commit: null,
        author: 'pmos-bot', labels: ['pmos-proposal', 'pmos-proposal:content'],
        created_at: new Date(now - 5 * 24 * 60 * MIN).toISOString(), updated_at: new Date(now - 3 * 24 * 60 * MIN).toISOString(),
      },
    ],
  };

  const protection = (): Protection => {
    if (!configured) return { branch: null, required_reviews: null, verified: false, github: null };
    if (prot === 'not_linked') return { branch: 'main', required_reviews: null, verified: false, github: 'not_linked' };
    if (prot === 'unverified') return { branch: 'main', required_reviews: null, verified: false, github: 'linked' };
    if (prot === 'no_reviews') return { branch: 'main', required_reviews: 0, verified: true, github: 'linked' };
    return { branch: 'main', required_reviews: 1, verified: true, github: 'linked' };
  };

  /** The refusals the tool server and GitHub can give a proposal, when the sample data is asked for one. */
  const forced = (kind: 'policy' | 'content', paths: string[]) => {
    if (!configured) throw new ApiError(501, 'not_configured', 'Proposals are off.');
    if (!linked) throw new ApiError(409, 'github_not_linked', 'Link GitHub in PM OS first: in Slack, open the PM OS app, Home tab, Accounts.');
    const code = options.proposalError;
    if (!code) return;
    const first = paths[0] ?? `content/${kind}`;
    switch (code) {
      case 'stale_base':
        throw new ApiError(409, 'stale_base', 'A file changed on the base branch.', { paths: [first], base: { branch: 'main', commit: BASE_COMMIT } });
      case 'policy_mismatch':
        throw new ApiError(422, 'policy_mismatch', 'The tool server generates different policies.', { paths: paths.filter((p) => p.startsWith('policies/')).slice(0, 2) });
      case 'invalid':
        throw new ApiError(422, 'invalid', 'The change does not pass validation.', {
          errors: [{ severity: 'error', code: 'net-tools', path: paths.find((p) => p.startsWith('policies/')) ?? first, line: null, message: 'the tool server refuses a tool that is not in its registry' }],
          warnings: [],
        });
      case 'rate_limited':
        throw new ApiError(429, 'rate_limited', 'Too many proposals this hour.', {}, 1800);
      case 'not_configured':
        throw new ApiError(501, 'not_configured', 'Proposals are off.');
      case 'unknown_outcome':
        throw new ApiError(502, 'unknown_outcome', 'The answer was lost.');
      case 'audit_unavailable':
      case 'upstream_unavailable':
      case 'github_refused':
      case 'github_unavailable':
        throw new ApiError(502, code, 'That did not go through.');
      case 'github_rate_limited':
        throw new ApiError(503, code, 'GitHub is limiting requests.');
      case 'github_not_linked':
        throw new ApiError(409, code, 'Link GitHub first.');
      default:
        throw new ApiError(400, code, 'That did not go through.');
    }
  };

  const checkTitle = (title: string, reason: string) => {
    // eslint-disable-next-line no-control-regex
    const t = title.replace(/[\u0000-\u001f\u007f]/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 120);
    const r = reason.trim().slice(0, 4000);
    if (!t || !r) throw new ApiError(400, 'bad_request', 'A title and a reason are both needed.');
    return t;
  };

  const opened = (kind: 'policy' | 'content', handle: string, title: string, v: Validation): ProposalResult => {
    const number = nextNumber++;
    const stamp = new Date(now).toISOString().replace(/[-:]/g, '').replace('T', '-').slice(0, 15);
    const branch = `pmos/proposal/${kind}/${handle}-${stamp}-${number.toString(16).padStart(8, 'a')}`;
    const url = `https://github.com/acme/pmos/pull/${number}`;
    lists[kind].unshift({
      number, url, title: `PM OS ${kind}: ${title}`, branch, base: 'main', status: 'open', proposed_by: handle, base_commit: BASE_COMMIT,
      author: `${handle}-gh`, labels: ['pmos-proposal', `pmos-proposal:${kind}`], created_at: new Date().toISOString(), updated_at: new Date().toISOString(),
    });
    return {
      number, url, author: `${handle}-gh`, branch, base: { branch: 'main', commit: BASE_COMMIT },
      files: [...v.content_changes.map((c) => c.path), ...v.policy_changes.map((c) => c.path)],
      widening: v.widening, allowance_changes: v.allowance_changes, stale_check: 'passed', labels_added: true,
    };
  };

  const lintOf = (path: string, text: string, mayOpen: boolean): ContentFile['lint'] => {
    const issues: Issue[] = [];
    const p = parseFrontMatter(text);
    const owner = p.fm ? String((p.fm.metadata as Record<string, string> | undefined)?.owner ?? p.fm.owner ?? '') : '';
    if (owner === 'TODO') issues.push({ severity: 'warning', code: 'owner-unassigned', path, line: null, message: 'owner is not assigned yet' });
    const reviewed = p.fm?.last_reviewed;
    if (typeof reviewed === 'string' && Date.parse(reviewed) < now - 180 * 24 * 60 * MIN) {
      issues.push({ severity: 'warning', code: 'review-overdue', path, line: null, message: `last reviewed on ${reviewed}, more than 180 days ago` });
    }
    if (path.endsWith('roadmap.md')) issues.push({ severity: 'warning', code: 'context-unlinked', path, line: 3, message: 'no role or skill reads this file yet' });
    const shown = issues.map((i) => (mayOpen || !isPrivate(path, text) ? i : { ...i, message: AUDITOR_REDACTED }));
    const errors = shown.filter((i) => i.severity === 'error').length;
    const warnings = shown.length - errors;
    return { status: errors ? 'error' : warnings ? 'warning' : 'ok', errors, warnings, issues: shown };
  };

  function contentValidation(next: Map<string, string | null>): Validation {
    const before = files;
    const after = new Map(before);
    for (const [path, text] of next) {
      if (text === null) after.delete(path);
      else after.set(path, text);
    }
    const contentDiffs: FileDiff[] = [];
    for (const [path, text] of next) {
      const was = before.get(path) ?? null;
      if (was !== text) contentDiffs.push(fileDiff(path, was, text));
    }
    if (contentDiffs.length === 0) throw new ApiError(400, 'no_change', 'Every change equals the deployed file.');
    const { spec: newSpec, errors: lintErrors } = specFromFiles(after);
    const changedPaths = new Set(contentDiffs.map((d) => d.path));
    // Errors on every file in the tree; warnings only on changed files, as the server does.
    const errors: Issue[] = [...lintErrors];
    const warnings: Issue[] = [];
    for (const path of changedPaths) {
      const text = after.get(path);
      if (text === undefined) continue;
      if (path.endsWith('.md') && !['readme', 'other', 'skill_file'].includes(kindOf(path))) {
        const p = parseFrontMatter(text);
        if (!p.fm && !errors.some((x) => x.path === path)) errors.push({ severity: 'error', code: 'front-matter', path, line: p.line, message: p.error ?? 'front matter does not parse' });
        if (p.fm) {
          const owner = String((p.fm.metadata as Record<string, string> | undefined)?.owner ?? p.fm.owner ?? '');
          if (!owner) warnings.push({ severity: 'warning', code: 'owner-missing', path, line: null, message: 'no owner is named' });
          else if (owner === 'TODO') warnings.push({ severity: 'warning', code: 'owner-unassigned', path, line: null, message: 'owner is not assigned yet' });
          if (kindOf(path) === 'playbook' && !p.fm.version) warnings.push({ severity: 'warning', code: 'version-missing', path, line: 2, message: 'no version in the front matter' });
        }
      }
    }
    const changes = errors.length ? [] : allowanceDiff(spec, newSpec);
    const policy = errors.length ? [] : policyChanges(spec, newSpec);
    for (const w of newWarnings(spec, newSpec, new Set())) if (!warnings.some((x) => x.code === w.code && x.path === w.path)) warnings.push(w);
    return validation(errors, warnings, contentDiffs, policy, changes);
  }

  function readChanges(raw: unknown): Map<string, string | null> {
    if (!raw || !Array.isArray(raw) || raw.length === 0) throw new ApiError(400, 'no_change', 'There are no changes.');
    if (raw.length > 50) throw new ApiError(413, 'too_many_files', 'At most 50 files in one change.');
    const out = new Map<string, string | null>();
    for (const c of raw as ContentChange[]) {
      if (!c || typeof c.path !== 'string' || out.has(c.path)) throw new ApiError(400, 'bad_request', "That request wasn't in a shape I could use.");
      checkPath(c.path);
      if (!/\.(md|ya?ml|txt)$/.test(c.path)) throw new ApiError(400, 'not_text', 'Only .md, .yaml, .yml and .txt files can change here.');
      if ('delete' in c) {
        if (!files.has(c.path)) throw new ApiError(400, 'not_found', 'That file is not in the content tree.');
        out.set(c.path, null);
      } else {
        if (typeof c.text !== 'string' || c.text.includes('\u0000')) throw new ApiError(400, 'not_text', 'That text has a NUL byte.');
        if (sizeOf(c.text) > 256 * 1024) throw new ApiError(413, 'too_large', 'A file is over 256 KiB.');
        const current = files.get(c.path);
        if (c.base_sha256 && current !== undefined && c.base_sha256 !== fakeSha(current)) throw new ApiError(409, 'stale_base', 'The file changed on the server.', { paths: [c.path] });
        if (current !== c.text) out.set(c.path, c.text);
      }
    }
    if (out.size === 0) throw new ApiError(400, 'no_change', 'Every change equals the deployed file.');
    return out;
  }

  function validatePolicy(raw: unknown): Validation {
    checkPatchShape(raw);
    const errs = specErrors(spec, raw);
    if (errs.length) return validation(errs, [], [], [], []);
    const next = applyPatch(spec, raw);
    const before = filesOf(spec);
    const after = filesOf(next);
    const content: FileDiff[] = [];
    for (const [path, text] of after) if (before.get(path) !== text) content.push(fileDiff(path, before.get(path) ?? null, text));
    return validation([], newWarnings(spec, next, new Set(content.map((c) => c.path))), content, policyChanges(spec, next), allowanceDiff(spec, next));
  }

  const bots = (): PolicyBot[] =>
    Object.keys(spec.bots).sort().map((bot) => {
      const b = spec.bots[bot]!;
      const allowances = allowancesOf(spec, bot);
      return {
        bot, role: b.role, scope: b.scope, sandbox: `pmos-${bot}`, skills: [...(spec.roles[b.role]?.skills ?? [])],
        tools: toolsOf(spec, bot).map(mcpName), master: mastersOf(spec, bot),
        policy_file: `policies/bots/${bot}.yaml`, launch_spec_file: `policies/bots/${bot}.sandbox.yaml`,
        allowances_from: 'checked_in', allowances: bot === 'pm-ben' ? handEdited(allowances) : allowances,
        prover: MOCK_PROVER[bot] ?? { state: 'missing', passed: false },
        sandbox_denials: bot === 'pm-ben' ? MOCK_DENIALS : [],
      };
    });

  return {
    policy() {
      const policyFiles: PolicyFile[] = [];
      for (const bot of Object.keys(spec.bots).sort()) {
        const generated = policyYaml(bot, allowancesOf(spec, bot));
        const checkedIn = bot === 'pm-ben' ? policyYaml(bot, handEdited(allowancesOf(spec, bot))) : generated;
        policyFiles.push({ path: `policies/bots/${bot}.sandbox.yaml`, state: 'same', generated: `# launch spec for pmos-${bot}\n`, checked_in: `# launch spec for pmos-${bot}\n` });
        policyFiles.push({ path: `policies/bots/${bot}.yaml`, state: generated === checkedIn ? 'same' : 'differs', generated, checked_in: checkedIn });
      }
      policyFiles.push({ path: 'policies/providers/pmos-gateway.yaml', state: 'same', generated: '# provider\n', checked_in: '# provider\n' });
      policyFiles.push({ path: 'policies/providers/pmos-worker.yaml', state: 'same', generated: '# provider\n', checked_in: '# provider\n' });
      policyFiles.sort((a, b) => (a.path < b.path ? -1 : 1));
      return {
        revision: { commit: COMMIT, source: 'git' },
        spec: structuredClone(spec),
        spec_sources: { bots: 'content/bots/<bot>/BOT.md', roles: 'content/roles/<role>/ROLE.md', skills: 'content/skills/<skill>/SKILL.md' },
        fixed: {
          services: [
            { service: 'toolserver', rule: 'pmos_toolserver', host: 'toolserver.pmos.internal', port: 8710, allowed_ips: ['10.201.77.10/32'], protocol: 'mcp', routes: null, credential_profile: null },
            { service: 'gateway', rule: 'pmos_gateway', host: 'gateway.pmos.internal', port: 8740, allowed_ips: ['10.201.77.11/32'], protocol: 'rest', routes: ['POST /v1/chat/completions'], credential_profile: 'pmos-gateway' },
            { service: 'runtime', rule: 'pmos_runtime', host: 'bots.pmos.internal', port: 8750, allowed_ips: ['10.201.77.12/32'], protocol: 'rest', routes: RUNTIME_ROUTES, credential_profile: 'pmos-worker' },
          ],
          mcp_path: '/mcp', mcp_methods: MCP_METHODS, mcp_versions: MCP_VERSIONS, worker_binary: WORKER, workspace: '/sandbox/workspace',
          content_mount: '/sandbox', shared_content: ['templates', 'reviewers', 'playbooks'], system_read_only: SYSTEM_RO, system_read_write: SYSTEM_RW,
          run_as: 'sandbox', landlock: 'hard_requirement', source: 'crates/policy/src/lib.rs',
        },
        bots: bots(),
        files: policyFiles,
        drift: {
          checked_in_available: true,
          in_sync: false,
          violations: [
            { severity: 'error', code: 'net-egress', path: 'policies/bots/pm-ben.yaml', line: null, message: 'extra_egress allows api.example.com:443, which is not a PM OS service' },
          ],
        },
        generate_errors: [],
        proposals_configured: configured,
        protection: protection(),
        prover_notes: MOCK_PROVER_NOTES,
        sandbox_audit: { available: true, configured: true, receiving: true, silent_since: null, last_gap_at: null },
      };
    },

    validate: validatePolicy,

    propose(handle, input) {
      const title = checkTitle(input.title, input.reason);
      const v = validatePolicy(input.spec);
      if (!v.valid) throw new ApiError(422, 'invalid', 'The change does not pass validation.', { errors: v.errors, warnings: v.warnings });
      if (v.content_changes.length === 0) throw new ApiError(400, 'no_change', 'The change changes nothing.');
      forced('policy', [...v.content_changes.map((c) => c.path), ...v.policy_changes.map((c) => c.path)]);
      return opened('policy', handle, title, v);
    },

    proposals(kind) {
      if (!configured) throw new ApiError(501, 'not_configured', 'Proposals are off.');
      if (!linked) {
        return { repo: 'acme/pmos', github: 'not_linked', message: 'Link your GitHub account to see proposals: in Slack, open the PM OS app, then Home, then Accounts.', proposals: [] };
      }
      return { repo: 'acme/pmos', github: 'linked', message: null, proposals: structuredClone(lists[kind]) };
    },

    content(mayOpen) {
      const list: ContentFile[] = [];
      for (const [path, text] of files) {
        const p = parseFrontMatter(text);
        const fm = p.fm ?? {};
        const meta = (fm.metadata ?? {}) as Record<string, string>;
        const str = (v: unknown) => (typeof v === 'string' && v ? v : null);
        const kind = kindOf(path);
        list.push({
          path, kind,
          name: str(fm.name), title: str(fm.title), version: str(meta.version ?? fm.version), owner: str(meta.owner ?? fm.owner),
          sensitivity: str(fm.sensitivity), last_reviewed: str(fm.last_reviewed), size: sizeOf(text),
          editable: /\.(md|ya?ml|txt)$/.test(path) && path !== 'content/team.yaml',
          readable: mayOpen || !isPrivate(path, text),
          lint: lintOf(path, text, mayOpen),
        });
      }
      list.push({
        path: 'content/skills/prd/diagram.png', kind: 'skill_file', name: null, title: null, version: null, owner: null, sensitivity: null, last_reviewed: null,
        size: 48_211, editable: false, readable: true, lint: { status: 'ok', errors: 0, warnings: 0, issues: [] },
      });
      list.sort((a, b) => (a.path < b.path ? -1 : 1));
      const warnings = list.reduce((n, f) => n + f.lint.warnings, 0);
      return {
        revision: { commit: COMMIT, source: 'git' },
        root: 'content',
        lint: { errors: 0, warnings, files_checked: list.length, checked_at: new Date(now - 2 * MIN).toISOString(), other: [] },
        files: list,
        roles: Object.entries(spec.roles).map(([name, r]) => ({
          name, version: '0.1.0', owner: ROLE_META[name]?.owner ?? 'TODO', model: ROLE_META[name]?.model ?? 'default', data_class: ROLE_META[name]?.data ?? null,
          skills: [...r.skills], bots: Object.entries(spec.bots).filter(([, b]) => b.role === name).map(([n]) => n),
        })),
        skills: Object.entries(spec.skills).map(([name, s]) => ({
          name, version: SKILL_META[name]?.version ?? null, owner: SKILL_META[name]?.owner ?? null, model: SKILL_META[name]?.model ?? null, data_class: SKILL_META[name]?.data ?? null,
          tools: [...s.tools], used_by: Object.entries(spec.roles).filter(([, r]) => r.skills.includes(name)).map(([n]) => n),
        })),
        bots: Object.entries(spec.bots).map(([name, b]) => ({ name, ...b })),
        proposals_configured: configured,
        protection: protection(),
      };
    },

    file(path, mayOpen) {
      checkPath(path.startsWith('content/team.yaml') ? 'content/x' : path);
      const text = files.get(path);
      if (text === undefined) throw new ApiError(404, 'not_found', 'There is no such file.');
      if (!mayOpen && isPrivate(path, text)) throw new ApiError(403, 'forbidden', 'Your role does not allow this.');
      const p = path.endsWith('.md') ? parseFrontMatter(text) : { fm: null, error: null, line: null };
      return {
        path, kind: kindOf(path), size: sizeOf(text), sha256: fakeSha(text), text,
        front_matter: p.fm, front_matter_error: path.endsWith('.md') && !p.fm ? p.error : null,
        editable: path !== 'content/team.yaml',
      };
    },

    validateContent(raw) {
      return contentValidation(readChanges(raw));
    },

    proposeContent(handle, input) {
      const title = checkTitle(input.title, input.reason);
      const v = contentValidation(readChanges(input.changes));
      if (!v.valid) throw new ApiError(422, 'invalid', 'The change does not pass validation.', { errors: v.errors, warnings: v.warnings });
      forced('content', [...v.content_changes.map((c) => c.path), ...v.policy_changes.map((c) => c.path)]);
      return opened('content', handle, title, v);
    },

    harness() {
      const loaded = ['docs', 'pm-ana', 'triage'];
      return {
        bots: Object.keys(spec.bots).sort().map((name) => {
          const b = spec.bots[name]!;
          const model = ROLE_META[b.role]?.model ?? null;
          return {
            name, role: b.role, scope: b.scope, owner: b.owner, model_role: model, gateway_alias: model ? `pmos/${model}` : null,
            gateway_model: model ? GATEWAY[model]?.model ?? null : null, skills: [...(spec.roles[b.role]?.skills ?? [])],
            tools: toolsOf(spec, name), loaded_by_runtime: loaded.includes(name),
          };
        }),
        model_roles: Object.entries(GATEWAY).map(([role, g]) => ({ role, alias: `pmos/${role}`, model: g.model, upstream: g.upstream, workarounds: [...g.workarounds] })),
        gateway_config: 'ok',
        runtime: { reachable: true, bots: loaded },
        content_revision: { commit: COMMIT, source: 'git' },
      };
    },
  };
}
