import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { ApiError, REJECT_REASONS } from '../api';
import type { Draft, RejectReason, Suggestion } from '../api';
import { messageOf, useApp } from '../app-context';
import { ApproveDialog } from '../components/ApproveDialog';
import { ErrorNote, ExtLink, Hint, HintAnchor, Markdown, Skeleton, StrippedNote } from '../components/bits';
import { Icon, kindIcon, systemIcon } from '../components/icons';
import { TopBarActions, TopBarLead, useShell } from '../components/Shell';
import { useToast } from '../components/Toasts';
import { copy } from '../copy';
import { useData } from '../data';
import { diffLines } from '../diff';
import type { DiffLine } from '../diff';
import { isExpired } from '../drafts';
import { renderMarkdown } from '../markdown';
import { longTime, relTime, timeLeft } from '../time';
import { taskHref } from '../url';
import { tabKeys } from './Drafts';

type SaveState = 'saved' | 'saving' | 'error';
type ApproveErrorKey = keyof typeof copy.approve.errors;
type PanelTab = 'doc' | 'sources' | 'ask' | 'details';

/** Changed lines with one line of context; long unchanged runs fold into a count. */
function BeforeAfter({ diff }: { diff: DiffLine[] }) {
  const changedAt = (i: number) => diff[i] !== undefined && diff[i]!.kind !== 'same';
  const keep = diff.map((_, i) => changedAt(i) || changedAt(i - 1) || changedAt(i + 1));
  const out: ReactNode[] = [];
  let folded = 0;
  const flush = (key: string) => {
    if (folded > 0) out.push(<li key={key} className="ba-fold">{copy.panel.unchanged(folded)}</li>);
    folded = 0;
  };
  diff.forEach((l, i) => {
    if (!keep[i]) {
      folded++;
      return;
    }
    flush(`f${i}`);
    // Blank lines carry no words to read; the lines around them show the change.
    if (!l.text.trim()) return;
    out.push(
      <li key={i} className={`ba-line ba-${l.kind}`}>
        <span className={l.kind === 'same' ? 'sr-only' : 'ba-tag'}>
          {l.kind === 'added' ? copy.draft.diffAdded : l.kind === 'removed' ? copy.draft.diffRemoved : copy.draft.diffSame}
        </span>
        <span className="ba-text">{l.text || ' '}</span>
      </li>,
    );
  });
  flush('f-end');
  return (
    <ol className="before-after" aria-label="Before and after">
      {out}
    </ol>
  );
}

