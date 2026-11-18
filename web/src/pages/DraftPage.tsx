import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { ApiError, REJECT_REASONS } from '../api';
import type { Draft, HintName, RejectReason, Suggestion } from '../api';
import { useApp } from '../app-context';
import { ApproveDialog } from '../components/ApproveDialog';
import { DraftPill, ErrorNote, ExtLink, Hint, Markdown, Skeleton, StrippedNote, SystemTag, TargetLink, kindSystem } from '../components/bits';
import { Recommendations } from '../components/Recommendations';
import { SourceList, useSourceJump } from '../components/Sources';
import { Icon } from '../components/icons';
import { PageBar, useShell } from '../components/Shell';
import { useToast } from '../components/Toasts';
import { copy } from '../copy';
import { useData } from '../data';
import { diffBlocks, diffLines } from '../diff';
import type { DiffLine, DiffPart, InlinePart } from '../diff';
import { isExpired } from '../drafts';
import { stripMarkers } from '../citations';
import { errorText, isDefiniteRefusal } from '../errors';
import { usePoll } from '../polling';
import { renderMarkdown } from '../markdown';
import { longTime, relTime, timeLeft } from '../time';
import { taskHref } from '../url';
import { tabKeys } from './Drafts';

type SaveState = 'saved' | 'saving' | 'error';
type ApproveErrorKey = keyof typeof copy.approve.errors;
/** A problem under the change box. `check` is the "Check again" action, when the outcome is not known. */
interface ChangeError {
  text: string;
  check?: 'draft' | 'suggestion';
}
interface RejectError {
  text: string;
  unsure: boolean;
}
/** Phone only: the document, the sources, and the checklist with details. */
type PanelTab = 'doc' | 'sources' | 'details';

