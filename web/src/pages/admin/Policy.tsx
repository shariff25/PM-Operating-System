import { useCallback, useId, useMemo, useState } from 'react';
import { canEditPolicy } from '../../admin/access';
import { usePending, useLeaveGuard } from '../../admin/pending';
import { destinationOf, handEdits, readsOf, toolSystem } from '../../admin/policyView';
import { applySpecPatch, parseRawPatch, patchSize, specPatch, splitTools, stableKey } from '../../admin/proposals';
import { unifiedDiff } from '../../admin/unified';
import { useLoad } from '../../admin/useLoad';
import { useProposalFlow } from '../../admin/useProposalFlow';
import type { PolicyBot, PolicySpec, PolicyView, SpecPatch } from '../../api';
import { useApp } from '../../app-context';
import { Empty, ErrorNote, Skeleton, systemTone } from '../../components/bits';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { AdminHead, NoAccess } from './parts';
import {
  CheckResult,
  DiffView,
  IssueList,
  ProblemNote,
  ProposalDone,
  ProposalsCard,
  ProposeBar,
  ProposeDialog,
  ProtectionNotes,
  ProtectionTile,
  RevisionChip,
  StatusTile,
  useCheckProposals,
} from './proposal';

const T = copy.admin.policy;
const B = T.bots;
const PROPOSALS_ID = 'policy-proposals';

// ---------- What each bot may do ----------

function Chips({ items, mono, tone, label }: { items: string[]; mono?: boolean; tone?: boolean; label: string }) {
  return (
    <ul className="chip-list" aria-label={label}>
      {items.map((t) => (
        <li key={t} className={`mini-chip ${mono ? 'mono-val' : ''} ${tone ? `mini-chip-${systemTone(toolSystem(t))} sys-${toolSystem(t)}` : ''}`}>{t}</li>
      ))}
    </ul>
  );
}

function BotCard({ bot, owner }: { bot: PolicyBot; owner: string | null }) {
  const id = useId();
  const a = bot.allowances;
  const edits = a ? handEdits(a) : [];
  const reads = a ? readsOf(a) : null;
  const unassigned = owner === null || owner === 'TODO';
  return (
    <article className={`bot-card ${edits.length ? 'bot-card-flag' : ''}`} aria-labelledby={`${id}-h`}>
      <header className="bot-head">
        <span className="bot-mark" aria-hidden="true"><Icon name="cpu" size={16} /></span>
        <div className="bot-title">
          <h3 id={`${id}-h`} className="mono-val">{bot.bot}</h3>
          {a && <span className="bot-glance">{B.glance(bot.tools.length, a.network.length, a.filesystem.read_write.length)}</span>}
        </div>
        <span className={`pill ${bot.scope === 'personal' ? 'pill-accent' : ''}`}>
          <span className="pill-dot" aria-hidden="true" />
          {bot.scope === 'personal' ? B.personal : B.team}
        </span>
      </header>
      {edits.length > 0 && (
        <div className="bot-flag" role="note">
          <span className="flag-tag"><Icon name="alert" size={12} weight={2.25} />{B.handEdit}</span>
          <p>{B.handEditBody}</p>
          <ul>
            {edits.map((e) => <li key={e} className="mono-val">{e}</li>)}
          </ul>
        </div>
      )}
      <dl className="bot-grid">
        <div className="bot-fact">
          <dt>{B.role}</dt>
          <dd className="mono-val">{bot.role}</dd>
        </div>
        <div className="bot-fact">
          <dt>{B.owner}</dt>
          <dd className={unassigned ? 'fact-quiet' : ''}>{unassigned ? B.ownerNone : `@${owner}`}</dd>
        </div>
        <div className="bot-fact bot-fact-wide">
          <dt>{B.skills}</dt>
          <dd><Chips items={bot.skills} label={`${B.skills}, ${bot.bot}`} /></dd>
        </div>
        <div className="bot-fact bot-fact-wide">
          <dt>{B.tools}</dt>
          <dd>{bot.tools.length ? <Chips items={bot.tools} mono tone label={`${B.tools}, ${bot.bot}`} /> : <span className="fact-quiet">{B.toolsNone}</span>}</dd>
        </div>
        {a && (
          <>
            <div className="bot-fact bot-fact-wide">
              <dt>{B.talksTo}</dt>
              <dd>
                <ul className="dest-list">
                  {a.network.map((n) => {
                    const d = destinationOf(n);
                    return (
                      <li key={n.rule} className={d.unknown ? 'dest dest-unknown' : 'dest'}>
                        <span className="dest-name">{d.name}</span>
                        <span className="mono-val dest-where">{d.where}</span>
                        <span className="mono-val dest-proto">{d.protocol}</span>
                      </li>
                    );
                  })}
                </ul>
              </dd>
            </div>
            <div className="bot-fact">
              <dt>{B.reads}</dt>
              <dd>
                <ul className="path-list">
                  {reads!.content.map((p) => <li key={p} className="mono-val">{p}</li>)}
                </ul>
                {reads!.system > 0 && <span className="fact-quiet small">{B.system(reads!.system)}</span>}
              </dd>
            </div>
            <div className="bot-fact">
              <dt>{B.writes}</dt>
              <dd>
                <ul className="path-list">
                  {a.filesystem.read_write.map((p) => <li key={p} className="mono-val">{p}</li>)}
                </ul>
              </dd>
            </div>
          </>
        )}
      </dl>
      {!a && <p className="bot-missing"><Icon name="alert" size={14} /> {B.noAllowances}</p>}
      <footer className="bot-foot">
        {a && a.summary.length > 0 && (
          <details className="bot-sentences">
            <summary>
              <Icon name="chevronRight" size={14} className="disclose-icon" />
              {B.sentences}
            </summary>
            <ul>
              {a.summary.map((s) => <li key={s}>{s}</li>)}
            </ul>
          </details>
        )}
        <span className="bot-file mono-val" title={bot.allowances_from === 'generated' ? B.fromGenerated : undefined}>{bot.policy_file}</span>
      </footer>
    </article>
  );
}

