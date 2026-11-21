import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import { canEditContent } from '../../admin/access';
import { useLeaveGuard, usePending } from '../../admin/pending';
import { MAX_FILES, newPathProblem, removeDraft, stableKey, toChanges, upsertDraft } from '../../admin/proposals';
import type { ContentDraft } from '../../admin/proposals';
import { useLoad } from '../../admin/useLoad';
import { useProposalFlow } from '../../admin/useProposalFlow';
import type { ContentFile, ContentFileText, ContentView, Harness as HarnessData } from '../../api';
import { ApiError } from '../../api';
import { useApp } from '../../app-context';
import { Empty, ErrorNote, Markdown, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import type { IconName } from '../../components/icons';
import { copy } from '../../copy';
import { errorText } from '../../errors';
import { renderMarkdown } from '../../markdown';
import { relTime } from '../../time';
import { AdminHead, NoAccess } from './parts';
import {
  CheckResult,
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

const T = copy.admin.content;
const TR = T.tree;
const PROPOSALS_ID = 'content-proposals';

// ---------- The tree ----------

const GROUP_OF: Record<string, string> = {
  role: 'role',
  bot: 'bot',
  persona: 'persona',
  skill: 'skill',
  skill_file: 'skill',
  playbook: 'playbook',
  context: 'context',
  template: 'template',
  reviewer: 'reviewer',
  master: 'master',
  personal_master: 'master',
};
const GROUPS = ['role', 'bot', 'persona', 'skill', 'playbook', 'context', 'template', 'reviewer', 'master', 'other'];
const GROUP_ICON: Record<string, IconName> = {
  role: 'followups',
  bot: 'cpu',
  persona: 'mascot',
  skill: 'sparkle',
  playbook: 'docs',
  context: 'layers',
  template: 'file',
  reviewer: 'shield',
  master: 'home',
  other: 'file',
};

const short = (path: string) => path.replace(/^content\//, '');
const fileName = (path: string) => path.split('/').pop() ?? path;

/** The name a person would call a file: a role, bot or skill by its name, anything else by its title. */
function labelOf(f: ContentFile): string {
  if (f.kind === 'role' || f.kind === 'bot' || f.kind === 'skill') return f.name ?? f.path.split('/')[2] ?? fileName(f.path);
  return f.title ?? fileName(f.path);
}

function LintDot({ f }: { f: Pick<ContentFile, 'lint'> }) {
  const l = f.lint;
  const text = l.status === 'error' ? TR.lintError(l.errors) : l.status === 'warning' ? TR.lintWarn(l.warnings) : TR.lintOk;
  return (
    <span className={`lint-dot lint-${l.status}`} title={text}>
      <span className="sr-only">{text}</span>
      {l.status !== 'ok' && <span aria-hidden="true">{l.status === 'error' ? l.errors : l.warnings}</span>}
    </span>
  );
}

interface TreeProps {
  data: ContentView;
  drafts: ContentDraft[];
  selected: string | null;
  onOpen: (path: string) => void;
  canEdit: boolean;
  onAdd: (path: string) => void;
}

function Tree({ data, drafts, selected, onOpen, canEdit, onAdd }: TreeProps) {
  const [q, setQ] = useState('');
  const [adding, setAdding] = useState(false);
  const [newPath, setNewPath] = useState('content/playbooks/');
  const [addError, setAddError] = useState<string | null>(null);
  const id = useId();
  const ql = q.trim().toLowerCase();
  const toolsOf = useMemo(() => new Map(data.skills.map((s) => [s.name, s])), [data.skills]);
  const roleOf = useMemo(() => new Map(data.roles.map((r) => [r.name, r])), [data.roles]);
  const draftOf = new Map(drafts.map((d) => [d.path, d]));
  const added: ContentFile[] = drafts
    .filter((d) => d.kind === 'add')
    .map((d) => ({ path: d.path, kind: 'other', name: null, title: null, version: null, owner: null, sensitivity: null, last_reviewed: null, size: new TextEncoder().encode((d as { text: string }).text).length, editable: true, readable: true, lint: { status: 'ok', errors: 0, warnings: 0, issues: [] } }));
  const all = [...data.files, ...added];

  const extra = (f: ContentFile): string | null => {
    if (f.kind === 'skill') {
      const s = toolsOf.get(f.name ?? '');
      return s ? (s.used_by.length ? TR.usedBy(s.used_by.join(', ')) : TR.unused) : null;
    }
    if (f.kind === 'role') {
      const r = roleOf.get(f.name ?? '');
      return r ? TR.skillsOf(r.skills.join(', ')) : null;
    }
    return null;
  };
  const matches = (f: ContentFile) => {
    if (!ql) return true;
    const hay = [f.path, f.name, f.title, f.owner, extra(f), ...(f.kind === 'skill' ? toolsOf.get(f.name ?? '')?.tools ?? [] : [])].join(' ').toLowerCase();
    return hay.includes(ql);
  };
  const shown = all.filter(matches);
  const grouped = GROUPS.map((g) => ({ g, files: shown.filter((f) => (GROUP_OF[f.kind] ?? 'other') === g) })).filter((x) => x.files.length > 0);
  const existing = new Set(all.map((f) => f.path));

  const add = () => {
    const p = newPath.trim();
    const problem = newPathProblem(p, existing);
    if (problem) {
      setAddError(problem);
      return;
    }
    onAdd(p);
    setAdding(false);
    setAddError(null);
    setNewPath('content/playbooks/');
  };

  return (
    <section className="card tree-card" aria-labelledby={`${id}-h`}>
      <div className="card-head">
        <h2 id={`${id}-h`}>
          {TR.heading}
          <span className="card-count">{TR.count(data.files.length)}</span>
        </h2>
        {canEdit && !adding && (
          <button type="button" className="btn btn-sm btn-ghost" onClick={() => setAdding(true)}>
            <Icon name="plus" size={14} />
            {TR.newFile}
          </button>
        )}
      </div>
      {adding && (
        <form
          className="add-file"
          onSubmit={(e) => {
            e.preventDefault();
            add();
          }}
        >
          <label className="field-label" htmlFor={`${id}-np`}>{T.add.path}</label>
          <input
            id={`${id}-np`}
            type="text"
            className="mono-input"
            value={newPath}
            spellCheck={false}
            aria-describedby={`${id}-nph`}
            aria-invalid={addError ? true : undefined}
            onChange={(e) => {
              setNewPath(e.target.value);
              setAddError(null);
            }}
            autoFocus
          />
          <p id={`${id}-nph`} className={`field-hint ${addError ? 'field-error' : ''}`} role={addError ? 'alert' : undefined}>{addError ?? T.add.pathHint}</p>
          <div className="add-file-actions">
            <button type="submit" className="btn btn-sm btn-primary">{T.add.add}</button>
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => { setAdding(false); setAddError(null); }}>{T.add.cancel}</button>
          </div>
        </form>
      )}
      <div className="tree-search">
        <Icon name="search" size={15} className="tree-search-icon" />
        <input type="search" aria-label={TR.search} placeholder={TR.searchPlaceholder} value={q} onChange={(e) => setQ(e.target.value)} />
      </div>
      <nav className="tree" aria-label={TR.label}>
        {grouped.length === 0 && <p className="tree-empty muted small">{TR.noMatch}</p>}
        {grouped.map(({ g, files }) => (
          <div key={g} className="tree-group">
            <h3 className="tree-group-head micro-label">
              <Icon name={GROUP_ICON[g] ?? 'file'} size={13} />
              {TR.groups[g]}
              <span className="tree-group-count">{files.length}</span>
            </h3>
            <ul>
              {files.map((f) => {
                const d = draftOf.get(f.path);
                const ex = extra(f);
                const meta = (
                  <span className="tree-meta">
                    <span className="mono-val tree-path">{short(f.path)}</span>
                    {f.version && <span className="mono-val">v{f.version}</span>}
                    {f.owner && <span className={f.owner === 'TODO' ? 'tree-todo' : ''}>{f.owner === 'TODO' ? copy.admin.policy.bots.ownerNone : `@${f.owner}`}</span>}
                  </span>
                );
                if (!f.readable) {
                  return (
                    <li key={f.path}>
                      <div className="tree-row tree-locked" title={TR.lockedReason}>
                        <span className="tree-main">
                          <span className="tree-name">
                            <Icon name="lock" size={13} />
                            {labelOf(f)}
                            <span className="tag">{f.kind === 'personal_master' ? TR.personal : TR.restricted}</span>
                          </span>
                          {meta}
                          <span className="tree-why">{TR.lockedReason}</span>
                        </span>
                        <span className="tree-side"><LintDot f={f} /></span>
                      </div>
                    </li>
                  );
                }
                return (
                  <li key={f.path}>
                    <button type="button" className="tree-row" aria-current={selected === f.path ? 'true' : undefined} onClick={() => onOpen(f.path)}>
                      <span className="tree-main">
                        <span className="tree-name">
                          {labelOf(f)}
                          {f.sensitivity === 'restricted' && <span className="tag tag-warn">{TR.restricted}</span>}
                          {f.kind === 'personal_master' && <span className="tag">{TR.personal}</span>}
                          {!f.editable && <span className="tag">{TR.readOnly}</span>}
                        </span>
                        {meta}
                        {ex && <span className="tree-extra">{ex}</span>}
                      </span>
                      <span className="tree-side">
                        {d && <span className={`change-tag change-${d.kind}`}>{TR.changed[d.kind]}</span>}
                        <span className="mono-val tree-size">{TR.size(f.size)}</span>
                        <LintDot f={f} />
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </nav>
    </section>
  );
}

// ---------- One file ----------

/** Front matter as rows: nested maps flattened (`metadata.owner`), lists joined. */
function factsOf(fm: Record<string, unknown>): [string, string][] {
  const out: [string, string][] = [];
  const walk = (prefix: string, v: unknown) => {
    if (v && typeof v === 'object' && !Array.isArray(v)) for (const [k, x] of Object.entries(v)) walk(prefix ? `${prefix}.${k}` : k, x);
    else out.push([prefix, Array.isArray(v) ? v.join(', ') : String(v ?? '')]);
  };
  walk('', fm);
  return out;
}

const bodyOf = (text: string) => {
  const m = /^---\n[\s\S]*?\n---\n?/.exec(text);
  return m ? text.slice(m[0].length) : text;
};

/** A plain text editor with line numbers beside it. The numbers follow the text as it scrolls. */
function TextEditor({ value, onChange, label }: { value: string; onChange: (v: string) => void; label: string }) {
  const gutter = useRef<HTMLDivElement>(null);
  const lines = value.split('\n').length;
  return (
    <div className="text-editor">
      <div ref={gutter} className="te-gutter" aria-hidden="true">
        {Array.from({ length: lines }, (_, i) => <span key={i}>{i + 1}</span>)}
      </div>
      <textarea
        className="te-area"
        aria-label={label}
        value={value}
        spellCheck={false}
        wrap="off"
        onChange={(e) => onChange(e.target.value)}
        onScroll={(e) => {
          if (gutter.current) gutter.current.scrollTop = e.currentTarget.scrollTop;
        }}
      />
    </div>
  );
}

function RawText({ text, label }: { text: string; label: string }) {
  const lines = text.replace(/\n$/, '').split('\n');
  return (
    <div className="raw-view" role="region" tabIndex={0} aria-label={label}>
      {lines.map((l, i) => (
        <div key={i} className="raw-line">
          <span className="udiff-no" aria-hidden="true">{i + 1}</span>
          <span className="raw-text">{l || ' '}</span>
        </div>
      ))}
    </div>
  );
}

interface FileViewProps {
  path: string;
  listed: ContentFile | null;
  draft: ContentDraft | undefined;
  canEdit: boolean;
  onDraft: (d: ContentDraft) => void;
  onUndo: (path: string) => void;
  onClose: () => void;
}

function FileView({ path, listed, draft, canEdit, onDraft, onUndo, onClose }: FileViewProps) {
  const { api } = useApp();
  const F = T.file;
  const isNew = draft?.kind === 'add';
  const load = useCallback(async (): Promise<ContentFileText | null> => {
    if (isNew) return null;
    try {
      return await api.adminContentFile(path);
    } catch (e) {
      // audit_unavailable and the other codes read as this app's own sentence.
      if (e instanceof ApiError && e.status !== 403) throw new ApiError(e.status, e.code, errorText(e, F.loadFailed));
      throw e;
    }
  }, [api, path, isNew, F.loadFailed]);
  const { data, error, forbidden, reload } = useLoad(load, F.loadFailed);
  const [view, setView] = useState<'rendered' | 'raw'>('rendered');
  const [editing, setEditing] = useState(isNew);
  const id = useId();

  const kind = listed?.kind ?? data?.kind ?? 'other';
  const md = path.endsWith('.md');
  const text = draft && draft.kind !== 'delete' ? draft.text : data?.text ?? '';
  const editable = (listed?.editable ?? data?.editable ?? true) && canEdit;
  const deleting = draft?.kind === 'delete';
  const rendered = useMemo(() => (md ? renderMarkdown(bodyOf(text)) : null), [md, text]);
  const fm = data && !draft ? data.front_matter : null;

  const edit = (next: string) => {
    if (isNew) onDraft({ path, kind: 'add', text: next });
    else if (data) onDraft({ path, kind: 'edit', text: next, original: data.text, base_sha256: data.sha256 });
  };

  return (
    <section className="card file-card" aria-labelledby={`${id}-h`}>
      <div className="card-head file-head">
        <div className="file-title">
          <span className="file-kind micro-label">{F.kinds[kind] ?? kind}</span>
          <h2 id={`${id}-h`} className="mono-val file-path-h">{short(path)}</h2>
        </div>
        <div className="file-actions">
          {editable && !deleting && (
            editing ? (
              <button type="button" className="btn btn-sm" onClick={() => setEditing(false)}>
                <Icon name="check" size={14} />
                {F.done}
              </button>
            ) : (
              <button type="button" className="btn btn-sm" disabled={!data && !isNew} onClick={() => setEditing(true)}>
                <Icon name="edit" size={14} />
                {F.edit}
              </button>
            )
          )}
          {editable && !isNew && !deleting && (
            <button type="button" className="btn btn-sm btn-ghost btn-danger-text" disabled={!data} onClick={() => { setEditing(false); onDraft({ path, kind: 'delete' }); }}>
              <Icon name="trash" size={14} />
              {F.delete}
            </button>
          )}
          <button type="button" className="icon-btn" aria-label={F.close} title={F.close} onClick={onClose}>
            <Icon name="x" size={16} />
          </button>
        </div>
      </div>
      {forbidden && (
        <div className="card-body">
          <Empty icon="lock" small>{TR.lockedReason}</Empty>
        </div>
      )}
      {error && !forbidden && (
        <div className="card-body"><ErrorNote actionLabel={copy.errors.retry} onAction={reload}>{error}</ErrorNote></div>
      )}
      {!data && !error && !forbidden && !isNew && <div className="card-body"><Skeleton rows={4} label={F.loading} variant="doc" /></div>}
      {(data || isNew) && (
        <>
          {(draft || !editable) && (
            <div className="file-notes">
              {deleting && (
                <div className="note note-warn">
                  <Icon name="trash" className="note-icon" />
                  <div className="note-body">
                    <p>{F.willDelete}</p>
                    <button type="button" className="btn btn-sm" onClick={() => onUndo(path)}>{F.keep}</button>
                  </div>
                </div>
              )}
              {isNew && <p className="file-note"><span className="change-tag change-add">{TR.changed.add}</span>{F.isNew}</p>}
              {draft?.kind === 'edit' && (
                <p className="file-note">
                  <span className="change-tag change-edit">{TR.changed.edit}</span>
                  {F.edited}
                  <button type="button" className="btn btn-sm btn-ghost" onClick={() => { setEditing(false); onUndo(path); }}>{F.revert}</button>
                </p>
              )}
              {!editable && canEdit && (
                <p className="file-note"><Icon name="lock" size={13} />{F.notEditable} {kind === 'team' ? F.teamNote : ''}</p>
              )}
            </div>
          )}
          {data && (
            <dl className="file-meta">
              <div><dt>{F.size}</dt><dd className="mono-val">{TR.size(data.size)}</dd></div>
              <div><dt>{F.kind}</dt><dd>{F.kinds[data.kind] ?? data.kind}</dd></div>
              <div><dt>sha256</dt><dd className="mono-val" title={data.sha256}>{data.sha256.slice(0, 12)}</dd></div>
              <div><dt>{F.linesLabel}</dt><dd className="mono-val">{text.replace(/\n$/, '').split('\n').length}</dd></div>
            </dl>
          )}
          {fm && Object.keys(fm).length > 0 && (
            <div className="card-body file-facts">
              <h3 className="micro-label">{F.facts}</h3>
              <dl className="facts facts-compact">
                {factsOf(fm).map(([k, v]) => (
                  <div key={k} className="facts-row">
                    <dt className="mono-val">{k}</dt>
                    <dd>{v === 'TODO' ? <span className="tree-todo">{copy.admin.policy.bots.ownerNone}</span> : v}</dd>
                  </div>
                ))}
              </dl>
            </div>
          )}
          {data?.front_matter_error && !draft && (
            <div className="card-body"><ErrorNote>{F.frontError(data.front_matter_error)}</ErrorNote></div>
          )}
          {listed && listed.lint.issues.length > 0 && !draft && (
            <div className="card-body file-findings">
              <h3 className="micro-label">{F.findings}</h3>
              <IssueList issues={listed.lint.issues} label={F.findings} compact />
            </div>
          )}
          <div className="file-body">
            {editing && !deleting ? (
              <TextEditor value={text} onChange={edit} label={F.editorLabel(path)} />
            ) : (
              <>
                {md && (
                  <div className="file-view-toggle">
                    <div className="segment" role="radiogroup" aria-label={F.viewLabel}>
                      {(['rendered', 'raw'] as const).map((v) => (
                        <button key={v} type="button" role="radio" aria-checked={view === v} className="segment-item" onClick={() => setView(v)}>
                          {F[v]}
                        </button>
                      ))}
                    </div>
                  </div>
                )}
                {md && view === 'rendered' && rendered ? (
                  <Markdown rendered={rendered} className="file-md" />
                ) : (
                  <RawText text={text} label={F.editorLabel(path)} />
                )}
              </>
            )}
          </div>
        </>
      )}
    </section>
  );
}

// ---------- The change tray ----------

function Tray({ drafts, configured, onOpen, onRemove, onDiscard, onOpened, bump }: {
  drafts: ContentDraft[];
  configured: boolean;
  onOpen: (path: string) => void;
  onRemove: (path: string) => void;
  onDiscard: () => void;
  onOpened: () => void;
  bump: () => void;
}) {
  const { api } = useApp();
  const changes = useMemo(() => toChanges(drafts), [drafts]);
  const tooMany = changes.length > MAX_FILES;
  const key = stableKey(changes);
  const validate = useCallback(() => api.adminContentValidate(changes), [api, changes]);
  const propose = useCallback((title: string, reason: string) => api.adminContentPropose({ changes, title, reason }), [api, changes]);
  const flow = useProposalFlow({ key, empty: changes.length === 0 || tooMany, validate, propose, onOpened });
  const checkProposals = useCheckProposals(PROPOSALS_ID, bump, flow.closeDialog);
  const Y = T.tray;
  if (drafts.length === 0 && !flow.done) return null;
  return (
    <section id="content-tray" className="card propose-card tray" aria-labelledby="content-tray-h">
      <div className="card-head card-head-sub">
        <h2 id="content-tray-h">
          <Icon name="edit" size={15} />
          {Y.heading}
        </h2>
        {drafts.length > 0 && (
          <span className="card-head-end">
            <span className="pill pill-accent"><span className="pill-dot" aria-hidden="true" />{Y.count(drafts.length)}</span>
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => { onDiscard(); flow.reset(); }}>
              <Icon name="x" size={14} />
              {Y.discardAll}
            </button>
          </span>
        )}
      </div>
      {drafts.length > 0 && (
        <ul className="rows tray-rows" aria-label={Y.listLabel}>
          {drafts.map((d) => (
            <li key={d.path} className="row tray-row">
              <span className={`change-tag change-${d.kind}`}>{TR.changed[d.kind]}</span>
              <button type="button" className="tray-path mono-val" onClick={() => onOpen(d.path)} aria-label={Y.open(d.path)}>{d.path}</button>
              <button type="button" className="icon-btn icon-btn-sm" aria-label={Y.remove(d.path)} title={Y.remove(d.path)} onClick={() => onRemove(d.path)}>
                <Icon name="x" size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="propose-foot">
        {drafts.length > 0 && <p className="muted small tray-kept"><Icon name="info" size={14} /> {Y.kept}</p>}
        {tooMany && <ErrorNote>{Y.tooMany}</ErrorNote>}
        {drafts.length > 0 && <ProposeBar flow={flow} empty={changes.length === 0 || tooMany} configured={configured} many />}
        {flow.checkProblem && <ProblemNote problem={flow.checkProblem} onCheckProposals={checkProposals} />}
        <CheckResult flow={flow} headingId="content-check" />
        {flow.done && <ProposalDone result={flow.done} onAnother={flow.reset} />}
      </div>
      {flow.dialogOpen && <ProposeDialog kind="content" flow={flow} onCheckProposals={checkProposals} />}
    </section>
  );
}

// ---------- The harness ----------

function HarnessCard() {
  const { api } = useApp();
  const H = T.harness;
  const load = useCallback(() => api.adminHarness(), [api]);
  const { data, error, reload } = useLoad<HarnessData>(load, H.failed);
  return (
    <section className="card card-table" aria-labelledby="harness-h">
      <div className="card-head">
        <h2 id="harness-h">
          <Icon name="cpu" size={15} />
          {H.heading}
        </h2>
        <span className="muted small harness-lede">{H.lede}</span>
      </div>
      {error && <div className="card-body"><ErrorNote actionLabel={copy.errors.retry} onAction={reload}>{error}</ErrorNote></div>}
      {!data && !error && <div className="card-body"><Skeleton rows={3} label={H.loading} /></div>}
      {data && (
        <>
          {(data.gateway_config !== 'ok' || !data.runtime.reachable) && (
            <div className="card-body stack stack-tight">
              {data.gateway_config !== 'ok' && <div className="note note-warn"><Icon name="alert" className="note-icon" /><div className="note-body"><p>{H.gateway[data.gateway_config]}</p></div></div>}
              {!data.runtime.reachable && <div className="note note-warn"><Icon name="alert" className="note-icon" /><div className="note-body"><p>{H.runtimeDown}</p></div></div>}
            </div>
          )}
          {data.bots.length === 0 ? (
            <Empty icon="cpu" small>{H.empty}</Empty>
          ) : (
            <div className="table-scroll" role="region" tabIndex={0} aria-label={H.tableLabel}>
              <table className="dtable harness-table" aria-label={H.tableLabel}>
                <thead>
                  <tr>
                    <th scope="col" className="c-bot">{H.cols.bot}</th>
                    <th scope="col" className="c-role">{H.cols.role}</th>
                    <th scope="col" className="c-mrole">{H.cols.modelRole}</th>
                    <th scope="col" className="c-model">{H.cols.model}</th>
                    <th scope="col" className="c-loaded">{H.cols.loaded}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.bots.map((b) => (
                    <tr key={b.name}>
                      <th scope="row" className="c-bot">
                        <span className="mono-val harness-bot">{b.name}</span>
                        <span className="fact-quiet small">{b.scope === 'personal' ? copy.admin.policy.bots.personal : copy.admin.policy.bots.team}</span>
                      </th>
                      <td className="c-role mono-val">{b.role}</td>
                      <td className="c-mrole">{b.model_role ? <span className="mini-chip mono-val">{b.model_role}</span> : <span className="fact-quiet">–</span>}</td>
                      <td className="c-model">
                        {b.gateway_model ? <span className="mono-val harness-model">{b.gateway_model}</span> : <span className="fact-quiet">–</span>}
                        {b.gateway_alias && <span className="fact-quiet mono-val small block">{b.gateway_alias}</span>}
                      </td>
                      <td className="c-loaded">
                        {b.loaded_by_runtime === null ? (
                          <span className="pill"><span className="pill-dot" aria-hidden="true" />{H.unknown}</span>
                        ) : b.loaded_by_runtime ? (
                          <span className="pill pill-good"><span className="pill-dot" aria-hidden="true" />{H.loaded}</span>
                        ) : (
                          <span className="pill pill-warn" title={H.notLoadedHint}><span className="pill-dot" aria-hidden="true" />{H.notLoaded}</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {data.model_roles && data.model_roles.length > 0 && (
            <details className="disclose harness-models">
              <summary>
                <Icon name="chevronRight" size={15} className="disclose-icon" />
                <span>{H.models}</span>
                <span className="card-count">{data.model_roles.length}</span>
              </summary>
              <div className="table-scroll" role="region" tabIndex={0} aria-label={H.modelsLabel}>
                <table className="dtable models-table" aria-label={H.modelsLabel}>
                  <thead>
                    <tr>
                      <th scope="col">{H.modelCols.role}</th>
                      <th scope="col">{H.modelCols.alias}</th>
                      <th scope="col">{H.modelCols.model}</th>
                      <th scope="col">{H.modelCols.upstream}</th>
                      <th scope="col">{H.modelCols.workarounds}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.model_roles.map((m) => (
                      <tr key={m.role}>
                        <th scope="row" className="mono-val">{m.role}</th>
                        <td className="mono-val">{m.alias}</td>
                        <td className="mono-val">{m.model}</td>
                        <td className="mono-val">{m.upstream}</td>
                        <td className="mono-val">{m.workarounds.length ? m.workarounds.join(', ') : <span className="fact-quiet">{H.none}</span>}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </details>
          )}
          <div className="harness-foot">
            <p className="harness-note"><Icon name="info" size={14} /><span>{H.notShown}</span></p>
            {data.gateway_config === 'ok' && <p className="harness-note muted"><Icon name="info" size={14} /><span>{H.gatewayNote}</span></p>}
          </div>
        </>
      )}
    </section>
  );
}

// ---------- The page ----------

function ContentBody({ data, search }: { data: ContentView; search: string }) {
  const { api, me, navigate } = useApp();
  const canEdit = canEditContent(me);
  const [drafts, setDrafts] = usePending<ContentDraft[]>(api, 'content.drafts', []);
  const [version, setVersion] = useState(0);
  const bump = useCallback(() => setVersion((v) => v + 1), []);
  useLeaveGuard(drafts.length > 0);
  const selected = new URLSearchParams(search).get('file');
  const listed = selected ? data.files.find((f) => f.path === selected) ?? null : null;
  const draftOf = selected ? drafts.find((d) => d.path === selected) : undefined;
  const openable = selected !== null && (listed ? listed.readable : draftOf?.kind === 'add');
  const viewRef = useRef<HTMLDivElement>(null);
  const loadProposals = useCallback(() => api.adminContentProposals(), [api]);

  const open = (path: string) => navigate(`/admin/content?file=${encodeURIComponent(path)}`);
  const close = () => navigate('/admin/content');
  useEffect(() => {
    if (selected && window.matchMedia?.('(max-width: 1023px)').matches) viewRef.current?.scrollIntoView?.({ block: 'start' });
  }, [selected]);

  const S = T.strip;
  const lint = data.lint;
  return (
    <>
      <div className="stats status-strip" role="group" aria-label={S.label}>
        <StatusTile
          label={S.lint}
          value={lint.errors ? S.errors(lint.errors) : lint.warnings ? S.warnings(lint.warnings) : S.lintClean}
          sub={S.checked(relTime(lint.checked_at, Date.now()))}
          tone={lint.errors ? 'bad' : lint.warnings ? 'warn' : 'good'}
          text
        />
        <StatusTile label={S.files} value={data.files.length} sub={S.filesSub(data.roles.length, data.skills.length, data.bots.length)} />
        <ProtectionTile protection={data.protection} configured={data.proposals_configured} />
        {canEdit ? (
          <StatusTile label={T.tray.heading} value={drafts.length} sub={drafts.length ? T.tray.count(drafts.length) : T.tray.empty} tone={drafts.length ? 'warn' : undefined} />
        ) : (
          <StatusTile label={copy.admin.policy.strip.bots} value={data.bots.length} sub={S.filesSub(data.roles.length, data.skills.length, data.bots.length)} />
        )}
      </div>
      <ProtectionNotes protection={data.protection} configured={data.proposals_configured} />
      {lint.other.length > 0 && <IssueList issues={lint.other} label={S.lint} />}
      {!canEdit && <p className="meta-line"><Icon name="lock" size={15} /><span>{copy.admin.repo.readOnly}</span></p>}
      {canEdit && !data.proposals_configured && <p className="meta-line"><Icon name="info" size={15} /><span>{copy.admin.repo.proposalsOff}</span></p>}
      {canEdit && (
        <Tray
          drafts={drafts}
          configured={data.proposals_configured}
          onOpen={open}
          onRemove={(p) => setDrafts((d) => removeDraft(d, p))}
          onDiscard={() => setDrafts([])}
          onOpened={() => { setDrafts([]); bump(); }}
          bump={bump}
        />
      )}
      <div className={`content-grid ${openable ? 'content-grid-open' : ''}`}>
        <Tree
          data={data}
          drafts={drafts}
          selected={selected}
          onOpen={open}
          canEdit={canEdit}
          onAdd={(p) => {
            setDrafts((d) => upsertDraft(d, { path: p, kind: 'add', text: T.add.starter(fileName(p).replace(/\.[a-z]+$/, '')) }));
            open(p);
          }}
        />
        <div ref={viewRef} className="content-view">
          {openable && selected ? (
            <FileView
              key={selected}
              path={selected}
              listed={listed}
              draft={draftOf}
              canEdit={canEdit}
              onDraft={(d) => setDrafts((all) => upsertDraft(all, d))}
              onUndo={(p) => setDrafts((all) => removeDraft(all, p))}
              onClose={close}
            />
          ) : selected && listed && !listed.readable ? (
            <section className="card"><Empty icon="lock" small>{TR.lockedReason}</Empty></section>
          ) : (
            <section className="card file-placeholder" aria-label={T.file.empty}>
              <Empty icon="file" small>{T.file.empty}</Empty>
            </section>
          )}
        </div>
      </div>
      <HarnessCard />
      <ProposalsCard kind="content" id={PROPOSALS_ID} load={loadProposals} version={version} />
    </>
  );
}

/** Admins and auditors: the content tree and the harness; admins can propose changes to the files. */
export function Content({ search }: { search: string }) {
  const { api } = useApp();
  const load = useCallback(() => api.adminContent(), [api]);
  const { data, error, forbidden, reload } = useLoad(load, T.loadFailed);
  if (forbidden) return <NoAccess />;
  return (
    <div className="page page-wide">
      <AdminHead section="content" title={copy.admin.sections.content} lede={T.lede}>
        {data && <RevisionChip revision={data.revision} />}
      </AdminHead>
      {error && <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>{error}</ErrorNote>}
      {!data && !error && <Skeleton rows={4} label={T.loading} variant="cards" />}
      {data && <ContentBody data={data} search={search} />}
    </div>
  );
}
