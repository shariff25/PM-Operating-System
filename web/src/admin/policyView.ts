import type { Allowances, NetworkAllowance } from '../api';
import { copy } from '../copy';

const B = copy.admin.policy.bots;

/**
 * Everything in a bot's checked-in policy that PM OS would never generate: a destination it does
 * not know, extra ports or IP ranges, a service not pinned to its own address, logging instead of
 * enforcing, TLS passed through, an access preset, a middleware, or a writable working directory.
 * Each is a hand edit to look at.
 */
export function handEdits(a: Allowances): string[] {
  const out: string[] = [];
  for (const n of a.network) {
    const where = `${n.host ?? '?'}:${n.port ?? '?'}`;
    if (n.service === null) out.push(B.unknownDest(where));
    if (n.ports.length) out.push(`${where}: ${B.extraPorts(n.ports.join(', '))}`);
    if (n.service !== null && !n.pinned) out.push(`${where}: ${B.notPinned(n.allowed_ips.join(', '))}`);
    else if (n.service === null && n.allowed_ips.length) out.push(`${where}: ${B.ipRanges(n.allowed_ips.join(', '))}`);
    if (n.enforcement !== 'enforce') out.push(`${where}: ${B.logsOnly}`);
    if (n.tls !== null) out.push(`${where}: ${B.tlsSkip}`);
    if (n.access !== null) out.push(`${where}: ${B.accessPreset(n.access)}`);
  }
  for (const m of a.network_middlewares) out.push(B.middleware(m));
  if (a.filesystem.include_workdir) out.push(B.workdir);
  return out;
}

const SANDBOX = '/sandbox/';

/** Content paths a bot reads, without the `/sandbox/` prefix, and how many system paths besides. */
export function readsOf(a: Allowances): { content: string[]; system: number } {
  const content = a.filesystem.read_only.filter((p) => p.startsWith(SANDBOX)).map((p) => p.slice(SANDBOX.length));
  return { content, system: a.filesystem.read_only.length - content.length };
}

/** A destination as one line: which service, where, and how it is inspected. */
export function destinationOf(n: NetworkAllowance): { name: string; where: string; protocol: string; unknown: boolean } {
  return {
    name: n.service ? (B.services[n.service] ?? n.service) : n.rule,
    where: `${n.host ?? '?'}:${n.port ?? '?'}`,
    protocol: n.protocol ?? B.protocolNone,
    unknown: n.service === null,
  };
}

/** The system a tool belongs to, from its MCP name (`jira_search` is Jira). */
export function toolSystem(tool: string): string {
  return tool.split(/[._]/)[0] ?? tool;
}