function BotsSection({ data }: { data: PolicyView }) {
  const [only, setOnly] = useState<string | null>(null);
  const shown = only ? data.bots.filter((b) => b.bot === only) : data.bots;
  return (
    <section className="card" aria-labelledby="policy-bots">
      <div className="card-head card-head-sub">
        <h2 id="policy-bots">
          {B.heading}
          <span className="card-count">{data.bots.length}</span>
        </h2>
        {data.bots.length > 1 && (
          <div className="seg-filter" role="group" aria-label={B.filterLabel}>
            <button type="button" className="seg-btn" aria-pressed={only === null} onClick={() => setOnly(null)}>{B.all}</button>
            {data.bots.map((b) => (
              <button key={b.bot} type="button" className="seg-btn mono-val" aria-pressed={only === b.bot} onClick={() => setOnly(only === b.bot ? null : b.bot)}>
                {b.bot}
              </button>
            ))}
          </div>
        )}
      </div>
      {data.bots.length === 0 ? (
        <Empty icon="cpu" small>{B.noBots}</Empty>
      ) : (
        <div className="bot-cards">
          {shown.map((b) => <BotCard key={b.bot} bot={b} owner={data.spec.bots[b.bot]?.owner ?? null} />)}
        </div>
      )}
    </section>
  );
}

// ---------- Fixed parts and files ----------

