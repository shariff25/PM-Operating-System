// The pieces Policy and Content share: branch protection, a check result (issues, allowance
// changes, file diffs), the pull-request dialog, its result and errors, and the open proposals.
import { useCallback, useEffect, useId, useRef, useState } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { cleanTitle, diffStat, groupChanges, parseUnified, REASON_MAX, TITLE_MAX, wideningsOf } from '../../admin/proposals';
import type { Problem } from '../../admin/proposals';
import type { ProposalFlow } from '../../admin/useProposalFlow';
import { useLoad } from '../../admin/useLoad';
import type { AllowanceChange, FileDiff, Issue, ProposalList, ProposalResult, Protection, Revision } from '../../api';
import { ApiError } from '../../api';
import { Empty, ErrorNote, ExtLink, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { relTime } from '../../time';

const R = copy.admin.repo;
const P = copy.admin.propose;

// ---------- Status strip ----------

/** One tile of a page's status strip: a mono label, a value, and one quiet line under it. */
export function StatusTile({ label, value, sub, tone, text }: { label: string; value: ReactNode; sub: ReactNode; tone?: 'warn' | 'bad' | 'good'; text?: boolean }) {
  return (
    <div className={`stat status-tile ${tone ? `status-${tone}` : ''}`}>
      <span className="stat-label">{label}</span>
      <span className={`stat-value ${text ? 'stat-value-text' : ''}`}>
        {tone && tone !== 'good' && <Icon name="alert" size={text ? 16 : 18} className="status-icon" />}
        {tone === 'good' && <Icon name="check" size={text ? 16 : 18} weight={2.25} className="status-icon" />}
        {value}
      </span>
      <span className="stat-sub">{sub}</span>
    </div>
  );
}

/** The protection tile: which branch, and whether GitHub requires someone else's review. */
export function ProtectionTile({ protection, configured }: { protection: Protection; configured: boolean }) {
  if (!configured || protection.branch === null) return <StatusTile label={R.protection} value={R.off} sub={R.offSub} text />;
  const unprotected = !protection.verified || protection.required_reviews === 0 || protection.required_reviews === null;
  const value = protection.verified && protection.required_reviews !== null
    ? protection.required_reviews === 0 ? R.noReviews : R.reviews(protection.required_reviews)
    : R.unknownReviews;
  return (
    <StatusTile
      label={R.protection}
      value={value}
      sub={<span className="mono-val">{protection.branch}</span>}
      tone={unprotected ? 'warn' : 'good'}
      text
    />
  );
}

/**
 * What an admin must know before trusting review: GitHub not linked (a call to action), the branch
 * not requiring a review, or protection that could not be read. Best effort: never blocks.
 */
export function ProtectionNotes({ protection, configured }: { protection: Protection; configured: boolean }) {
  if (!configured || protection.branch === null) return null;
  const branch = protection.branch;
  const notes: ReactNode[] = [];
  if (protection.github === 'not_linked') {
    notes.push(
      <div key="link" className="note note-cta">
        <Icon name="link" className="note-icon" />
        <div className="note-body">
          <p className="note-title">{R.notLinkedTitle}</p>
          <p>{R.notLinked}</p>
        </div>
      </div>,
    );
  }
  const zero = protection.verified && protection.required_reviews === 0;
  // Not linked: protection could not be read with this viewer's account; the call to link says why.
  const unknown = !protection.verified && protection.github !== 'not_linked';
  if (zero || unknown) {
    notes.push(
      <div key="prot" className="note note-warn">
        <Icon name="alert" className="note-icon" />
        <div className="note-body">
          <p className="note-title">{R.unprotectedTitle}</p>
          <p>{zero ? R.zeroReviews(branch) : protection.github === 'unavailable' ? R.unavailable : R.unverified(branch)}</p>
        </div>
      </div>,
    );
  }
  return notes.length ? <div className="stack stack-tight">{notes}</div> : null;
}

/** The deployed revision, short, with the whole commit for anyone who hovers. */
export function RevisionChip({ revision }: { revision: Revision }) {
  return (
    <span className="rev-chip" title={revision.commit ? `${revision.commit}. ${R.revisionNote}` : R.revisionNote}>
      <span className="rev-label">{R.revision}</span>
      <span className="mono-val">{revision.commit ? revision.commit.slice(0, 7) : R.revisionUnknown}</span>
    </span>
  );
}

// ---------- Issues ----------

export function IssueList({ issues, label, compact }: { issues: readonly Issue[]; label: string; compact?: boolean }) {
  if (issues.length === 0) return null;
  return (
    <ul className={`issues ${compact ? 'issues-compact' : ''}`} aria-label={label}>
      {issues.map((i, n) => (
        <li key={`${i.path}-${i.code}-${n}`} className={`issue issue-${i.severity}`}>
          <Icon name={i.severity === 'error' ? 'alert' : 'info'} size={15} className="issue-icon" />
          <span className="issue-main">
            <span className="issue-msg">{i.message}</span>
            <span className="issue-where">
              <span className="mono-val issue-path">{i.path}</span>
              {i.line !== null && <span className="mono-val">{P.result.line(i.line)}</span>}
              <span className="issue-code mono-val">{i.code}</span>
            </span>
          </span>
          <span className="sr-only">{i.severity === 'error' ? P.result.errors : P.result.warnings}</span>
        </li>
      ))}
    </ul>
  );
}

// ---------- Allowance changes ----------

function ChangeRow({ c }: { c: AllowanceChange }) {
  return (
    <li className={`achange achange-${c.direction}`}>
      <span className="achange-kind">{P.kinds[c.kind] ?? c.kind}</span>
      <span className="achange-main">
        <span className="achange-desc">{c.description}</span>
        {(c.before !== null || c.after !== null) && (
          <span className="achange-vals mono-val">
            {c.before !== null && <del>{c.before}</del>}
            {c.before !== null && c.after !== null && <span aria-hidden="true"> → </span>}
            {c.after !== null && <ins>{c.after}</ins>}
          </span>
        )}
      </span>
      {c.bot && <span className="bot-chip mono-val">{c.bot}</span>}
    </li>
  );
}

/** What a change does to the bots' sandboxes: Wider first, each one spelled out, then Narrower and Changed. */
export function AllowanceChanges({ changes }: { changes: readonly AllowanceChange[] }) {
  const g = groupChanges(changes);
  const groups = [
    { key: 'wider', items: g.wider, title: P.groups.wider, hint: P.groups.widerHint },
    { key: 'changed', items: g.changed, title: P.groups.changed, hint: P.groups.changedHint },
    { key: 'narrower', items: g.narrower, title: P.groups.narrower, hint: P.groups.narrowerHint },
  ].filter((x) => x.items.length > 0);
  if (groups.length === 0) {
    return (
      <p className="quiet-line">
        <Icon name="check" size={15} />
        {P.result.noSandbox}
      </p>
    );
  }
  return (
    <div className="agroups">
      {groups.map((x) => (
        <section key={x.key} className={`agroup agroup-${x.key}`} aria-label={P.groups.label(x.title, x.items.length)}>
          <header className="agroup-head">
            <span className={`dir-tag dir-${x.key}`}>
              {x.key !== 'narrower' && <Icon name="alert" size={12} weight={2.25} />}
              {x.title}
              <span className="dir-count">{x.items.length}</span>
            </span>
            <span className="agroup-hint">{x.hint}</span>
          </header>
          <ul className="achanges">
            {x.items.map((c, i) => (
              <ChangeRow key={`${c.bot}-${c.kind}-${c.item}-${i}`} c={c} />
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

// ---------- Diffs ----------

/** A unified diff, monospaced, with line numbers and the unchanged stretches between hunks folded. */
export function DiffView({ diff, path }: { diff: string; path: string }) {
  const hunks = parseUnified(diff);
  if (hunks.length === 0) return <p className="muted small diff-empty">{P.diff.empty}</p>;
  return (
    <div className="udiff" role="region" tabIndex={0} aria-label={P.diff.label(path)}>
      {hunks.map((h, hi) => (
        <div key={hi} className="udiff-hunk">
          {h.skipped > 0 && <div className="udiff-fold">{P.diff.unchanged(h.skipped)}</div>}
          {h.lines.map((l, li) => (
            <div key={li} className={`udiff-line udiff-${l.kind}`}>
              <span className="udiff-no" aria-hidden="true">{l.old ?? ''}</span>
              <span className="udiff-no" aria-hidden="true">{l.new ?? ''}</span>
              <span className="udiff-sign" aria-hidden="true">{l.kind === 'add' ? '+' : l.kind === 'del' ? '−' : ''}</span>
              <span className="udiff-text">
                {l.kind !== 'ctx' && <span className="sr-only">{l.kind === 'add' ? `${P.diff.added}: ` : `${P.diff.deleted}: `}</span>}
                {l.text || ' '}
              </span>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

function FileDiffRow({ f }: { f: FileDiff }) {
  const s = diffStat(f.diff);
  return (
    <details className="fdiff">
      <summary>
        <Icon name="chevronRight" size={14} className="disclose-icon" />
        <span className={`change-tag change-${f.change}`}>{P.diff[f.change]}</span>
        <span className="mono-val fdiff-path">{f.path}</span>
        <span className="fdiff-stat mono-val" aria-label={P.diff.statLabel(s.added, s.removed)}>
          <span className="stat-add">+{s.added}</span> <span className="stat-del">−{s.removed}</span>
        </span>
      </summary>
      <DiffView diff={f.diff} path={f.path} />
    </details>
  );
}

/** Every file the change writes, content then regenerated policy, collapsed. */
export function FileDiffs({ content, policy }: { content: readonly FileDiff[]; policy: readonly FileDiff[] }) {
  const n = content.length + policy.length;
  if (n === 0) return null;
  return (
    <details className="disclose">
      <summary>
        <Icon name="chevronRight" size={15} className="disclose-icon" />
        <span>{P.result.files(n)}</span>
      </summary>
      <div className="disclose-body">
        {content.length > 0 && (
          <div className="fdiff-group">
            <h4 className="micro-label">{P.result.contentFiles}</h4>
            {content.map((f) => <FileDiffRow key={f.path} f={f} />)}
          </div>
        )}
        {policy.length > 0 && (
          <div className="fdiff-group">
            <h4 className="micro-label">{P.result.policyFiles}</h4>
            {policy.map((f) => <FileDiffRow key={f.path} f={f} />)}
          </div>
        )}
      </div>
    </details>
  );
}

// ---------- Problems ----------

/** A short command to copy, shown in a chip with its own Copy button. */
export function CommandChip({ command, label }: { command: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const copyIt = () => {
    navigator.clipboard?.writeText(command).then(() => setCopied(true), () => {});
  };
  return (
    <div className="cmd-chip" role="group" aria-label={label}>
      <code className="mono-val">{command}</code>
      <button type="button" className="cmd-chip-btn" onClick={copyIt}>
        {copied ? P.copiedCommand : P.copyCommand}
      </button>
    </div>
  );
}

export function ProblemNote({ problem, onCheckProposals }: { problem: Problem; onCheckProposals?: () => void }) {
  const cls = problem.callToAction ? 'note-cta' : problem.checkProposals ? 'note-warn' : 'note-error';
  return (
    <div className={`note ${cls}`} role="alert">
      <Icon name={problem.callToAction ? 'link' : 'alert'} className="note-icon" />
      <div className="note-body">
        <p>{problem.text}</p>
        {problem.next && <p className="note-next">{problem.next}</p>}
        {problem.code === 'github_not_linked' && <CommandChip command={P.linkCommand} label={P.linkCommandLabel} />}
        {problem.paths.length > 0 && (
          <div className="note-paths">
            <span className="micro-label">{P.pathsLabel}</span>
            <ul>
              {problem.paths.map((p) => <li key={p} className="mono-val">{p}</li>)}
            </ul>
            {problem.base && <span className="mono-val muted">{P.baseLine(problem.base.branch, problem.base.commit)}</span>}
          </div>
        )}
        {problem.checkProposals && onCheckProposals && (
          <button type="button" className="btn btn-sm" onClick={onCheckProposals}>{P.checkProposals}</button>
        )}
      </div>
    </div>
  );
}

// ---------- Check result ----------

export function CheckResult({ flow, headingId }: { flow: ProposalFlow; headingId: string }) {
  const c = flow.checked;
  if (!c) return null;
  const r = c.result;
  return (
    <section className="check-result" aria-labelledby={headingId} aria-live="polite">
      <header className="check-head">
        <h3 id={headingId}>{P.result.heading}</h3>
        <span className={`pill ${r.valid ? 'pill-good' : 'pill-bad'}`}>
          <span className="pill-dot" aria-hidden="true" />
          {r.valid ? P.result.passes : P.result.fails(r.errors.length)}
        </span>
        {r.warnings.length > 0 && <span className="pill pill-warn"><span className="pill-dot" aria-hidden="true" />{P.result.warningsCount(r.warnings.length)}</span>}
        <span className="check-sum mono-val">
          {(['wider', 'narrower', 'changed'] as const).map((d) => (
            <span key={d} className={`sum-${d} ${r.summary[d] > 0 ? 'sum-on' : ''}`}>
              {P.groups[d]} {r.summary[d]}
            </span>
          ))}
        </span>
      </header>
      {flow.stale && (
        <div className="note note-warn">
          <Icon name="refresh" className="note-icon" />
          <div className="note-body"><p>{P.stale}</p></div>
        </div>
      )}
      <IssueList issues={r.errors} label={P.result.errors} />
      <IssueList issues={r.warnings} label={P.result.warnings} />
      <div className="check-block">
        <h4 className="micro-label">{P.result.allowances}</h4>
        {r.valid || r.allowance_changes.length > 0 ? <AllowanceChanges changes={r.allowance_changes} /> : <p className="quiet-line">{P.result.noSandboxErrors}</p>}
      </div>
      <FileDiffs content={r.content_changes} policy={r.policy_changes} />
    </section>
  );
}

/** The two buttons under a change, and the one line that says why proposing is not open yet. */
export function ProposeBar({ flow, empty, configured, many, extra }: { flow: ProposalFlow; empty: boolean; configured: boolean; many?: boolean; extra?: ReactNode }) {
  const hint = empty
    ? null
    : flow.checked && !flow.stale && !flow.checked.result.valid
      ? P.fixFirst
      : flow.stale
        ? P.stale
        : !flow.checked
          ? P.needsCheck
          : null;
  return (
    <div className="propose-bar">
      <div className="propose-actions">
        <button type="button" className="btn" disabled={empty || flow.checking} onClick={() => void flow.runCheck()}>
          <Icon name="check" size={15} />
          {flow.checking ? P.checking : many ? P.checkMany : P.check}
        </button>
        {configured && (
          <button type="button" className="btn btn-primary" disabled={!flow.ready} onClick={flow.openDialog}>
            <Icon name="pullRequest" size={15} />
            {P.open}
          </button>
        )}
        {extra}
      </div>
      {hint && configured && <p className="propose-hint">{hint}</p>}
    </div>
  );
}

// ---------- The pull-request dialog ----------

const FOCUSABLE = 'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])';

export function ProposeDialog({
  kind,
  flow,
  onCheckProposals,
}: {
  kind: 'policy' | 'content';
  flow: ProposalFlow;
  onCheckProposals: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const [title, setTitle] = useState('');
  const [reason, setReason] = useState('');
  const id = useId();
  const D = P.dialog;
  const result = flow.checked?.result;
  const widenings = wideningsOf(result?.allowance_changes ?? []);
  const files = (result?.content_changes.length ?? 0) + (result?.policy_changes.length ?? 0);
  const ok = cleanTitle(title).length > 0 && reason.trim().length > 0;

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    titleRef.current?.focus();
    return () => before?.focus?.();
  }, []);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (!flow.submitting) flow.closeDialog();
    } else if (e.key === 'Tab' && box.current) {
      const els = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const first = els[0];
      const last = els[els.length - 1];
      if (!first || !last) return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  return createPortal(
    <div className="modal-backdrop">
      <div ref={box} className="modal confirm propose-dialog" role="dialog" aria-modal="true" aria-labelledby={`${id}-h`} onKeyDown={onKey}>
        <form
          className="modal-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (ok && !flow.submitting) void flow.submit(cleanTitle(title), reason.trim());
          }}
        >
          <div className="modal-scroll">
            <h2 id={`${id}-h`}>{D.heading}</h2>
            <p className="muted">{D.intro}</p>
            <div className="field">
              <div className="field-row">
                <label className="field-label" htmlFor={`${id}-t`}>{D.title}</label>
                <span className="field-count mono-val" aria-hidden="true">{D.titleCount(title.length, TITLE_MAX)}</span>
              </div>
              <input ref={titleRef} id={`${id}-t`} type="text" value={title} maxLength={TITLE_MAX} required aria-describedby={`${id}-tp`} onChange={(e) => setTitle(e.target.value)} />
              <p id={`${id}-tp`} className="field-hint">
                {D.prTitleLabel} <span className="mono-val">{D.prTitle(kind, cleanTitle(title))}</span>
              </p>
            </div>
            <div className="field">
              <div className="field-row">
                <label className="field-label" htmlFor={`${id}-r`}>{D.reason}</label>
                <span className="field-count mono-val" aria-hidden="true">{D.reasonCount(reason.length, REASON_MAX)}</span>
              </div>
              <textarea id={`${id}-r`} rows={4} value={reason} maxLength={REASON_MAX} required aria-describedby={`${id}-rh`} onChange={(e) => setReason(e.target.value)} />
              <p id={`${id}-rh`} className="field-hint">{D.reasonHint}</p>
            </div>
            {widenings.length > 0 ? (
              <div className="widen-confirm" role="group" aria-labelledby={`${id}-w`}>
                <h3 id={`${id}-w`}>
                  <Icon name="alert" size={14} weight={2.25} />
                  {D.widens(widenings.length)}
                </h3>
                <ul className="widen-list">
                  {widenings.map((c, i) => (
                    <li key={`${c.bot}-${c.kind}-${c.item}-${i}`}>
                      <span className="achange-kind">{P.kinds[c.kind] ?? c.kind}</span>
                      <span>{c.description}</span>
                    </li>
                  ))}
                </ul>
                <p className="widen-note">{D.widensNote}</p>
              </div>
            ) : (
              <p className="quiet-line">
                <Icon name="check" size={15} />
                {D.noWidening}
              </p>
            )}
            <p className="muted small">{D.filesLine(files)}</p>
            {flow.submitProblem && <ProblemNote problem={flow.submitProblem} onCheckProposals={onCheckProposals} />}
          </div>
          <div className="modal-actions">
            <button type="submit" className="btn btn-primary" disabled={!ok || flow.submitting}>
              {flow.submitting ? D.submitting : D.submit}
            </button>
            <button type="button" className="btn" disabled={flow.submitting} onClick={flow.closeDialog}>{D.cancel}</button>
            {!ok && <span className="modal-hint">{D.required}</span>}
          </div>
        </form>
      </div>
    </div>,
    document.body,
  );
}

/** After a pull request opened: where it is, who opened it, and what it writes. */
export function ProposalDone({ result, onAnother }: { result: ProposalResult; onAnother: () => void }) {
  const D = P.done;
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => box.current?.focus(), []);
  const widenings = wideningsOf(result.allowance_changes).length;
  return (
    <div ref={box} className="pr-done" tabIndex={-1} role="status">
      <div className="pr-done-head">
        <span className="pr-done-icon"><Icon name="pullRequest" size={18} /></span>
        <div className="pr-done-text">
          <h3>{D.heading(result.number)}</h3>
          <p className="muted">{D.body}</p>
        </div>
        <ExtLink href={result.url} className="btn btn-sm btn-primary pr-done-link" label={` #${result.number}`}>
          {D.open}
          <Icon name="external" size={13} />
        </ExtLink>
      </div>
      <dl className="facts pr-facts">
        <dt>{D.author}</dt>
        <dd className="mono-val">{result.author ?? D.authorUnknown}</dd>
        <dt>{D.branch}</dt>
        <dd className="mono-val">{result.branch}</dd>
        <dt>{D.base}</dt>
        <dd className="mono-val">{result.base.branch} · {result.base.commit.slice(0, 7)}</dd>
        <dt>{D.files}</dt>
        <dd>
          <ul className="fact-lines">
            {result.files.map((f) => <li key={f} className="mono-val">{f}</li>)}
          </ul>
        </dd>
      </dl>
      {widenings > 0 && <p className="pr-widen"><Icon name="alert" size={14} /> {D.widening(widenings)}</p>}
      {!result.labels_added && <p className="muted small">{D.labelsMissing}</p>}
      {result.stale_check === 'skipped' && <p className="muted small">{D.staleSkipped}</p>}
      <div>
        <button type="button" className="btn btn-sm" onClick={onAnother}>{D.another}</button>
      </div>
    </div>
  );
}

// ---------- Open proposals ----------

/** The open pull requests for one kind. `version` going up reloads the list (after a proposal opens). */
export function ProposalsCard({ kind, load, version, id }: { kind: 'policy' | 'content'; load: () => Promise<ProposalList>; version: number; id: string }) {
  const T = copy.admin.proposals;
  const wrapped = useCallback(async (): Promise<ProposalList | 'off'> => {
    try {
      return await load();
    } catch (e) {
      if (e instanceof ApiError && e.code === 'not_configured') return 'off';
      throw e;
    }
  }, [load]);
  const { data, error, loading, reload } = useLoad(wrapped, T.failed);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    reload();
    // reload changes identity each render; the version is what asks for a new load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version]);
  const now = Date.now();
  const list = data && data !== 'off' ? data : null;
  return (
    <section id={id} className="card card-table" aria-labelledby={`${id}-h`} tabIndex={-1}>
      <div className="card-head">
        <h2 id={`${id}-h`}>
          {T.heading}
          {list && list.github === 'linked' && <span className="card-count">{list.proposals.length}</span>}
        </h2>
        <span className="card-head-end">
          {list?.repo && <span className="mono-val muted">{T.repo(list.repo)}</span>}
          <button type="button" className="btn btn-sm btn-ghost" disabled={loading} onClick={reload}>
            <Icon name="refresh" size={14} />
            {T.reload}
          </button>
        </span>
      </div>
      {error && <div className="card-body"><ErrorNote actionLabel={copy.errors.retry} onAction={reload}>{error}</ErrorNote></div>}
      {!data && !error && <div className="card-body"><Skeleton rows={2} label={T.loading} /></div>}
      {data === 'off' && <Empty icon="pullRequest" small>{T.off}</Empty>}
      {list && list.github === 'not_linked' && (
        <Empty icon="link" small>{list.message ?? T.notLinked}</Empty>
      )}
      {list && list.github === 'linked' && list.proposals.length === 0 && <Empty icon="pullRequest" small>{T.empty}</Empty>}
      {list && list.github === 'linked' && list.proposals.length > 0 && (
        <ul className="rows" aria-label={T.tableLabel(kind)}>
          {list.proposals.map((p) => (
            <li key={p.number} className="row pr-row">
              <span className="pr-num mono-val">#{p.number}</span>
              <span className="row-main">
                <ExtLink href={p.url} className="row-title pr-title" label={T.openIn(p.number)}>{p.title}</ExtLink>
                <span className="row-meta">
                  <span className="mono-val pr-branch" title={p.branch}>{p.branch}</span>
                </span>
              </span>
              <span className="pr-by">
                <span className="pr-who">{p.proposed_by ?? T.unknownProposer}</span>
                {p.author && <span className="pr-gh mono-val">{T.viaGithub(p.author)}</span>}
              </span>
              {p.status === 'draft' ? (
                <span className="pill"><span className="pill-dot" aria-hidden="true" />{T.draft}</span>
              ) : (
                <span className="pill pill-accent"><span className="pill-dot" aria-hidden="true" />{T.open}</span>
              )}
              <time className="row-when" dateTime={p.created_at ?? undefined}>{p.created_at ? relTime(p.created_at, now) : ''}</time>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Move to the proposals list after an outcome that is not known, and load it again. */
export function useCheckProposals(id: string, bump: () => void, close: () => void) {
  return useCallback(() => {
    close();
    bump();
    // The list may be on a part of the page that is only now being shown: wait a few frames for it.
    const go = (tries: number) => requestAnimationFrame(() => {
      const el = document.getElementById(id);
      if ((!el || el.closest('[hidden]')) && tries > 0) {
        go(tries - 1);
        return;
      }
      el?.scrollIntoView?.({ block: 'start', behavior: 'smooth' });
      el?.focus({ preventScroll: true });
    });
    go(20);
  }, [id, bump, close]);
}
