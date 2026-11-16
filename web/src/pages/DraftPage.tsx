import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiError, REJECT_REASONS } from '../api';
import type { Draft, RejectReason, Suggestion } from '../api';
import { messageOf, useApp } from '../app-context';
import { ApproveDialog } from '../components/ApproveDialog';
import { ErrorNote, ExtLink, Hint, Markdown, StrippedNote } from '../components/bits';
import { copy } from '../copy';
import { diffLines } from '../diff';
import { isExpired } from '../drafts';
import { renderMarkdown } from '../markdown';

type SaveState = 'saved' | 'saving' | 'error';
type ApproveErrorKey = keyof typeof copy.approve.errors;

export function DraftPage({ id }: { id: string }) {
  const { api, me, refreshMe, pollMs, navigate } = useApp();

  const [draft, setDraft] = useState<Draft | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [saveState, setSaveState] = useState<SaveState>('saved');
  const [editing, setEditing] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);

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
      setSuggestion(await api.getSuggestion(id));
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

  if (loadError) {
    return (
      <div className="stack">
        <a href="#/" className="back">{copy.draft.back}</a>
        <ErrorNote next={undefined} actionLabel={copy.errors.retry} onAction={() => void load()}>
          {loadError}
        </ErrorNote>
      </div>
    );
  }
  if (!draft) return <p className="muted">{copy.draft.loading}</p>;

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
      void refreshMe();
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
    } catch (err) {
      setRejectError(messageOf(err, copy.errors.unknown));
    }
  };

  const remove = async () => {
    setDeleteError(null);
    try {
      await api.deleteDraft(id);
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
  const chipClass = dirty || saveState === 'error' ? 'chip chip-unsaved' : 'chip chip-saved';

  return (
    <>
      <div className="stack">
        <a href="#/" className="back">{copy.draft.back}</a>

        {draft.practice && (
          <div className="note note-practice">
            <p><strong>{copy.draft.practiceLabel}.</strong> {copy.draft.practiceNote}</p>
            {pending && <p>{copy.welcomeNoDrafts}</p>}
            <button type="button" className="btn" onClick={remove}>{copy.draft.practiceDelete}</button>
            {deleteError && <ErrorNote>{deleteError}</ErrorNote>}
          </div>
        )}

        {pending && (
          <form onSubmit={askChange} className="card ask-change" aria-label={copy.draft.askHeading}>
            <h2>{copy.draft.askHeading}</h2>
            <Hint name="ask" />
            <label htmlFor="change-box">{copy.draft.askLabel}</label>
            <div className="ask-row">
              <input
                id="change-box"
                type="text"
                value={instruction}
                placeholder={copy.draft.askPlaceholder}
                onChange={(e) => setInstruction(e.target.value)}
                autoComplete="off"
              />
              <button
                type="submit"
                className="btn"
                disabled={changeBusy || !instruction.trim() || suggestion?.status === 'working'}
              >
                {copy.draft.askButton}
              </button>
            </div>
            {changeError && <ErrorNote>{changeError}</ErrorNote>}
            {suggestion?.status === 'working' && (
              <p role="status" className="muted">{copy.draft.suggestionWorking}</p>
            )}
            {suggestion?.status === 'failed' && (
              <ErrorNote next={copy.draft.suggestionFailedNext} actionLabel={copy.draft.rejectSuggestion} onAction={() => void decideSuggestion(false)}>
                {copy.draft.suggestionFailed(suggestion.error ?? '')}
              </ErrorNote>
            )}
            {suggestion?.status === 'ready' && diff && (
              <div className="suggestion" role="region" aria-label={copy.draft.suggestionHeading}>
                <h3>{copy.draft.suggestionHeading}</h3>
                <p className="muted">{copy.draft.suggestionFor(suggestion.instruction)}</p>
                {changed ? (
                  <pre className="diff" aria-label="Before and after">
                    {diff.map((l, i) => (
                      <div key={i} className={`diff-${l.kind}`}>
                        <span className="diff-tag">
                          {l.kind === 'added' ? copy.draft.diffAdded : l.kind === 'removed' ? copy.draft.diffRemoved : copy.draft.diffSame}
                        </span>
                        {l.text || ' '}
                      </div>
                    ))}
                  </pre>
                ) : (
                  <p>{copy.draft.diffNoChange}</p>
                )}
                <div className="row">
                  <button type="button" className="btn btn-primary" onClick={() => void decideSuggestion(true)}>
                    {copy.draft.accept}
                  </button>
                  <button type="button" className="btn" onClick={() => void decideSuggestion(false)}>
                    {copy.draft.rejectSuggestion}
                  </button>
                </div>
              </div>
            )}
          </form>
        )}

        <div className="workspace">
          <section className="card doc" aria-label="Draft">
            <div className="doc-head">
              <span className="muted">{copy.home.kindLabel(draft.kind)} · {draft.target.label}</span>
              <span className="row">
                {pending && <span role="status" className={chipClass}>{saveChip}</span>}
                {pending && !editing && (
                  <button type="button" className="btn" onClick={startEdit}>
                    {copy.draft.edit}
                  </button>
                )}
              </span>
            </div>
            {editing ? (
              <>
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
                <p className="muted">{copy.draft.editingNote}</p>
                <div className="row save-row">
                  <button type="button" className="btn btn-primary" onClick={() => void save()} disabled={!dirty || saveState === 'saving'}>
                    {copy.draft.save}
                  </button>
                  <button type="button" className="btn" onClick={discard}>
                    {copy.draft.discard}
                  </button>
                </div>
              </>
            ) : (
              <article aria-label={copy.draft.formattedLabel}>
                <h1 className="doc-title">{draft.title}</h1>
                <Markdown rendered={rendered} className="preview" />
                <StrippedNote parts={rendered.stripped} what={copy.stripped.draft} />
              </article>
            )}
            {!pending && !expired && <p className="muted">{copy.draft.readOnly(draft.status)}</p>}
          </section>

          <aside className="card sources" aria-label={copy.draft.sourcesHeading}>
            <h2>{copy.draft.sourcesHeading}</h2>
            <Hint name="sources" />
            {draft.sources.length === 0 ? (
              <p className="muted">{copy.noSources}</p>
            ) : (
              <ul className="list">
                {draft.sources.map((s) => (
                  <li key={s.id}>
                    <button
                      type="button"
                      className={`source ${selected === s.id ? 'source-on' : ''}`}
                      aria-pressed={selected === s.id}
                      onClick={() => setSelected(selected === s.id ? null : s.id)}
                    >
                      <span className="source-sys">{s.system}</span>
                      <span className="source-title">{s.title}</span>
                      {s.excerpt && <span className="source-excerpt">“{s.excerpt}”</span>}
                    </button>
                    {selected === s.id && (
                      <ExtLink href={s.url} className="source-link" label={`: ${s.title}`}>
                        {copy.draft.sourceOpen}
                      </ExtLink>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </aside>
        </div>

        {pending && (
          <section className="card decide" aria-label="Decide">
            <Hint name="approve" />
            {!confirming && approveNote}
            <div className="row">
              <button
                type="button"
                className="btn btn-primary btn-big"
                onClick={openConfirm}
                disabled={blocked !== null}
                aria-describedby={blocked ? 'approve-blocked' : undefined}
              >
                {copy.approve.button(draft.kind)}
              </button>
              <button type="button" className="btn" onClick={() => setRejecting((r) => !r)} aria-expanded={rejecting}>
                {copy.reject.button}
              </button>
            </div>
            {blocked && <p id="approve-blocked" className="muted">{blocked}</p>}

            {rejecting && (
              <fieldset className="reject">
                <legend>{copy.reject.heading}</legend>
                {REJECT_REASONS.map((r) => (
                  <label key={r} className="check">
                    <input type="radio" name="reject-reason" checked={reason === r} onChange={() => setReason(r)} />
                    {copy.reject.reasons[r]}
                  </label>
                ))}
                {rejectError && <ErrorNote>{rejectError}</ErrorNote>}
                <div className="row">
                  <button type="button" className="btn btn-danger" onClick={() => void reject()}>
                    {copy.reject.confirm}
                  </button>
                  <button type="button" className="btn" onClick={() => setRejecting(false)}>
                    {copy.reject.cancel}
                  </button>
                </div>
              </fieldset>
            )}
          </section>
        )}

        {!pending && (
          <section className="card outcome" aria-label="Outcome" role="status">
            {draft.status === 'approved' && draft.practice && <p>{copy.approve.practiceApproved}</p>}
            {draft.status === 'approved' && !draft.practice && (
              <>
                {justApproved && draft.kind === 'confluence_page' && <p><strong>{copy.doneFirstApproval}</strong></p>}
                <p>{copy.approve.approvedWhere(draft.target.label)}</p>
                {draft.result_url ? (
                  <ExtLink href={draft.result_url} className="btn btn-primary">
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
            {draft.practice && <button type="button" className="btn" onClick={remove}>{copy.draft.practiceDelete}</button>}
          </section>
        )}
      </div>

      {confirming && pending && (
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
      )}
    </>
  );
}