function FixedSection({ data }: { data: PolicyView }) {
  const F = T.fixed;
  const f = data.fixed;
  return (
    <details className="card disclose-card">
      <summary className="card-head">
        <h2>
          <Icon name="chevronRight" size={15} className="disclose-icon" />
          {F.heading}
        </h2>
        <span className="tag"><Icon name="lock" size={12} />&nbsp;{F.badge}</span>
      </summary>
      <div className="card-body stack stack-tight">
        <p className="muted small">{F.note(f.source)}</p>
        <dl className="facts">
          <dt>{F.destinations}</dt>
          <dd>
            <ul className="fact-lines">
              {f.services.map((s) => (
                <li key={s.rule}>
                  <strong>{B.services[s.service] ?? s.service}</strong>{' '}
                  <span className="mono-val">{s.host}:{s.port}</span>{' '}
                  <span className="mono-val muted">{s.protocol}{s.routes ? `, ${F.routes(s.routes.length)}` : ''}, {s.credential_profile ?? F.noCredential}</span>
                </li>
              ))}
            </ul>
          </dd>
          <dt>{F.mcp}</dt>
          <dd className="mono-val">{F.mcpLine(f.mcp_path, f.mcp_methods.length, f.mcp_versions.join(', '))}</dd>
          <dt>{F.worker}</dt>
          <dd className="mono-val">{f.worker_binary}</dd>
          <dt>{F.workspace}</dt>
          <dd className="mono-val">{f.workspace}</dd>
          <dt>{F.content}</dt>
          <dd className="mono-val">{f.content_mount}</dd>
          <dt>{F.shared}</dt>
          <dd className="mono-val">{f.shared_content.join(', ')}</dd>
          <dt>{F.sysRo}</dt>
          <dd className="mono-val">{f.system_read_only.join(', ')}</dd>
          <dt>{F.sysRw}</dt>
          <dd className="mono-val">{f.system_read_write.join(', ')}</dd>
          <dt>{F.runAs}</dt>
          <dd className="mono-val">{f.run_as}</dd>
          <dt>{F.landlock}</dt>
          <dd className="mono-val">{f.landlock}</dd>
        </dl>
      </div>
    </details>
  );
}

const STATE_TONE: Record<string, string> = { same: 'pill-good', differs: 'pill-warn', missing: 'pill-warn', orphan: 'pill-warn', unknown: '' };

function FilesSection({ data }: { data: PolicyView }) {
  const F = T.files;
  const off = data.files.filter((f) => f.state !== 'same').length;
  return (
    <details className="card disclose-card">
      <summary className="card-head">
        <h2>
          <Icon name="chevronRight" size={15} className="disclose-icon" />
          {F.heading}
          <span className="card-count">{data.files.length}</span>
        </h2>
        {off > 0 && <span className="pill pill-warn"><span className="pill-dot" aria-hidden="true" />{T.strip.drift(off)}</span>}
      </summary>
      <ul className="rows" aria-label={F.tableLabel}>
        {data.files.map((f) => (
          <li key={f.path} className="file-state-row">
            <div className="row file-row">
              <span className="mono-val row-main file-path">{f.path}</span>
              <span className="fact-quiet small file-hint">{F.stateHints[f.state] ?? ''}</span>
              <span className={`pill ${STATE_TONE[f.state] ?? ''}`}><span className="pill-dot" aria-hidden="true" />{F.states[f.state] ?? f.state}</span>
            </div>
            {f.state === 'differs' && (
              <details className="fdiff fdiff-inline">
                <summary>
                  <Icon name="chevronRight" size={14} className="disclose-icon" />
                  <span>{F.showDiff(f.path)}</span>
                </summary>
                <DiffView path={f.path} diff={unifiedDiff(f.path, f.checked_in, f.generated)} />
              </details>
            )}
          </li>
        ))}
      </ul>
    </details>
  );
}

// ---------- Proposing a change ----------

type Tab = 'bots' | 'roles' | 'skills' | 'raw';

/** Where a patch changes something, as short mono paths: `bots.docs.owner`. */
function changedPaths(patch: SpecPatch): string[] {
  const out: string[] = [];
  for (const [b, f] of Object.entries(patch.bots ?? {})) for (const k of Object.keys(f)) out.push(`bots.${b}.${k}`);
  for (const r of Object.keys(patch.roles ?? {})) out.push(`roles.${r}.skills`);
  for (const s of Object.keys(patch.skills ?? {})) out.push(`skills.${s}.tools`);
  return out;
}

function ToolsInput({ skill, tools, changed, onChange }: { skill: string; tools: string[]; changed: boolean; onChange: (tools: string[]) => void }) {
  // The words as typed, so a space at the end survives until the next word.
  const [text, setText] = useState(tools.join(' '));
  const E = T.editor;
  return (
    <input
      type="text"
      className={`mono-input ${changed ? 'is-edited' : ''}`}
      aria-label={E.toolsOf(skill)}
      value={text}
      spellCheck={false}
      onChange={(e) => {
        setText(e.target.value);
        onChange(splitTools(e.target.value));
      }}
    />
  );
}

