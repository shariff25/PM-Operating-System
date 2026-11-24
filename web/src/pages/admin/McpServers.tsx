import { useId } from 'react';
import type { McpServerView, McpTool, McpView } from '../../api';
import { Empty, systemTone } from '../../components/bits';
import { Icon, systemIcon } from '../../components/icons';
import { copy } from '../../copy';

// Policy Admin's read-only view of the MCP servers the tool server reaches
// (docs/mcp-servers-spec.md §5). Admins and Auditors see the same metadata; nothing here changes
// anything: a server or tool is added or approved through a content pull request.

const M = copy.admin.policy.mcp;

/** The systems a deployment can put on an MCP server, in the order the app names them. */
export const MCP_SYSTEMS = ['jira', 'confluence', 'slack', 'docs'] as const;
const SYSTEM_NAMES: Record<string, string> = { jira: 'Jira', confluence: 'Confluence', slack: 'Slack', docs: 'Docs' };
export const systemName = (s: string) => SYSTEM_NAMES[s] ?? s;
const names = (list: readonly string[]) => list.map(systemName).join(', ');

/** A revoked tool: approved once, since changed or gone at its server. */
export const isRevoked = (t: McpTool) => t.state === 'changed' || t.state === 'missing';

export type McpCheck = 'passed' | 'failed' | 'never' | 'changed';

/**
 * What the server's last check says as one state. A failed connection wins over everything; a
 * passing check with a revoked tool is `changed`, never `passed`.
 */
export function mcpCheck(s: McpServerView): McpCheck {
  const outcome = s.last_check.outcome;
  if (outcome === null || outcome === undefined) return 'never';
  if (outcome !== 'ok') return 'failed';
  return s.tools.some(isRevoked) ? 'changed' : 'passed';
}

const CHECK_TONE: Record<McpCheck, string> = { passed: 'pill-good', failed: 'pill-bad', never: 'pill-warn', changed: 'pill-warn' };
const CHECK_PILL: Record<McpCheck, string> = { passed: M.check.pillPassed, failed: M.failedTag, never: M.check.never, changed: M.check.changed };

/** `sha256:` and the first 12 hex characters: enough to compare with a record by eye. */
export function shortDigest(d: string): string {
  const [algo, hex] = d.includes(':') ? (d.split(':', 2) as [string, string]) : ['', d];
  const short = hex.slice(0, 12);
  return algo ? `${algo}:${short}` : short;
}

/** The systems no listed server is serving: they stay on their built-in connectors. */
export function builtInSystems(servers: McpServerView[]): string[] {
  const served = new Set(servers.flatMap((s) => s.serving ?? s.systems));
  return MCP_SYSTEMS.filter((s) => !served.has(s));
}

const when = (at: string) => new Date(at).toLocaleString();

function SystemChips({ systems, label }: { systems: string[]; label: string }) {
  return (
    <ul className="chip-list" aria-label={label}>
      {systems.map((s) => (
        <li key={s} className={`mini-chip mini-chip-${systemTone(s)} sys-${s}`}>{systemName(s)}</li>
      ))}
    </ul>
  );
}

function ToolRow({ tool }: { tool: McpTool }) {
  const revoked = isRevoked(tool);
  return (
    <li className={`mcp-tool ${revoked ? 'mcp-tool-revoked' : ''}`} data-state={tool.state}>
      <div className="mcp-tool-main">
        <span className="mono-val mcp-tool-name">{tool.registry}</span>
        <span className={`tag ${tool.class === 'write' ? 'tag-warn' : ''}`}>{M.classes[tool.class] ?? tool.class}</span>
        {tool.state !== 'approved' && (
          <span className={`pill ${revoked ? 'pill-bad' : 'pill-warn'}`}>
            <span className="pill-dot" aria-hidden="true" />
            {M.toolStates[tool.state] ?? tool.state}
          </span>
        )}
      </div>
      <div className="fact-quiet small mcp-tool-meta">
        <span>{M.upstream(tool.upstream)}</span>
        <span className="mono-val" title={tool.digest}>{M.digest(shortDigest(tool.digest))}</span>
        {tool.approved_by && <span>{M.approvedBy(tool.approved_by)}</span>}
      </div>
    </li>
  );
}