export function DraftPage({ id }: { id: string }) {
  const { api, me, refreshMe, pollMs, navigate } = useApp();
  const { reloadDrafts } = useData();
  const { isPhone } = useShell();
  const toast = useToast();

  const [draft, setDraft] = useState<Draft | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [saveState, setSaveState] = useState<SaveState>('saved');
  const [editing, setEditing] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [tab, setTab] = useState<PanelTab>('sources');
  const tabRefs = useRef<Partial<Record<PanelTab, HTMLButtonElement | null>>>({});

  const [instruction, setInstruction] = useState('');
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null);
  const [changeError, setChangeError] = useState<string | null>(null);
  const [changeBusy, setChangeBusy] = useState(false);

  const [confirming, setConfirming] = useState(false);
  const [forceExternal, setForceExternal] = useState(false);
  const [externalOk, setExternalOk] = useState(false);
  const [approving, setApproving] = useState(false);
  const [approveError, setApproveError] = useState<{ key: ApproveErrorKey; message: string } | null>(null);
  const [justApproved, setJustApproved] = useState(false);

  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState<RejectReason | ''>('');
  const [rejectError, setRejectError] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const rejectBox = useRef<HTMLDivElement>(null);

  // What the PM reads is the saved text. Links and stripped parts come from the same rendering.
  const rendered = useMemo(() => renderMarkdown(draft?.body ?? ''), [draft?.body]);

  const adopt = useCallback((d: Draft) => {
    setDraft(d);
    setTitle(d.title);
    setBody(d.body);
    setSaveState('saved');
    setEditing(false);
  }, []);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const d = await api.getDraft(id);
      adopt(d);
      await refreshMe();
      const s = await api.getSuggestion(id);
      setSuggestion(s);
      // An open suggestion from an earlier visit needs a decision: show it.
      if (s && (s.status === 'working' || s.status === 'ready')) setTab('ask');
    } catch (e) {
      setLoadError(
        e instanceof ApiError && e.code === 'not_found' ? copy.errors.notFound : messageOf(e, copy.errors.loadDraftFailed),
      );
    }
  }, [api, id, adopt, refreshMe]);

  useEffect(() => {
    setDraft(null);
    setConfirming(false);
    setRejecting(false);
    setApproveError(null);
    setJustApproved(false);
    void load();
  }, [load]);

  // Poll a working suggestion.
  useEffect(() => {
    if (suggestion?.status !== 'working') return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const next = await api.getSuggestion(id);
        if (!cancelled) setSuggestion(next);
      } catch (e) {
        if (!cancelled) setChangeError(messageOf(e, copy.errors.unknown));
      }
    }, pollMs);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [suggestion, api, id, pollMs]);

  // The reject popover closes on Escape and on a click outside it.
  useEffect(() => {
    if (!rejecting) return;
    const away = (e: MouseEvent) => {
      if (rejectBox.current && !rejectBox.current.contains(e.target as Node)) setRejecting(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [rejecting]);

  // On phone the document is a tab; on desktop it is always shown.
  useEffect(() => {
    if (!isPhone && tab === 'doc') setTab('sources');
  }, [isPhone, tab]);

  const crumb = (
    <nav aria-label={copy.panel.breadcrumb} className="crumbs">
      <a href="#/drafts">{copy.nav.drafts}</a>
      <Icon name="chevronRight" size={14} className="crumb-sep" />
      <span className="crumb-current" aria-current="page">{draft?.title ?? '…'}</span>
    </nav>
  );

  if (loadError) {
    return (
      <div className="page page-narrow">
        <TopBarLead>{crumb}</TopBarLead>
        <a href="#/" className="back">
          <Icon name="arrowLeft" size={16} />
          {copy.draft.back}
        </a>
        <ErrorNote next={undefined} actionLabel={copy.errors.retry} onAction={() => void load()}>
          {loadError}
        </ErrorNote>
      </div>
    );
  }
  if (!draft) {
    return (
      <div className="workspace">
        <TopBarLead>{crumb}</TopBarLead>
        <div className="doc-col">
          <div className="doc">
            <Skeleton rows={4} label={copy.draft.loading} variant="doc" />
          </div>
        </div>
      </div>
    );
  }

  const expired = isExpired(draft);
  const pending = draft.status === 'pending' && !expired;
  const dirty = title !== draft.title || body !== draft.body;
  const unresolved = suggestion?.status === 'working' || suggestion?.status === 'ready';
  // The PM must have seen the formatted, saved text, with no suggestion left hanging.
  const blocked = editing || dirty ? copy.draft.saveFirst : unresolved ? copy.draft.resolveFirst : null;

  const startEdit = () => {
    setTitle(draft.title);
    setBody(draft.body);
    setEditing(true);
    if (isPhone) setTab('doc');
  };

  const discard = () => {
    setTitle(draft.title);
    setBody(draft.body);
    setSaveState('saved');
    setEditing(false);
  };

  const save = async (): Promise<boolean> => {
    setSaveState('saving');
    try {
      const d = await api.saveDraft(id, { title, body });
      adopt(d);
      toast(copy.toast.saved, 'good');
      void reloadDrafts();
      return true;
    } catch {
      setSaveState('error');
      return false;
    }
  };

  const askChange = async (e: FormEvent) => {
    e.preventDefault();
    const text = instruction.trim();
    if (!text) return;
    setChangeError(null);
    setChangeBusy(true);
    try {
      if (dirty && !(await save())) return;
      setSuggestion(await api.requestChange(id, text));
      setInstruction('');
    } catch (err) {
      setChangeError(
        err instanceof ApiError && err.code === 'change_in_progress'
          ? copy.draft.changeInProgress
          : messageOf(err, copy.errors.unknown),
      );
    } finally {
      setChangeBusy(false);
    }
  };

  const decideSuggestion = async (accept: boolean) => {
    setChangeError(null);
    try {
      const d = accept ? await api.acceptSuggestion(id) : await api.rejectSuggestion(id);
      if (accept) adopt(d);
      else setDraft(d);
      setSuggestion(null);
      if (accept) toast(copy.toast.accepted, 'good');
    } catch (err) {
      setChangeError(messageOf(err, copy.errors.unknown));
    }
  };

  const openConfirm = () => {
    if (blocked) return;
    setApproveError(null);
    setExternalOk(false);
    setConfirming(true);
  };

  const approve = async () => {
    if (blocked) return;
    setApproving(true);
    setApproveError(null);
    try {
      const d = await api.approve(id, { confirmExternal: externalOk, version: draft.version });
      setDraft(d);
      setConfirming(false);
      setJustApproved(true);
      toast(d.practice ? copy.toast.practiceApproved : copy.toast.approved, 'good');
      void refreshMe();
      void reloadDrafts();
    } catch (err) {
      if (err instanceof ApiError) {
        const known: ApproveErrorKey[] = ['needs_external_confirm', 'audience_blocked', 'expired', 'not_pending', 'stale'];
        const key = known.find((k) => k === err.code) ?? 'other';
        if (key === 'needs_external_confirm') setForceExternal(true);
        if (key === 'expired' || key === 'not_pending' || key === 'stale') {
          // The draft changed under us: show what is really there, and read it again.
          try {
            adopt(await api.getDraft(id));
            setSuggestion(await api.getSuggestion(id));
          } catch {
            /* keep what we have */
          }
        }
        if (key === 'stale') setConfirming(false);
        if (isPhone) setTab('doc');
        setApproveError({ key, message: key === 'stale' ? '' : err.message });
      } else {
        setApproveError({ key: 'other', message: copy.errors.unknown });
      }
    } finally {
      setApproving(false);
    }
  };

  const reject = async () => {
    setRejectError(null);
    try {
      setDraft(await api.reject(id, reason || undefined));
      setRejecting(false);
      toast(copy.toast.rejected);
      void reloadDrafts();
    } catch (err) {
      setRejectError(messageOf(err, copy.errors.unknown));
    }
  };

  const remove = async () => {
    setDeleteError(null);
    try {
      await api.deleteDraft(id);
      void reloadDrafts();
      navigate('/');
    } catch (err) {
      setDeleteError(messageOf(err, copy.errors.unknown));
    }
  };

  const showExternal = draft.external || forceExternal;
  const confirmLabel = showExternal
    ? copy.approve.confirmExternalButton
    : draft.kind.startsWith('slack')
      ? copy.approve.confirmSlack
      : copy.approve.confirm;
  const diff = suggestion?.status === 'ready' && suggestion.body !== null ? diffLines(draft.body, suggestion.body) : null;
  const changed = diff?.some((l) => l.kind !== 'same') ?? false;
  const nextHelp = approveError ? copy.approve.errors[approveError.key] : null;

  const approveNote =
    approveError && nextHelp ? (
      <ErrorNote
        next={nextHelp.next}
        actionLabel={
          approveError.key === 'expired'
            ? copy.approve.goHome
            : approveError.key === 'not_pending'
              ? copy.approve.reload
              : undefined
        }
        onAction={() => (approveError.key === 'expired' ? navigate('/') : void load())}
      >
        {approveError.message || nextHelp.text}
      </ErrorNote>
    ) : null;

  const saveChip =
    saveState === 'saving'
      ? copy.draft.saving
      : saveState === 'error'
        ? copy.draft.saveFailed
        : dirty
          ? copy.draft.unsaved
          : copy.draft.saved;
  const chipTone = dirty || saveState === 'error' ? 'unsaved' : 'saved';
  const left = pending ? timeLeft(draft.expires_at) : null;

  // ---- The header actions: save state, Edit, Reject, and the one primary action. ----
  const actions = pending ? (
    <div className="draft-actions">
      <span role="status" className={`save-state save-${chipTone}`}>
        <span className="save-dot" aria-hidden="true" />
        {saveChip}
      </span>
      {!editing && (
        <button type="button" className="btn" onClick={startEdit}>
          <Icon name="edit" size={16} />
          {copy.draft.edit}
        </button>
      )}
      <div className="popover-anchor" ref={rejectBox}>
        <button
          type="button"
          className="btn"
          onClick={() => setRejecting((r) => !r)}
          aria-expanded={rejecting}
          aria-controls={rejecting ? 'reject-pop' : undefined}
        >
          {isPhone ? (
            <>
              <Icon name="x" size={18} />
              <span className="sr-only">{copy.reject.button}</span>
            </>
          ) : (
            copy.reject.button
          )}
        </button>
        {rejecting && (
          <div
            id="reject-pop"
            className="popover"
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.preventDefault();
                setRejecting(false);
              }
            }}
          >
            <fieldset className="reject">
              <legend>{copy.reject.heading}</legend>
              {REJECT_REASONS.map((r) => (
                <label key={r} className="radio">
                  <input type="radio" name="reject-reason" checked={reason === r} onChange={() => setReason(r)} />
                  {copy.reject.reasons[r]}
                </label>
              ))}
            </fieldset>
            {rejectError && <ErrorNote>{rejectError}</ErrorNote>}
            <div className="popover-foot">
              <button type="button" className="btn btn-ghost" onClick={() => setRejecting(false)}>
                {copy.reject.cancel}
              </button>
              <button type="button" className="btn btn-danger" onClick={() => void reject()}>
                {copy.reject.confirm}
              </button>
            </div>
          </div>
        )}
      </div>
      <HintAnchor>
        <button
          type="button"
          className="btn btn-primary"
          onClick={openConfirm}
          disabled={blocked !== null}
          aria-describedby={blocked ? 'approve-blocked' : undefined}
        >
          <Icon name="check" size={16} />
          {copy.approve.button(draft.kind)}
        </button>
        <Hint name="approve" align="end" />
      </HintAnchor>
    </div>
  ) : null;

  // ---- The panel: sources, ask for a change, details. ----
  const tabs: { key: PanelTab; label: string; count?: number }[] = [
    ...(isPhone ? [{ key: 'doc' as const, label: copy.panel.doc }] : []),
    { key: 'sources', label: copy.panel.sources, count: draft.sources.length },
    ...(pending ? [{ key: 'ask' as const, label: copy.panel.ask }] : []),
    { key: 'details', label: copy.panel.details },
  ];
  const activeTab = tabs.some((t) => t.key === tab) ? tab : tabs[0]!.key;
  const pickTab = (t: PanelTab) => {
    setTab(t);
    tabRefs.current[t]?.focus();
  };

  const tabList = (
    <div
      className="tabs panel-tabs"
      role="tablist"
      aria-label={copy.panel.tabsLabel}
      onKeyDown={(e) => tabKeys(e, tabs.map((t) => t.key), activeTab, pickTab)}
    >
      {tabs.map((t) => {
        const button = (
          <button
            key={t.key}
            ref={(el) => { tabRefs.current[t.key] = el; }}
            type="button"
            role="tab"
            id={`ptab-${t.key}`}
            aria-selected={activeTab === t.key}
            aria-controls={`ppanel-${t.key}`}
            tabIndex={activeTab === t.key ? 0 : -1}
            className="tab"
            onClick={() => setTab(t.key)}
          >
            {t.label}
            {t.count !== undefined && <span className="tab-count">{t.count}</span>}
          </button>
        );
        if (t.key === 'sources' || t.key === 'ask') {
          return (
            <HintAnchor key={t.key}>
              {button}
              <Hint name={t.key} align={isPhone || t.key === 'sources' ? 'start' : 'end'} />
            </HintAnchor>
          );
        }
        return button;
      })}
    </div>
  );

  const sourcesPanel = draft.sources.length === 0 ? (
    <div className="empty empty-sm">
      <span className="empty-icon"><Icon name="link" size={18} /></span>
      <p>{copy.noSources}</p>
    </div>
  ) : (
    <ul className="sources">
      {draft.sources.map((s) => (
        <li key={s.id} className={`source-item ${selected === s.id ? 'source-item-on' : ''}`}>
          <button
            type="button"
            className="source"
            aria-pressed={selected === s.id}
            onClick={() => setSelected(selected === s.id ? null : s.id)}
          >
            <span className="source-icon"><Icon name={systemIcon(s.system)} size={16} /></span>
            <span className="source-text">
              <span className="source-sys">{s.system}</span>
              <span className="source-title">{s.title}</span>
              {s.excerpt && <span className="source-excerpt">“{s.excerpt}”</span>}
            </span>
          </button>
          {selected === s.id && (
            <ExtLink href={s.url} className="source-link" label={`: ${s.title}`}>
              {copy.draft.sourceOpen}
              <Icon name="external" size={13} />
            </ExtLink>
          )}
        </li>
      ))}
    </ul>
  );

  const askPanel = (
    <div className="ask-panel">
      <form onSubmit={askChange} className="change-form" aria-label={copy.draft.askHeading}>
        <h2 className="panel-title">{copy.draft.askHeading}</h2>
        <label htmlFor="change-box" className="field-label">{copy.draft.askLabel}</label>
        <textarea
          id="change-box"
          rows={3}
          value={instruction}
          placeholder={copy.draft.askPlaceholder}
          onChange={(e) => setInstruction(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              e.currentTarget.form?.requestSubmit();
            }
          }}
          autoComplete="off"
        />
        <button
          type="submit"
          className="btn btn-block"
          disabled={changeBusy || !instruction.trim() || suggestion?.status === 'working'}
        >
          <Icon name="sparkle" size={16} />
          {copy.draft.askButton}
        </button>
      </form>
      {changeError && <ErrorNote>{changeError}</ErrorNote>}
      {suggestion?.status === 'working' && (
        <p role="status" className="live-line live-line-on">
          <span className="live-dot" aria-hidden="true" />
          {copy.draft.suggestionWorking}
        </p>
      )}
      {suggestion?.status === 'failed' && (
        <ErrorNote next={copy.draft.suggestionFailedNext} actionLabel={copy.draft.rejectSuggestion} onAction={() => void decideSuggestion(false)}>
          {copy.draft.suggestionFailed(suggestion.error ?? '')}
        </ErrorNote>
      )}
      {suggestion?.status === 'ready' && diff && (
        <div className="suggestion" role="region" aria-label={copy.draft.suggestionHeading}>
          <h3>{copy.draft.suggestionHeading}</h3>
          <p className="suggestion-for">{copy.draft.suggestionFor(suggestion.instruction)}</p>
          {changed ? <BeforeAfter diff={diff} /> : <p>{copy.draft.diffNoChange}</p>}
          <div className="suggestion-foot">
            <button type="button" className="btn btn-ghost" onClick={() => void decideSuggestion(false)}>
              {copy.draft.rejectSuggestion}
            </button>
            <button type="button" className="btn btn-primary" onClick={() => void decideSuggestion(true)}>
              {copy.draft.accept}
            </button>
          </div>
        </div>
      )}
    </div>
  );

  const detailsPanel = (
    <dl className="details">
      <dt>{copy.panel.kind}</dt>
      <dd>{copy.home.kindLabel(draft.kind)}</dd>
      <dt>{copy.panel.target}</dt>
      <dd>{draft.target.label}</dd>
      <dt>{copy.panel.bot}</dt>
      <dd>{draft.bot}</dd>
      <dt>{copy.panel.created}</dt>
      <dd>{longTime(draft.created_at)} <span className="muted">· {relTime(draft.created_at)}</span></dd>
      <dt>{copy.panel.expires}</dt>
      <dd>{draft.expires_at ? <>{longTime(draft.expires_at)}{left && <span className="muted"> · {left}</span>}</> : copy.panel.never}</dd>
      <dt>{copy.panel.id}</dt>
      <dd><code className="mono-id">{draft.id}</code></dd>
      <dt>{copy.panel.task}</dt>
      <dd>{draft.task_id ? <a href={taskHref(draft.task_id)}>{copy.panel.openTask}</a> : <span className="muted">{copy.panel.noTask}</span>}</dd>
    </dl>
  );

  const panelBody = { doc: null, sources: sourcesPanel, ask: askPanel, details: detailsPanel }[activeTab];

  // ---- The document column ----
  const docColumn = (
    <div className="doc-col">
      {draft.practice && (
        <div className="banner banner-practice">
          <Icon name="lightbulb" className="banner-icon" />
          <div className="banner-body">
            <p><strong>{copy.draft.practiceLabel}.</strong> {copy.draft.practiceNote}</p>
            {pending && <p>{copy.welcomeNoDrafts}</p>}
            <button type="button" className="btn btn-sm" onClick={remove}>{copy.draft.practiceDelete}</button>
            {deleteError && <ErrorNote>{deleteError}</ErrorNote>}
          </div>
        </div>
      )}

      {pending && !confirming && approveNote}
      {pending && blocked && (
        <p id="approve-blocked" className="blocked-line">
          <Icon name="info" size={16} />
          {blocked}
        </p>
      )}

      {!pending && (
        <section className={`banner banner-${draft.status === 'approved' ? 'good' : 'neutral'} outcome`} aria-label="Outcome" role="status">
          <Icon name={draft.status === 'approved' ? 'check' : 'info'} className="banner-icon" />
          <div className="banner-body">
            {draft.status === 'approved' && draft.practice && <p>{copy.approve.practiceApproved}</p>}
            {draft.status === 'approved' && !draft.practice && (
              <>
                {justApproved && draft.kind === 'confluence_page' && <p><strong>{copy.doneFirstApproval}</strong></p>}
                <p>{copy.approve.approvedWhere(draft.target.label)}</p>
                {draft.result_url ? (
                  <ExtLink href={draft.result_url} className="btn btn-sm btn-primary">
                    {copy.approve.resultLink}
                  </ExtLink>
                ) : (
                  <p>{copy.approve.approvedNoLink}</p>
                )}
              </>
            )}
            {approveNote}
            {draft.status === 'rejected' && <p>{copy.approve.rejectedNote}</p>}
            {expired && <p>{copy.approve.expiredNote}</p>}
            {draft.status === 'failed' && <p>{copy.approve.failedNote}</p>}
            {draft.practice && <button type="button" className="btn btn-sm" onClick={remove}>{copy.draft.practiceDelete}</button>}
          </div>
        </section>
      )}

      <section className="doc" aria-label="Draft">
        <div className="doc-meta">
          <span className="doc-kind"><Icon name={kindIcon(draft.kind)} size={16} />{copy.home.kindLabel(draft.kind)}</span>
          <span className="doc-target">{draft.target.label}</span>
          {draft.external && <span className="tag tag-warn">{copy.panel.external}</span>}
          {left && <span className="doc-left"><Icon name="clock" size={14} />{left}</span>}
          {draft.edited && <span className="doc-edited">{copy.panel.edited}</span>}
        </div>
        {editing ? (
          <div className="editor">
            <label htmlFor="draft-title" className="sr-only">{copy.draft.titleLabel}</label>
            <input
              id="draft-title"
              className="title-input"
              value={title}
              onChange={(e) => { setTitle(e.target.value); setSaveState('saved'); }}
            />
            <label htmlFor="draft-body" className="sr-only">{copy.draft.bodyLabel}</label>
            <textarea
              id="draft-body"
              className="body-input"
              value={body}
              onChange={(e) => { setBody(e.target.value); setSaveState('saved'); }}
              rows={18}
            />
            <div className="editor-foot">
              <p className="muted small">{copy.draft.editingNote}</p>
              <div className="editor-buttons">
                <button type="button" className="btn btn-ghost" onClick={discard}>
                  {copy.draft.discard}
                </button>
                <button type="button" className="btn btn-primary" onClick={() => void save()} disabled={!dirty || saveState === 'saving'}>
                  {copy.draft.save}
                </button>
              </div>
            </div>
          </div>
        ) : (
          <article aria-label={copy.draft.formattedLabel}>
            <h1 className="doc-title">{draft.title}</h1>
            <Markdown rendered={rendered} className="prose" />
            <StrippedNote parts={rendered.stripped} what={copy.stripped.draft} />
          </article>
        )}
        {!pending && !expired && <p className="muted small read-only">{copy.draft.readOnly(draft.status)}</p>}
      </section>
    </div>
  );

  const dialog =
    confirming && pending ? (
      <ApproveDialog
        draft={draft}
        rendered={rendered}
        name={me.name}
        showExternal={showExternal}
        externalOk={externalOk}
        onExternalOk={setExternalOk}
        approving={approving}
        confirmLabel={confirmLabel}
        note={approveNote}
        onConfirm={() => void approve()}
        onCancel={() => setConfirming(false)}
      />
    ) : null;

  if (isPhone) {
    return (
      <>
        <TopBarLead>{crumb}</TopBarLead>
        <div className="workspace workspace-phone">
          {actions && <div className="phone-actions">{actions}</div>}
          {tabList}
          {activeTab === 'doc' ? (
            <div role="tabpanel" id="ppanel-doc" aria-labelledby="ptab-doc">{docColumn}</div>
          ) : (
            <div role="tabpanel" id={`ppanel-${activeTab}`} aria-labelledby={`ptab-${activeTab}`} className="panel-body">
              {panelBody}
            </div>
          )}
        </div>
        {dialog}
      </>
    );
  }

  return (
    <>
      <TopBarLead>{crumb}</TopBarLead>
      <TopBarActions>{actions}</TopBarActions>
      <div className="workspace">
        {docColumn}
        <aside className="panel" aria-label={copy.panel.label}>
          {tabList}
          <div role="tabpanel" id={`ppanel-${activeTab}`} aria-labelledby={`ptab-${activeTab}`} className="panel-body">
            {panelBody}
          </div>
        </aside>
      </div>
      {dialog}
    </>
  );
}