function Editor({ data, onOpened, proposalsVersion }: { data: PolicyView; onOpened: () => void; proposalsVersion: () => void }) {
  const { api } = useApp();
  const E = T.editor;
  const base = data.spec;
  const [patch, setPatch] = usePending<SpecPatch>(api, 'policy.patch', {});
  const [raw, setRaw] = usePending<string | null>(api, 'policy.raw', null);
  const [rawError, setRawError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>(raw !== null ? 'raw' : 'bots');
  const [resetKey, setResetKey] = useState(0);
  const edited = useMemo(() => applySpecPatch(base, patch), [base, patch]);
  const size = patchSize(patch);
  const key = stableKey(patch);
  const empty = size === 0 || rawError !== null;
  useLeaveGuard(size > 0);

  const validate = useCallback(() => api.adminPolicyValidate(patch), [api, patch]);
  const propose = useCallback((title: string, reason: string) => api.adminPolicyPropose({ spec: patch, title, reason }), [api, patch]);
  const flow = useProposalFlow({
    key,
    empty,
    validate,
    propose,
    onOpened: () => {
      setPatch({});
      setRaw(null);
      setResetKey((k) => k + 1);
      onOpened();
    },
  });
  const checkProposals = useCheckProposals(PROPOSALS_ID, proposalsVersion, flow.closeDialog);

  const update = (next: PolicySpec) => {
    setPatch(specPatch(base, next));
    setRaw(null);
  };
  const discard = () => {
    setPatch({});
    setRaw(null);
    setRawError(null);
    setResetKey((k) => k + 1);
    flow.reset();
  };
  const choose = (t: Tab) => {
    if (t === 'raw' && raw === null) setRaw(JSON.stringify(patch, null, 2));
    setTab(t);
  };

  const was = (bot: string, f: 'role' | 'scope' | 'owner') => patch.bots?.[bot]?.[f] !== undefined;
  const roles = Object.keys(edited.roles);
  const skills = Object.keys(edited.skills);
  const usedBy = (skill: string) => roles.filter((r) => edited.roles[r]!.skills.includes(skill));
  const paths = changedPaths(patch);
  const tabs: Tab[] = ['bots', 'roles', 'skills', 'raw'];

  return (
    <section className="card propose-card" aria-labelledby="policy-propose">
      <div className="card-head card-head-sub">
        <h2 id="policy-propose">
          <Icon name="edit" size={15} />
          {E.heading}
        </h2>
        {size > 0 && <span className="pill pill-accent"><span className="pill-dot" aria-hidden="true" />{E.changes(size)}</span>}
      </div>
      <div className="card-body propose-intro">
        <p className="muted small">{E.lede}</p>
      </div>
      <div className="card-tabs">
        <div className="tabs" role="tablist" aria-label={E.tabsLabel}>
          {tabs.map((t) => (
            <button key={t} type="button" role="tab" id={`ptab-${t}`} aria-controls={`ppanel-${t}`} aria-selected={tab === t} className="tab" onClick={() => choose(t)}>
              {E.tabs[t]}
            </button>
          ))}
        </div>
      </div>
      <div className="tabpanel" role="tabpanel" id={`ppanel-${tab}`} aria-labelledby={`ptab-${tab}`} key={`${tab}-${resetKey}`}>
        {tab === 'bots' && (
          <div className="table-scroll">
            <table className="dtable edit-table">
              <thead>
                <tr>
                  <th scope="col">{E.cols.bot}</th>
                  <th scope="col">{E.cols.role}</th>
                  <th scope="col">{E.cols.scope}</th>
                  <th scope="col">{E.cols.owner}</th>
                </tr>
              </thead>
              <tbody>
                {Object.entries(edited.bots).map(([bot, b]) => (
                  <tr key={bot}>
                    <th scope="row" className="mono-val">{bot}</th>
                    <td>
                      <select className={was(bot, 'role') ? 'is-edited' : ''} aria-label={E.roleLabel(bot)} value={b.role} onChange={(e) => update({ ...edited, bots: { ...edited.bots, [bot]: { ...b, role: e.target.value } } })}>
                        {[...new Set([...roles, b.role])].map((r) => <option key={r} value={r}>{r}</option>)}
                      </select>
                    </td>
                    <td>
                      <select className={was(bot, 'scope') ? 'is-edited' : ''} aria-label={E.scopeLabel(bot)} value={b.scope} onChange={(e) => update({ ...edited, bots: { ...edited.bots, [bot]: { ...b, scope: e.target.value } } })}>
                        {[...new Set(['team', 'personal', b.scope])].map((s) => <option key={s} value={s}>{E.scopes[s] ?? s}</option>)}
                      </select>
                    </td>
                    <td>
                      <input type="text" className={`mono-input ${was(bot, 'owner') ? 'is-edited' : ''}`} aria-label={E.ownerLabel(bot)} title={E.ownerHint} value={b.owner} spellCheck={false} onChange={(e) => update({ ...edited, bots: { ...edited.bots, [bot]: { ...b, owner: e.target.value.trim() } } })} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {tab === 'roles' && (
          <ul className="role-edit">
            {roles.map((role) => {
              const have = edited.roles[role]!.skills;
              const order = [...have, ...skills.filter((s) => !have.includes(s))];
              return (
                <li key={role} className={`role-edit-row ${patch.roles?.[role] ? 'is-edited-row' : ''}`}>
                  <span className="mono-val role-edit-name">{role}</span>
                  <fieldset className="skill-boxes">
                    <legend className="sr-only">{E.skillsOf(role)}</legend>
                    {order.map((s) => (
                      <label key={s} className={`skill-box ${have.includes(s) ? 'on' : ''}`}>
                        <input
                          type="checkbox"
                          checked={have.includes(s)}
                          onChange={(e) => {
                            const next = e.target.checked ? [...have, s] : have.filter((x) => x !== s);
                            update({ ...edited, roles: { ...edited.roles, [role]: { skills: next } } });
                          }}
                        />
                        <span className="mono-val">{s}</span>
                      </label>
                    ))}
                  </fieldset>
                </li>
              );
            })}
          </ul>
        )}
        {tab === 'skills' && (
          <div className="table-scroll">
            <table className="dtable edit-table skills-edit">
              <thead>
                <tr>
                  <th scope="col" className="c-skill">{copy.admin.policy.bots.skills}</th>
                  <th scope="col">{copy.admin.policy.bots.tools}</th>
                </tr>
              </thead>
              <tbody>
                {skills.map((s) => {
                  const users = usedBy(s);
                  return (
                    <tr key={s}>
                      <th scope="row">
                        <span className="mono-val block">{s}</span>
                        <span className="fact-quiet small">{users.length ? E.usedBy(users.join(', ')) : E.unused}</span>
                      </th>
                      <td>
                        <ToolsInput
                          skill={s}
                          tools={edited.skills[s]!.tools}
                          changed={patch.skills?.[s] !== undefined}
                          onChange={(tools) => update({ ...edited, skills: { ...edited.skills, [s]: { tools } } })}
                        />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            <p className="field-hint edit-hint">{E.toolsHint}</p>
          </div>
        )}
        {tab === 'raw' && (
          <div className="card-body raw-edit">
            <label className="field-label" htmlFor="policy-raw">{E.rawLabel}</label>
            <textarea
              id="policy-raw"
              className="code-area"
              rows={12}
              spellCheck={false}
              aria-describedby="policy-raw-hint"
              value={raw ?? JSON.stringify(patch, null, 2)}
              onChange={(e) => {
                setRaw(e.target.value);
                const r = parseRawPatch(e.target.value);
                if ('patch' in r) {
                  setPatch(r.patch);
                  setRawError(null);
                } else setRawError(r.error);
              }}
            />
            <p id="policy-raw-hint" className={`field-hint ${rawError ? 'field-error' : ''}`} role={rawError ? 'alert' : undefined}>{rawError ?? E.rawHint}</p>
          </div>
        )}
      </div>
      <div className="propose-foot">
        <div className="change-line">
          {size === 0 ? (
            <span className="muted small">{E.noChanges}</span>
          ) : (
            <>
              <ul className="chip-list" aria-label={E.changes(size)}>
                {paths.map((p) => <li key={p} className="mini-chip mono-val mini-chip-edit">{p}</li>)}
              </ul>
              <button type="button" className="btn btn-sm btn-ghost" onClick={discard}>
                <Icon name="x" size={14} />
                {E.discard}
              </button>
            </>
          )}
        </div>
        {!(flow.done && size === 0) && <ProposeBar flow={flow} empty={empty} configured={data.proposals_configured} />}
        {flow.checkProblem && <ProblemNote problem={flow.checkProblem} onCheckProposals={checkProposals} />}
        <CheckResult flow={flow} headingId="policy-check" />
        {flow.done && <ProposalDone result={flow.done} onAnother={flow.reset} />}
      </div>
      {flow.dialogOpen && <ProposeDialog kind="policy" flow={flow} onCheckProposals={checkProposals} />}
    </section>
  );
}

// ---------- The page ----------

function PolicyBody({ data }: { data: PolicyView }) {
  const { api, me } = useApp();
  const [version, setVersion] = useState(0);
  const bump = useCallback(() => setVersion((v) => v + 1), []);
  const S = T.strip;
  const drifted = data.files.filter((f) => f.state !== 'same' && f.state !== 'unknown').length;
  const violations = data.drift.violations.length;
  const tools = new Set(data.bots.flatMap((b) => b.tools)).size;
  const editable = canEditPolicy(me);
  const loadProposals = useCallback(() => api.adminPolicyProposals(), [api]);

  return (
    <>
      <div className="stats status-strip" role="group" aria-label={S.label}>
        {data.drift.checked_in_available ? (
          <StatusTile
            label={S.files}
            value={drifted === 0 ? S.inSync : S.drift(drifted)}
            sub={drifted === 0 ? S.inSyncSub : S.driftSub}
            tone={drifted === 0 ? 'good' : 'warn'}
            text
          />
        ) : (
          <StatusTile label={S.files} value={S.noRoot} sub={S.noRootSub} text />
        )}
        <StatusTile label={S.rules} value={violations} sub={violations ? S.rulesSub : S.rulesSub0} tone={violations ? 'bad' : undefined} />
        <ProtectionTile protection={data.protection} configured={data.proposals_configured} />
        <StatusTile label={S.bots} value={data.bots.length} sub={S.botsSub(tools)} />
      </div>
      <ProtectionNotes protection={data.protection} configured={data.proposals_configured} />
      {data.generate_errors.length > 0 && (
        <section className="card" aria-labelledby="policy-gen">
          <div className="card-head"><h2 id="policy-gen">{T.generateHeading}</h2></div>
          <div className="card-body stack stack-tight">
            <p className="muted small">{T.generateBody}</p>
            <IssueList issues={data.generate_errors} label={T.generateHeading} />
          </div>
        </section>
      )}
      {violations > 0 && (
        <section className="card card-alert" aria-labelledby="policy-violations">
          <div className="card-head"><h2 id="policy-violations"><Icon name="alert" size={15} />{T.violationsHeading}<span className="card-count">{violations}</span></h2></div>
          <IssueList issues={data.drift.violations} label={T.violationsHeading} compact />
        </section>
      )}
      <BotsSection data={data} />
      {editable ? (
        <>
          {!data.proposals_configured && (
            <p className="meta-line"><Icon name="info" size={15} /><span>{copy.admin.repo.proposalsOff}</span></p>
          )}
          <Editor data={data} onOpened={bump} proposalsVersion={bump} />
        </>
      ) : (
        <p className="meta-line"><Icon name="lock" size={15} /><span>{copy.admin.repo.readOnly}</span></p>
      )}
      <ProposalsCard kind="policy" id={PROPOSALS_ID} load={loadProposals} version={version} />
      <FixedSection data={data} />
      <FilesSection data={data} />
    </>
  );
}

/** Admins and auditors: what each bot may do, what is fixed, and (admins) a way to propose a change. */
export function Policy() {
  const { api } = useApp();
  const load = useCallback(() => api.adminPolicy(), [api]);
  const { data, error, forbidden, reload } = useLoad(load, T.loadFailed);
  if (forbidden) return <NoAccess />;
  return (
    <div className="page page-wide">
      <AdminHead section="policy" title={copy.admin.sections.policy} lede={T.lede}>
        {data && <RevisionChip revision={data.revision} />}
      </AdminHead>
      {error && <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>{error}</ErrorNote>}
      {!data && !error && <Skeleton rows={4} label={T.loading} variant="cards" />}
      {data && <PolicyBody data={data} />}
    </div>
  );
}
