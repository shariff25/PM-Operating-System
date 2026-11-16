import { useCallback, useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiError, REJECT_REASONS } from '../api';
import type { Draft, RejectReason, Suggestion } from '../api';
import { messageOf, useApp } from '../app-context';
import { ErrorNote, Hint, Markdown } from '../components/bits';
import { copy } from '../copy';
import { diffLines } from '../diff';

type SaveState = 'saved' | 'saving' | 'error';
type ApproveErrorKey = keyof typeof copy.approve.errors;

export function DraftPage({ id }: { id: string }) {
  const { api, me, refreshMe, pollMs, navigate } = useApp();

  const [draft, setDraft] = useState<Draft | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [saveState, setSaveState] = useState<SaveState>('saved');
  const [tab, setTab] = useState<'edit' | 'preview'>('edit');
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

  const adopt = useCallback((d: Draft) => {
    setDraft(d);
    setTitle(d.title);
    setBody(d.body);
    setSaveState('saved');
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
        <a href="#/">{copy.draft.back}</a>
        <ErrorNote next={undefined} actionLabel={copy.errors.retry} onAction={() => void load()}>
          {loadError}
        </ErrorNote>
      </div>
    );
  }
  if (!draft) return <p className="muted">{copy.draft.loading}</p>;

  const pending = draft.status === 'pending';
  const dirty = title !== draft.title || body !== draft.body;

  const save = async (): Promise<boolean> => {
    setSaveState('saving');
    try {
      const d = await api.saveDraft(id, { title, body });
      setDraft(d);
      setTitle(d.title);
      setBody(d.body);
      setSaveState('saved');
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
    setApproveError(null);
    setExternalOk(false);
    setConfirming(true);
  };

  const approve = async () => {
    setApproving(true);
    setApproveError(null);
    try {
      if (dirty && !(await save())) {
        setApproveError({ key: 'other', message: copy.draft.saveFailed });
        return;
      }
      const d = await api.approve(id, { confirmExternal: externalOk });
      setDraft(d);
      setConfirming(false);
      setJustApproved(true);
      void refreshMe();
    } catch (err) {
      if (err instanceof ApiError) {
        const known: ApproveErrorKey[] = ['needs_external_confirm', 'audience_blocked', 'expired', 'not_pending'];
        const key = known.find((k) => k === err.code) ?? 'other';
        if (key === 'needs_external_confirm') setForceExternal(true);
        if (key === 'expired' || key === 'not_pending') {
          // The draft changed state under us; show the real state behind the message.
          try {
            setDraft(await api.getDraft(id));
          } catch {
            /* keep what we have */
          }
        }
        setApproveError({ key, message: err.message });
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

  return (
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
              <button type="button" className={`tab ${tab === 'edit' ? 'tab-on' : ''}`} aria-pressed={tab === 'edit'} onClick={() => setTab('edit')}>
                {copy.draft.editTab}
              </button>
              <button type="button" className={`tab ${tab === 'preview' ? 'tab-on' : ''}`} aria-pressed={tab === 'preview'} onClick={() => setTab('preview')}>
                {copy.draft.previewTab}
              </button>
            </span>
          </div>
          <label htmlFor="draft-title" className="sr-only">{copy.draft.titleLabel}</label>
          <input
            id="draft-title"
            className="title-input"
            value={title}
            readOnly={!pending}
            onChange={(e) => { setTitle(e.target.value); setSaveState('saved'); }}
          />
          {tab === 'edit' ? (
            <>
              <label htmlFor="draft-body" className="sr-only">{copy.draft.bodyLabel}</label>
              <textarea
                id="draft-body"
                className="body-input"
                value={body}
                readOnly={!pending}
                onChange={(e) => { setBody(e.target.value); setSaveState('saved'); }}
                rows={18}
              />
            </>
          ) : (
            <Markdown text={body} className="preview" />
          )}
          {pending ? (
            <div className="row save-row">
              <button type="button" className="btn" onClick={() => void save()} disabled={!dirty || saveState === 'saving'}>
                {copy.draft.save}
              </button>
              <span role="status" className={dirty || saveState === 'error' ? 'state-unsaved' : 'muted'}>
                {saveState === 'saving'
                  ? copy.draft.saving
                  : saveState === 'error'
                    ? copy.draft.saveFailed
                    : dirty
                      ? copy.draft.unsaved
                      : copy.draft.saved}
              </span>
            </div>
          ) : (
            <p className="muted">{copy.draft.readOnly(draft.status)}</p>
          )}
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
                    <a href={s.url} target="_blank" rel="noopener noreferrer" className="source-link">
                      {copy.draft.sourceOpen}<span className="sr-only">: {s.title}</span>
                    </a>
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
          {!confirming && (
            <div className="row">
              <button type="button" className="btn btn-primary btn-big" onClick={openConfirm}>
                {copy.approve.button(draft.kind)}
              </button>
              <button type="button" className="btn" onClick={() => setRejecting((r) => !r)} aria-expanded={rejecting}>
                {copy.reject.button}
              </button>
            </div>
          )}

          {confirming && (
            <div className="confirm" role="group" aria-label={copy.approve.confirmHeading}>
              <h2>{copy.approve.confirmHeading}</h2>
              <p>{copy.approve.confirmText(draft.kind, draft.target.label, me.name)}</p>
              <p>{draft.practice ? copy.approve.confirmPractice : copy.approve.confirmNothing}</p>
              {showExternal && (
                <div className="note note-warn">
                  <p>{copy.approve.externalWarning}</p>
                  <label className="check">
                    <input type="checkbox" checked={externalOk} onChange={(e) => setExternalOk(e.target.checked)} />
                    {copy.approve.externalCheckbox}
                  </label>
                </div>
              )}
              {approveNote}
              <div className="row">
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={() => void approve()}
                  disabled={approving || (showExternal && !externalOk)}
                >
                  {approving ? copy.approve.confirming : confirmLabel}
                </button>
                <button type="button" className="btn" onClick={() => setConfirming(false)}>
                  {copy.approve.cancel}
                </button>
              </div>
            </div>
          )}

          {rejecting && !confirming && (
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
                <a className="btn btn-primary" href={draft.result_url} target="_blank" rel="noopener noreferrer">
                  {copy.approve.resultLink}
                </a>
              ) : (
                <p>{copy.approve.approvedNoLink}</p>
              )}
            </>
          )}
          {approveNote}
          {draft.status === 'rejected' && <p>{copy.approve.rejectedNote}</p>}
          {draft.status === 'expired' && <p>{copy.approve.expiredNote}</p>}
          {draft.status === 'failed' && <p>{copy.approve.failedNote}</p>}
          {draft.practice && <button type="button" className="btn" onClick={remove}>{copy.draft.practiceDelete}</button>}
        </section>
      )}
    </div>
  );
}