function CheckFact({ server }: { server: McpServerView }) {
  const state = mcpCheck(server);
  const c = server.last_check;
  const C = M.check;
  const sub =
    state === 'failed'
      ? c.checked_at ? C.lastPassed(when(c.checked_at)) : C.neverPassed
      : state === 'never'
        ? null
        : c.checked_at ? C.at(when(c.checked_at)) : null;
  const text = state === 'failed' ? C.failed : state === 'never' ? (server.auth === 'oauth_per_user' ? C.neverPerUser : C.never) : C.passed;
  const tone = state === 'failed' ? 'pill-bad' : state === 'never' ? 'pill-warn' : 'pill-good';
  return (
    <div className="bot-fact bot-fact-wide mcp-check" data-state={state}>
      <dt>{M.facts.checked}</dt>
      <dd>
        <span className={`pill ${tone}`}>
          <span className="pill-dot" aria-hidden="true" />
          {text}
        </span>
        {sub && <span className="fact-quiet small">{sub}</span>}
        {state === 'failed' && c.outcome && <span className="mono-val mcp-why">{c.outcome}</span>}
        {c.server_name && <span className="fact-quiet small">{C.callsItself([c.server_name, c.server_version].filter(Boolean).join(' '))}</span>}
        {(server.refused_requests ?? 0) > 0 && <span className="fact-quiet small">{C.refused(server.refused_requests!)}</span>}
      </dd>
    </div>
  );
}