// A changed line keeps its words, but not the markdown that starts it: a heading's hashes go,
// and a bullet becomes a dot, so the change reads like the document around it.
const HEADING = /^(\s*)(#{1,6})\s+/;
const BULLET = /^(\s*)[-*+]\s+/;

type LineShape = 'h' | 'li' | 'p';

function shapeOf(text: string): LineShape {
  if (HEADING.test(text)) return 'h';
  if (BULLET.test(text)) return 'li';
  return 'p';
}

/** Take the heading or bullet marker off the front of the first part that carries it. */
function stripLead<T extends { text: string }>(parts: T[], shape: LineShape): T[] {
  if (shape === 'p' || parts.length === 0) return parts;
  const re = shape === 'h' ? HEADING : BULLET;
  const [first, ...rest] = parts;
  if (!first || !re.test(first.text)) return parts;
  const text = first.text.replace(re, '');
  return text ? [{ ...first, text }, ...rest] : rest;
}

/** The words of a line; the ones that changed get an inline highlight. */
function LineText({ line, shape }: { line: DiffLine; shape: LineShape }) {
  if (line.inline) {
    return (
      <span className="ba-text">
        {stripLead<InlinePart>(line.inline, shape).map((p, i) =>
          p.kind === 'added' ? (
            <ins key={i} className="ba-word">
              <span className="sr-only">{copy.draft.diffAddedCue}</span>
              {p.text}
              <span className="sr-only">{copy.draft.diffEndCue}</span>
            </ins>
          ) : p.kind === 'removed' ? (
            <del key={i} className="ba-word">
              <span className="sr-only">{copy.draft.diffRemovedCue}</span>
              {p.text}
              <span className="sr-only">{copy.draft.diffEndCue}</span>
            </del>
          ) : (
            <span key={i}>{p.text}</span>
          ),
        )}
      </span>
    );
  }
  if (!line.parts) {
    const text = stripLead([{ text: line.text }], shape)[0]?.text ?? '';
    if (line.kind === 'added') return <ins className="ba-text">{text}</ins>;
    if (line.kind === 'removed') return <del className="ba-text">{text}</del>;
    return <span className="ba-text">{text}</span>;
  }
  return (
    <span className="ba-text">
      {stripLead<DiffPart>(line.parts, shape).map((p, i) =>
        p.changed ? (
          line.kind === 'added' ? <ins key={i} className="ba-word">{p.text}</ins> : <del key={i} className="ba-word">{p.text}</del>
        ) : (
          <span key={i}>{p.text}</span>
        ),
      )}
    </span>
  );
}

/** Unchanged text inside the suggestion view, formatted like the rest of the document. */
function Plain({ text }: { text: string }) {
  const rendered = useMemo(() => renderMarkdown(text), [text]);
  return <Markdown rendered={rendered} className="prose" />;
}

/**
 * A suggested change, shown in the document itself. Unchanged paragraphs read as the formatted
 * document; a reworded paragraph shows its old and new words inline; a paragraph rewritten from
 * scratch shows the new text with the old one folded above it; a list whose markers alone
 * changed gets one note. Every change carries a spoken cue, not colour alone.
 */
function DocDiff({ diff }: { diff: DiffLine[] }) {
  const blocks = useMemo(() => diffBlocks(diff), [diff]);
  const [openRemoved, setOpenRemoved] = useState<Set<number>>(new Set());
  const items: ReactNode[] = blocks.map((block, at) => {
    if (block.styleOnly) {
      return (
        <li key={at} className="dd-block dd-style">
          <p className="ba-note">
            <Icon name="info" size={13} />
            {copy.draft.diffListStyle}
          </p>
          <Plain text={block.lines.map((l) => l.text).join('\n')} />
        </li>
      );
    }
    if (!block.changed) {
      return (
        <li key={at} className="dd-block dd-same">
          <Plain text={block.lines.map((l) => l.text).join('\n')} />
        </li>
      );
    }
    const shown = block.lines.filter((l) => !l.merged);
    // A paragraph rewritten from scratch: the old text is shown lighter, cut to its first lines.
    const replaced = shown.some((l) => l.kind === 'removed') && shown.some((l) => l.kind === 'added');
    const removedLines = shown.filter((l) => l.kind === 'removed');
    const collapsible = replaced && (removedLines.length > 1 || removedLines.map((l) => l.text).join('').length > 100);
    const expanded = !collapsible || openRemoved.has(at);
    const groupId = `ba-removed-${at}`;
    const row = (l: DiffLine, k: number) => {
      const shape = shapeOf(l.text);
      return (
        <p key={k} className={`ba-line ba-${l.inline ? 'changed' : l.kind} ba-shape-${shape}${replaced && l.kind === 'removed' ? ' ba-soft' : ''}`}>
          <span className="sr-only">
            {l.inline ? copy.draft.diffChanged : l.kind === 'added' ? copy.draft.diffAdded : l.kind === 'removed' ? copy.draft.diffRemoved : copy.draft.diffSame}
          </span>
          <LineText line={l} shape={shape} />
        </p>
      );
    };
    const out: ReactNode[] = [];
    let k = 0;
    while (k < shown.length) {
      if (collapsible && shown[k]!.kind === 'removed') {
        const group: ReactNode[] = [];
        while (k < shown.length && shown[k]!.kind === 'removed') {
          group.push(row(shown[k]!, k));
          k++;
        }
        out.push(
          <div key={`g${k}`} className="ba-removed-group">
            <div id={groupId} className={expanded ? undefined : 'ba-clamp'}>{group}</div>
            <button
              type="button"
              className="ba-more"
              aria-expanded={expanded}
              aria-controls={groupId}
              onClick={() =>
                setOpenRemoved((o) => {
                  const n = new Set(o);
                  if (n.has(at)) n.delete(at);
                  else n.add(at);
                  return n;
                })
              }
            >
              <Icon name={expanded ? 'chevronDown' : 'chevronRight'} size={13} />
              {expanded ? copy.draft.diffHideRemoved : copy.draft.diffShowRemoved}
            </button>
          </div>,
        );
      } else {
        out.push(row(shown[k]!, k));
        k++;
      }
    }
    return (
      <li key={at} className="dd-block ba-block">
        {out}
      </li>
    );
  });
  return (
    <ol className="doc-diff" aria-label={copy.draft.beforeAfter}>
      {items}
    </ol>
  );
}

type Check = { tone: 'ok' | 'warn' | 'info'; text: string };

/** A checklist row: the icon carries the tone, and a spoken word says it too. */
function CheckItem({ c }: { c: Check }) {
  const icon = c.tone === 'ok' ? 'check' : c.tone === 'warn' ? 'alert' : 'info';
  const word = c.tone === 'ok' ? copy.draft.checklist.ok : c.tone === 'warn' ? copy.draft.checklist.check : copy.draft.checklist.info;
  return (
    <li className={`check-item check-${c.tone}`}>
      <Icon name={icon} size={16} weight={c.tone === 'ok' ? 2.4 : 2} className="check-icon" />
      <span>
        <span className="sr-only">{word}: </span>
        {c.text}
      </span>
    </li>
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
  const [tab, setTab] = useState<PanelTab>('doc');
  const { selected, flash, pick: pickSource, toSource, toText } = useSourceJump(tab);
  const tabRefs = useRef<Partial<Record<PanelTab, HTMLButtonElement | null>>>({});
  const changeBox = useRef<HTMLTextAreaElement>(null);

  const [instruction, setInstruction] = useState('');
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null);
  const [changeError, setChangeError] = useState<ChangeError | null>(null);
  const [changeBusy, setChangeBusy] = useState(false);

  const [confirming, setConfirming] = useState(false);
  const [forceExternal, setForceExternal] = useState(false);
  const [externalOk, setExternalOk] = useState(false);
  const [approving, setApproving] = useState(false);
  const [approveError, setApproveError] = useState<{ key: ApproveErrorKey; message: string } | null>(null);
  const [justApproved, setJustApproved] = useState(false);
  // An approval whose outcome is not known: no second try until the PM has checked where it stands.
  const [uncertain, setUncertain] = useState(false);

  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState<RejectReason | ''>('');
  const [rejectError, setRejectError] = useState<RejectError | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const rejectBox = useRef<HTMLDivElement>(null);
  const [moreOpen, setMoreOpen] = useState(false);
  const moreBtn = useRef<HTMLButtonElement>(null);
  const moreItems = useRef<(HTMLButtonElement | null)[]>([]);

  // What the PM reads is the saved text. Links and stripped parts come from the same rendering.
  const rendered = useMemo(
    () => renderMarkdown(draft?.body ?? '', { sources: draft?.sources ?? [] }),
    [draft?.body, draft?.sources],
  );
  // The edit box, the suggestion diff and the "unsaved" check all work on the text without markers.
  const plainBody = useMemo(() => stripMarkers(draft?.body ?? ''), [draft?.body]);

  const adopt = useCallback((d: Draft) => {
    setDraft(d);
    setTitle(d.title);
    setBody(stripMarkers(d.body));
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
      // An open suggestion from an earlier visit needs a decision: it shows in the document.
      if (s && (s.status === 'working' || s.status === 'ready')) setTab('doc');
    } catch (e) {
      setLoadError(
        e instanceof ApiError && e.code === 'not_found' ? copy.errors.notFound : errorText(e, copy.errors.loadDraftFailed),
      );
    }
  }, [api, id, adopt, refreshMe]);

  useEffect(() => {
    setDraft(null);
    setConfirming(false);
    setRejecting(false);
    setApproveError(null);
    setJustApproved(false);
    setUncertain(false);
    void load();
  }, [load]);

  // Poll a working suggestion.
  usePoll(
    suggestion?.status === 'working',
    async () => {
      try {
        setSuggestion(await api.getSuggestion(id));
      } catch (e) {
        setChangeError({ text: errorText(e, copy.errors.unknown) });
      }
    },
    pollMs,
  );

  // The reject popover and the phone's More menu close on a click outside them.
  useEffect(() => {
    if (!rejecting && !moreOpen) return;
    const away = (e: MouseEvent) => {
      if (rejectBox.current && !rejectBox.current.contains(e.target as Node)) {
        setRejecting(false);
        setMoreOpen(false);
      }
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [rejecting, moreOpen]);

  useEffect(() => {
    if (moreOpen) moreItems.current[0]?.focus();
  }, [moreOpen]);

  const crumb = (
    <nav aria-label={copy.panel.breadcrumb} className="crumbs">
      <a href="#/drafts">{copy.nav.drafts}</a>
      <span className="crumb-sep" aria-hidden="true">/</span>
      <span className="crumb-current" aria-current="page">{draft?.title ?? '…'}</span>
    </nav>
  );

  if (loadError) {
    return (
      <>
        <PageBar crumb={crumb} />
        <div className="page page-narrow">
          <a href="#/" className="back">
            <Icon name="arrowLeft" size={16} />
            {copy.draft.back}
          </a>
          <ErrorNote next={undefined} actionLabel={copy.errors.retry} onAction={() => void load()}>
            {loadError}
          </ErrorNote>
        </div>
      </>
    );
  }
  if (!draft) {
    return (
      <>
        <PageBar crumb={crumb} />
        <div className="workspace">
          <div className="doc-col">
            <div className="sheet">
              <Skeleton rows={4} label={copy.draft.loading} variant="doc" />
            </div>
          </div>
        </div>
      </>
    );
  }

  const expired = isExpired(draft);
  const pending = draft.status === 'pending' && !expired;
  const dirty = title !== draft.title || body !== plainBody;
  const unresolved = suggestion?.status === 'working' || suggestion?.status === 'ready';
  // The PM must have seen the formatted, saved text, with no suggestion left hanging, and know
  // whether an approval that may or may not have gone through did.
  const blocked =
    editing || dirty ? copy.draft.saveFirst : unresolved ? copy.draft.resolveFirst : uncertain ? copy.draft.checkFirst : null;

  const startEdit = () => {
    setTitle(draft.title);
    setBody(plainBody);
    setEditing(true);
    if (isPhone) setTab('doc');
  };

  const discard = () => {
    setTitle(draft.title);
    setBody(plainBody);
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
          ? { text: copy.draft.changeInProgress }
          : isDefiniteRefusal(err)
            ? { text: errorText(err, copy.errors.unknown) }
            : { text: `${copy.errors.unsure(copy.errors.unsureWhat.asked)} ${copy.errors.unsureNext}`, check: 'suggestion' },
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
      setChangeError(
        isDefiniteRefusal(err)
          ? { text: errorText(err, copy.errors.unknown) }
          : {
              text: `${copy.errors.unsure(accept ? copy.errors.unsureWhat.accepted : copy.errors.unsureWhat.declined)} ${copy.errors.unsureNext}`,
              check: 'draft',
            },
      );
    }
  };

  /** "Check again" under the change box: look at what the server holds now. */
  const checkChange = async (what: 'draft' | 'suggestion') => {
    try {
      if (what === 'draft') adopt(await api.getDraft(id));
      const s = await api.getSuggestion(id);
      setSuggestion(s);
      setChangeError(null);
      if (what === 'suggestion' && !s) toast(copy.draft.checkedNoChange);
      void reloadDrafts();
    } catch {
      /* keep the note: it is still not known */
    }
  };

  /** Look at the draft as the server has it now, without touching the text being edited. */
  const checkNow = async (): Promise<Draft | null> => {
    try {
      const d = await api.getDraft(id);
      setDraft(d);
      void reloadDrafts();
      return d;
    } catch {
      return null;
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
      toast(d.practice ? copy.toast.practiceApproved : copy.toast.approved(d.kind), 'good');
      void refreshMe();
      void reloadDrafts();
    } catch (err) {
      if (!isDefiniteRefusal(err, 'decision')) {
        // A dropped connection, a timeout, a 5xx or unknown_outcome: it is not known whether it was
        // sent. Say so, offer to look, and do not offer a blind second try.
        setConfirming(false);
        setUncertain(true);
        if (isPhone) setTab('doc');
        setApproveError({ key: 'unknown_outcome', message: '' });
      } else {
        const known: ApproveErrorKey[] = ['needs_external_confirm', 'audience_blocked', 'expired', 'not_pending', 'stale'];
        const key = known.find((k) => k === err.code) ?? 'other';
        if (key === 'needs_external_confirm') setForceExternal(true);
        if (key === 'expired' || key === 'not_pending' || key === 'stale' || key === 'needs_external_confirm' || key === 'audience_blocked') {
          // The draft changed under us (or the server has now looked at the channel): show what is
          // really there, and read it again. A decided draft shows as decided.
          try {
            const fresh = await api.getDraft(id);
            if (key === 'needs_external_confirm' || key === 'audience_blocked') {
              // Only the facts the server has just learned: nothing the PM may be typing is touched.
              setDraft((d) => (d ? { ...d, audience: fresh.audience, external: fresh.external, restricted_sources: fresh.restricted_sources } : fresh));
            } else {
              adopt(fresh);
              setSuggestion(await api.getSuggestion(id));
              if (key === 'not_pending' && fresh.status !== 'pending') {
                // Decided somewhere else, perhaps in Slack: show where it stands, not an error.
                setConfirming(false);
                setApproveError(null);
                setJustApproved(false);
                toast(copy.approve.decidedElsewhere);
                void reloadDrafts();
                void refreshMe();
                return;
              }
            }
          } catch {
            /* keep what we have */
          }
        }
        if (key === 'stale') setConfirming(false);
        if (isPhone) setTab('doc');
        setApproveError({ key, message: key === 'other' ? errorText(err, copy.errors.unknown) : '' });
      }
    } finally {
      setApproving(false);
    }
  };

  /** "Check again" after an approval whose outcome was not known. */
  const checkApproval = async () => {
    try {
      const d = await api.getDraft(id);
      adopt(d);
      setSuggestion(await api.getSuggestion(id));
      setUncertain(false);
      setApproveError(null);
      void reloadDrafts();
      if (d.status === 'pending') toast(copy.approve.checkedPending);
      else if (d.status === 'approved') {
        setJustApproved(true);
        toast(d.practice ? copy.toast.practiceApproved : copy.toast.approved(d.kind), 'good');
        void refreshMe();
      }
    } catch {
      setApproveError({ key: 'unknown_outcome', message: copy.approve.checkFailed });
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
      if (err instanceof ApiError && (err.code === 'not_pending' || err.code === 'expired')) {
        // Decided somewhere else (perhaps in Slack): show where it stands.
        const d = await checkNow();
        if (d && d.status !== 'pending') {
          setRejecting(false);
          toast(copy.approve.decidedElsewhere);
          return;
        }
      }
      setRejectError(
        isDefiniteRefusal(err, 'decision')
          ? { text: errorText(err, copy.errors.unknown), unsure: false }
          : { text: `${copy.errors.unsure(copy.errors.unsureWhat.rejected)} ${copy.errors.unsureNext}`, unsure: true },
      );
    }
  };

  const checkReject = async () => {
    const d = await checkNow();
    if (!d) return;
    setRejectError(null);
    if (d.status !== 'pending') setRejecting(false);
  };

  const remove = async () => {
    setDeleteError(null);
    try {
      await api.deleteDraft(id);
      void reloadDrafts();
      navigate('/');
    } catch (err) {
      setDeleteError(errorText(err, copy.errors.unknown));
    }
  };

  /** "Ask me to add sources": fill the change box and put the cursor in it. */
  const askForSources = () => {
    setInstruction((t) => t || copy.draft.askSourcesPrefill);
    setTab('doc');
    setTimeout(() => changeBox.current?.focus(), 0);
  };

  const showExternal = draft.external || forceExternal;
  const confirmLabel = showExternal ? copy.approve.confirmExternalButton : copy.approve.confirm(draft.kind);
  const diff =
    suggestion?.status === 'ready' && suggestion.body !== null ? diffLines(plainBody, stripMarkers(suggestion.body)) : null;
  const changed = diff?.some((l) => l.kind !== 'same' || l.markerOnly) ?? false;
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
              : approveError.key === 'unknown_outcome'
                ? copy.approve.checkAgain
                : undefined
        }
        onAction={() =>
          approveError.key === 'expired'
            ? navigate('/')
            : approveError.key === 'unknown_outcome'
              ? void checkApproval()
              : void load()
        }
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
  const rejectPopover = rejecting && (
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
      {rejectError && (
        <ErrorNote actionLabel={rejectError.unsure ? copy.errors.checkAgain : undefined} onAction={() => void checkReject()}>
          {rejectError.text}
        </ErrorNote>
      )}
      <div className="popover-foot">
        <button type="button" className="btn btn-ghost" onClick={() => setRejecting(false)}>
          {copy.reject.cancel}
        </button>
        <button type="button" className="btn btn-danger" onClick={() => void reject()}>
          {copy.reject.confirm}
        </button>
      </div>
    </div>
  );

  const saveStatus = (
    <span role="status" className={`save-state save-${chipTone}`}>
      <span className="save-dot" aria-hidden="true" />
      {saveChip}
    </span>
  );
  const approveButton = (
    <button
      type="button"
      id="approve-btn"
      className="btn btn-accent"
      onClick={openConfirm}
      disabled={blocked !== null}
      aria-describedby={blocked ? 'approve-blocked' : undefined}
    >
      <Icon name="check" size={16} weight={2.4} />
      {copy.approve.button(draft.kind)}
    </button>
  );

  // On a phone the header holds the primary button and one More menu (Edit, Reject).
  const moreEntries = [
    ...(!editing ? [{ key: 'edit', label: copy.draft.edit, icon: 'edit' as const, run: startEdit }] : []),
    { key: 'reject', label: copy.reject.button, icon: 'x' as const, run: () => setRejecting(true) },
  ];
  const moreMenu = (
    <div className="popover-anchor" ref={rejectBox}>
      <button
        ref={moreBtn}
        type="button"
        className="btn"
        aria-haspopup="menu"
        aria-expanded={moreOpen}
        aria-controls={moreOpen ? 'more-menu' : undefined}
        onClick={() => {
          setRejecting(false);
          setMoreOpen((o) => !o);
        }}
      >
        <Icon name="more" size={18} />
        {copy.panel.moreShort}
      </button>
      {moreOpen && (
        <div
          id="more-menu"
          className="menu-panel more-panel"
          role="menu"
          aria-label={copy.panel.more}
          onKeyDown={(e) => {
            const cur = moreItems.current.findIndex((el) => el === document.activeElement);
            const n = moreEntries.length;
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              e.preventDefault();
              moreItems.current[(cur + (e.key === 'ArrowDown' ? 1 : -1) + n) % n]?.focus();
            } else if (e.key === 'Escape') {
              e.preventDefault();
              setMoreOpen(false);
              moreBtn.current?.focus();
            } else if (e.key === 'Tab') setMoreOpen(false);
          }}
        >
          {moreEntries.map((m, i) => (
            <button
              key={m.key}
              ref={(el) => { moreItems.current[i] = el; }}
              type="button"
              role="menuitem"
              tabIndex={-1}
              className="menu-item"
              onClick={() => {
                setMoreOpen(false);
                m.run();
              }}
            >
              <Icon name={m.icon} size={16} />
              <span className="menu-item-label">{m.label}</span>
            </button>
          ))}
        </div>
      )}
      {rejectPopover}
    </div>
  );

  const actions = pending ? (
    isPhone ? (
      <div className="draft-actions">
        {saveStatus}
        {approveButton}
        {moreMenu}
      </div>
    ) : (
      <div className="draft-actions">
        {!editing && (
          <button type="button" className="btn" onClick={startEdit}>
            <Icon name="edit" size={16} />
            {copy.draft.edit}
          </button>
        )}
        <div className="popover-anchor" ref={rejectBox}>
          <button
            type="button"
            className="btn btn-danger-text"
            onClick={() => setRejecting((r) => !r)}
            aria-expanded={rejecting}
            aria-controls={rejecting ? 'reject-pop' : undefined}
          >
            {copy.reject.button}
          </button>
          {rejectPopover}
        </div>
        {approveButton}
      </div>
    )
  ) : null;

  // ---- The side panel: what the bot read, the checklist, and the details. ----
  // Markers in the text, per source.
  const places = rendered.cites;
  const onCite = (sid: string) => {
    if (isPhone) setTab('sources');
    toSource(sid);
  };
  const showInText = (sid: string) => {
    setTab('doc');
    toText(sid);
  };

  const sourcesList = (
    <SourceList
      sources={draft.sources}
      places={places}
      selected={selected}
      flash={flash}
      onPick={pickSource}
      onShowInText={isPhone ? showInText : undefined}
      emptyAction={
        pending ? (
          <button type="button" className="btn btn-sm" onClick={askForSources}>
            {copy.draft.askSources}
          </button>
        ) : undefined
      }
    />
  );

  const sourcesSection = (
    <section className="side-section" aria-labelledby="h-src">
      <div className="side-head">
        <h2 id="h-src">{copy.draft.sourcesHeading}</h2>
        <span className="side-count">{copy.draft.sourcesCount(draft.sources.length)}</span>
      </div>
      {pending && !isPhone && <Hint name="sources" anchor="h-src" />}
      {sourcesList}
    </section>
  );

  // Computed from what the page knows, never assumed.
  const checks: Check[] = [];
  if (draft.practice) checks.push({ tone: 'info', text: copy.draft.checklist.practice });
  checks.push(
    draft.sources.length > 0
      ? { tone: 'ok', text: isPhone ? copy.draft.checklist.sourcesPhone(draft.sources.length) : copy.draft.checklist.sources(draft.sources.length) }
      : { tone: 'warn', text: copy.draft.checklist.noSources },
  );
  // The links between the text and its sources, from what the text really holds.
  const citedCount = draft.sources.filter((s) => s.cited).length;
  const linkCheck: Check | null =
    draft.practice || draft.sources.length === 0
      ? null
      : draft.edited
        ? { tone: 'info', text: copy.draft.checklist.editedNoLinks }
        : citedCount > 0
          ? { tone: 'ok', text: copy.draft.checklist.cited(citedCount, draft.sources.length) }
          : Object.keys(rendered.cites).length === 0
            ? { tone: 'warn', text: copy.draft.checklist.noneCited }
            : null;
  if (linkCheck) checks.push(linkCheck);
  if (draft.restricted_sources) checks.push({ tone: 'warn', text: copy.approve.restricted });
  checks.push(showExternal ? { tone: 'warn', text: copy.draft.checklist.external } : { tone: 'ok', text: copy.draft.checklist.internal });
  if (draft.audience?.members != null) checks.push({ tone: 'info', text: copy.approve.audienceVisible(draft.audience.members) });
  if (unresolved) checks.push({ tone: 'warn', text: copy.draft.checklist.suggestion });
  if (editing || dirty) checks.push({ tone: 'warn', text: copy.draft.checklist.unsaved });
  if (rendered.stripped.length > 0) checks.push({ tone: 'warn', text: copy.draft.checklist.stripped(rendered.stripped.length) });
  checks.push(left ? { tone: 'warn', text: copy.draft.checklist.expires(left) } : draft.expires_at ? { tone: 'info', text: longTime(draft.expires_at) } : { tone: 'ok', text: copy.draft.checklist.noExpiry });
  if (draft.edited && linkCheck?.text !== copy.draft.checklist.editedNoLinks) checks.push({ tone: 'info', text: copy.draft.checklist.edited });

  const checklist = pending ? (
    <section className="side-section" aria-labelledby="h-check">
      <h2 id="h-check">{copy.draft.checklistHeading}</h2>
      <ul className="checklist">
        {checks.map((c) => <CheckItem key={c.text} c={c} />)}
      </ul>
    </section>
  ) : null;

  const detailsSection = (
    <section className="side-section side-details" aria-labelledby="h-det">
      <h2 id="h-det">{copy.panel.details}</h2>
      <dl className="details">
        <dt>{copy.panel.kind}</dt>
        <dd>{copy.home.kindLabel(draft.kind)}</dd>
        <dt>{copy.panel.target}</dt>
        <dd>{draft.target.label} <TargetLink target={draft.target} /></dd>
        <dt>{copy.panel.bot}</dt>
        <dd><code className="mono-id">{draft.bot}</code></dd>
        <dt>{copy.panel.created}</dt>
        <dd>{longTime(draft.created_at)} <span className="muted">· {relTime(draft.created_at)}</span></dd>
        {draft.edited_at && (
          <>
            <dt>{copy.panel.editedAt}</dt>
            <dd>{longTime(draft.edited_at)} <span className="muted">· {relTime(draft.edited_at)}</span></dd>
          </>
        )}
        <dt>{copy.panel.expires}</dt>
        <dd>{draft.expires_at ? <>{longTime(draft.expires_at)}{left && <span className="muted"> · {left}</span>}</> : copy.panel.never}</dd>
        <dt>{copy.panel.task}</dt>
        <dd>{draft.task_id ? <a href={taskHref(draft.task_id)}>{copy.panel.openTask}</a> : <span className="muted">{copy.panel.noTask}</span>}</dd>
        <dt>{copy.panel.id}</dt>
        <dd><code className="mono-id">{draft.id}</code></dd>
      </dl>
    </section>
  );

  // Hints only appear on a pending draft: once it is decided there is nothing left to coach.
  const hintNames = (pending ? ['sources', 'ask', 'approve'] : []) as HintName[];

  // ---- The document: a paper sheet, with a suggestion shown in place ----
  const meta = (
    <div className="doc-meta">
      <SystemTag system={kindSystem(draft.kind)} label={copy.home.kindLabel(draft.kind)} />
      <span className="doc-target">{draft.target.label}</span>
      <TargetLink target={draft.target} />
      {!draft.practice && <span className="doc-who">{copy.draft.publishedAs(me.name)}</span>}
      {draft.external && <span className="tag tag-warn">{copy.panel.external}</span>}
      {left && <span className="doc-left"><Icon name="clock" size={14} />{left}</span>}
      {draft.edited && (
        <span className="doc-edited">{draft.edited_at ? copy.draft.editedWhen(relTime(draft.edited_at)) : copy.panel.edited}</span>
      )}
      {draft.restricted_sources && <span className="tag tag-warn">{copy.approve.restricted}</span>}
    </div>
  );

  let sheet: ReactNode;
  if (editing) {
    sheet = (
      <section className="sheet sheet-edit" aria-label={copy.panel.doc}>
        {meta}
        <div className="editor">
          <h1 className="sr-only">{draft.title}</h1>
          <label htmlFor="draft-title" className="micro-label">{copy.draft.titleLabel}</label>
          <input
            id="draft-title"
            className="title-input"
            value={title}
            onChange={(e) => { setTitle(e.target.value); setSaveState('saved'); }}
          />
          <label htmlFor="draft-body" className="micro-label">{copy.draft.bodyLabel}</label>
          <textarea
            id="draft-body"
            className="body-input"
            value={body}
            onChange={(e) => { setBody(e.target.value); setSaveState('saved'); }}
            rows={18}
          />
          <div className="editor-foot">
            <p className="muted small">
              {copy.draft.editingNote}
              {!draft.practice && <> {copy.draft.editSavedNote}</>}
            </p>
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
      </section>
    );
  } else if (suggestion?.status === 'ready' && diff) {
    sheet = (
      <section className="sheet sheet-suggest" aria-label={copy.draft.suggestionHeading}>
        <div className="suggest-bar">
          <p className="suggest-text">
            <Icon name="sparkle" size={15} />
            <span>{copy.draft.suggestionBar(suggestion.instruction)}</span>
          </p>
          <div className="suggest-buttons">
            <button type="button" className="btn btn-sm btn-quiet" onClick={() => void decideSuggestion(false)}>
              {copy.draft.rejectSuggestion}
            </button>
            <button type="button" className="btn btn-sm btn-primary" onClick={() => void decideSuggestion(true)}>
              {copy.draft.accept}
            </button>
          </div>
        </div>
        {meta}
        <h1 className="doc-title">{draft.title}</h1>
        {changed ? <DocDiff diff={diff} /> : <p className="muted">{copy.draft.diffNoChange}</p>}
      </section>
    );
  } else {
    sheet = (
      <article className="sheet" aria-label={copy.draft.formattedLabel}>
        {meta}
        <h1 className="doc-title">{draft.title}</h1>
        <Markdown rendered={rendered} className="prose" onCite={onCite} highlight={selected} />
        <StrippedNote parts={rendered.stripped} what={copy.stripped.draft} />
      </article>
    );
  }

  const changeForm = pending ? (
    <div className="dock">
      {!isPhone && <Hint name="ask" anchor="change-box" />}
      <form onSubmit={askChange} className="change-form" aria-label={copy.draft.askHeading}>
        <label htmlFor="change-box" className="micro-label change-label">{copy.draft.changeShort}</label>
        <textarea
          id="change-box"
          aria-label={copy.draft.askLabel}
          ref={changeBox}
          rows={1}
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
          className="btn btn-primary"
          disabled={changeBusy || !instruction.trim() || suggestion?.status === 'working'}
        >
          <Icon name="sparkle" size={15} />
          {copy.draft.askButton}
        </button>
      </form>
      {changeError && (
        <ErrorNote
          actionLabel={changeError.check ? copy.errors.checkAgain : undefined}
          onAction={changeError.check ? () => void checkChange(changeError.check!) : undefined}
        >
          {changeError.text}
        </ErrorNote>
      )}
      {suggestion?.status === 'working' && (
        <p role="status" className="live-line live-line-on dock-status">
          <span className="live-dot" aria-hidden="true" />
          <span className="live-text">{copy.draft.suggestionWorking}</span>
        </p>
      )}
      {suggestion?.status === 'failed' && (
        <ErrorNote next={copy.draft.suggestionFailedNext} actionLabel={copy.draft.rejectSuggestion} onAction={() => void decideSuggestion(false)}>
          {copy.draft.suggestionFailed(suggestion.error ?? '')}
        </ErrorNote>
      )}
    </div>
  ) : null;

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
                <p>{copy.approve.approvedWhere(draft.kind, draft.target.label)}</p>
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

      {isPhone && (
        <div className="recs-phone">
          <Recommendations recs={draft.recommendations} />
        </div>
      )}
      {sheet}
      {!pending && !expired && <p className="muted small read-only">{copy.draft.readOnly(draft.status)}</p>}
      {changeForm}
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
        // the dialog's own external notice carries the needs_external_confirm answer: no second box
        externalAsked={approveError?.key === 'needs_external_confirm'}
        note={approveError?.key === 'needs_external_confirm' ? null : approveNote}
        blocked={approveError?.key === 'audience_blocked'}
        onReject={() => {
          setConfirming(false);
          setRejecting(true);
        }}
        onAskElsewhere={() => {
          setConfirming(false);
          setInstruction((t) => t || copy.approve.blockedAskPrefill);
          setTab('doc');
          setTimeout(() => changeBox.current?.focus(), 0);
        }}
        onConfirm={() => void approve()}
        onCancel={() => setConfirming(false)}
      />
    ) : null;

  if (isPhone) {
    const tabs: { key: PanelTab; label: string; count?: number }[] = [
      { key: 'doc', label: copy.panel.doc },
      { key: 'sources', label: copy.panel.sources, count: draft.sources.length },
      { key: 'details', label: copy.panel.details },
    ];
    const pickTab = (t: PanelTab) => {
      setTab(t);
      tabRefs.current[t]?.focus();
    };
    const panelBody = {
      doc: null,
      sources: sourcesSection,
      details: <>{checklist}{detailsSection}</>,
    }[tab];
    return (
      <>
        <PageBar crumb={crumb} />
        <div className="workspace workspace-phone">
          {tab !== 'doc' && <h1 className="sr-only">{draft.title}</h1>}
          {hintNames.map((n) => <Hint key={n} name={n} kind={draft.kind} />)}
          <div className="phone-actions">
            <DraftPill status={expired ? 'expired' : draft.status} />
            {actions}
          </div>
          <div
            className="tabs panel-tabs"
            role="tablist"
            aria-label={copy.panel.tabsLabel}
            onKeyDown={(e) => tabKeys(e, tabs.map((t) => t.key), tab, pickTab)}
          >
            {tabs.map((t) => (
              <button
                key={t.key}
                ref={(el) => { tabRefs.current[t.key] = el; }}
                type="button"
                role="tab"
                id={`ptab-${t.key}`}
                aria-selected={tab === t.key}
                aria-controls={`ppanel-${t.key}`}
                tabIndex={tab === t.key ? 0 : -1}
                className="tab"
                onClick={() => setTab(t.key)}
              >
                {t.label}
                {t.count !== undefined && <span className="tab-count">{t.count}</span>}
              </button>
            ))}
          </div>
          {tab === 'doc' ? (
            <div role="tabpanel" id="ppanel-doc" aria-labelledby="ptab-doc">{docColumn}</div>
          ) : (
            <div role="tabpanel" id={`ppanel-${tab}`} aria-labelledby={`ptab-${tab}`} className="panel-body">
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
      <PageBar crumb={crumb}>
        <DraftPill status={expired ? 'expired' : draft.status} />
        {pending && saveStatus}
        <span className="pagebar-spacer" />
        {actions}
      </PageBar>
      <div className="workspace">
        {docColumn}
        <aside className="panel" aria-label={copy.panel.label}>
          {pending && <Hint name="approve" anchor="approve-btn" kind={draft.kind} />}
          <Recommendations recs={draft.recommendations} />
          {sourcesSection}
          {checklist}
          {detailsSection}
        </aside>
      </div>
      {dialog}
    </>
  );
}