export function McpServerCard({ server }: { server: McpServerView }) {
  const id = useId();
  const state = mcpCheck(server);
  const serving = server.serving ?? server.systems;
  const notServing = server.serving ? server.systems.filter((s) => !server.serving!.includes(s)) : [];
  const revoked = server.tools.filter(isRevoked);
  const writes = server.tools.some((t) => t.class === 'write');
  const scopes = server.scopes ?? [];
  const flagged = state === 'failed' || revoked.length > 0;
  return (
    <article className={`mcp-card ${flagged ? 'bot-card-flag' : ''}`} aria-labelledby={`${id}-h`} data-check={state}>
      <header className="bot-head">
        <span className="bot-mark" aria-hidden="true"><Icon name={systemIcon(serving[0] ?? '')} size={16} /></span>
        <div className="bot-title">
          <h3 id={`${id}-h`} className="mono-val">{server.id}</h3>
          <span className="bot-glance">{M.glance(serving.length ? names(serving) : M.nothingServed, server.host)}</span>
        </div>
        <span className={`pill ${CHECK_TONE[state]}`}>
          <span className="pill-dot" aria-hidden="true" />
          {CHECK_PILL[state]}
        </span>
      </header>
      {state === 'failed' && (
        <div className="bot-flag" role="note">
          <span className="flag-tag"><Icon name="alert" size={12} weight={2.25} />{M.failedTag}</span>
          <p>{M.failedBody}</p>
        </div>
      )}
      {revoked.length > 0 && (
        <div className="bot-flag" role="note">
          <span className="flag-tag"><Icon name="alert" size={12} weight={2.25} />{M.changedTag}</span>
          <p>{M.changedBody(revoked.length)}</p>
          <ul>
            {revoked.map((t) => <li key={t.registry} className="mono-val">{t.registry}</li>)}
          </ul>
        </div>
      )}
      <dl className="bot-grid">
        <div className="bot-fact">
          <dt>{M.facts.runBy}</dt>
          <dd>
            {M.operators[server.operator] ?? server.operator}
            {M.operatorHints[server.operator] && <span className="fact-quiet small block">{M.operatorHints[server.operator]}</span>}
          </dd>
        </div>
        <div className="bot-fact">
          <dt>{M.facts.host}</dt>
          <dd className="mono-val mcp-host">{server.host}</dd>
        </div>
        <div className="bot-fact">
          <dt>{M.facts.signIn}</dt>
          <dd>{M.auth[server.auth] ?? server.auth}</dd>
        </div>
        <div className="bot-fact">
          <dt>{M.facts.dataClass}</dt>
          <dd>{M.dataClasses[server.data_class] ?? server.data_class}</dd>
        </div>
        <div className="bot-fact bot-fact-wide">
          <dt>{M.facts.serves}</dt>
          <dd>
            {serving.length ? <SystemChips systems={serving} label={`${M.facts.serves}, ${server.id}`} /> : <span className="fact-quiet">{M.nothingServed}</span>}
            {notServing.length > 0 && <span className="fact-quiet small block mcp-sub">{M.notServing(names(notServing))}</span>}
            {server.serving === null && <span className="fact-quiet small block mcp-sub">{M.servingUnknown}</span>}
          </dd>
        </div>
        {scopes.length > 0 && (
          <div className="bot-fact bot-fact-wide">
            <dt>{M.facts.scopes}</dt>
            <dd>
              <ul className="path-list" aria-label={`${M.facts.scopes}, ${server.id}`}>
                {scopes.map((s) => <li key={s} className="mono-val">{s}</li>)}
              </ul>
            </dd>
          </div>
        )}
        <CheckFact server={server} />
        <div className="bot-fact bot-fact-wide">
          <dt>{M.facts.tools}</dt>
          <dd>
            {server.tools.length === 0 ? (
              <span className="fact-quiet">{M.toolsNone}</span>
            ) : (
              <ul className="mcp-tools" aria-label={`${M.facts.tools}, ${server.id}`}>
                {server.tools.map((t) => <ToolRow key={t.registry} tool={t} />)}
              </ul>
            )}
            {writes && (
              <p className="fact-quiet small mcp-write-note">
                <Icon name="lock" size={13} />
                <span>{M.writeNote}</span>
              </p>
            )}
          </dd>
        </div>
      </dl>
      {server.approved_by && (
        <footer className="bot-foot">
          <span className="bot-file">{M.approvedIn(server.approved_by, server.pull ?? '')}</span>
        </footer>
      )}
    </article>
  );
}

/** The MCP servers section of Policy Admin's overview. `mcp` is missing from an older server. */
export function McpSection({ mcp, editable }: { mcp: McpView | undefined; editable: boolean }) {
  const known = mcp?.available === true;
  const servers = known ? mcp.servers : [];
  const builtIn = builtInSystems(servers);
  return (
    <section className="card mcp-section" aria-labelledby="policy-mcp">
      <div className="card-head card-head-sub">
        <h2 id="policy-mcp">
          {M.heading}
          {known && <span className="card-count">{servers.length}</span>}
        </h2>
      </div>
      <div className="card-body mcp-intro">
        <p className="muted small">{M.lede}</p>
        {known && servers.length > 0 && <p className="small mcp-builtin">{builtIn.length ? M.builtIn(names(builtIn)) : M.allOnMcp}</p>}
      </div>
      {!known ? (
        <Empty icon="alert" small>{M.unknown}</Empty>
      ) : servers.length === 0 ? (
        <Empty icon="globe" small>{M.none}</Empty>
      ) : (
        <div className="bot-cards">
          {servers.map((s) => <McpServerCard key={s.id} server={s} />)}
        </div>
      )}
      <div className="card-foot mcp-foot">
        <p className="fact-quiet small"><Icon name="info" size={13} /><span>{M.cannotShow}</span></p>
        <p className="fact-quiet small">
          <Icon name="pullRequest" size={13} />
          <span>
            {M.howToChange}
            {editable && (
              <>
                {' '}
                <a href="#/admin/content">{M.proposeLink}</a>
              </>
            )}
          </span>
        </p>
      </div>
    </section>
  );
}
