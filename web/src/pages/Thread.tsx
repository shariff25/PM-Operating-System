import { useEffect, useMemo, useState } from 'react';
import { ApiError } from '../api';
import type { Draft, Task } from '../api';
import { messageOf, useApp } from '../app-context';
import { identify } from '../bots';
import { BotFace, FallbackNote, PreviewTag } from '../components/BotParts';
import { Avatar, Empty, ErrorNote, Markdown, Skeleton, StrippedNote, SystemTag, kindSystem } from '../components/bits';
import { Icon, Mark, kindIcon } from '../components/icons';
import { PageBar } from '../components/Shell';
import { copy } from '../copy';
import { useAsk, useData } from '../data';
import { renderMarkdown } from '../markdown';
import { usePoll } from '../polling';
import { isRetrying, isSettled, milestones, usableProgress } from '../tasks';
import { relTime } from '../time';
import { draftHref } from '../url';

/** The request's reference, for when someone asks support about it. Only shown with the steps. */
function TaskReference({ id }: { id: string }) {
  const [copied, setCopied] = useState(false);
  const copyIt = () => {
    navigator.clipboard?.writeText(id).then(() => setCopied(true), () => {});
  };
  return (
    <p className="task-ref">
      <span>{copy.activity.reference}</span>
      <code className="mono-id">{id}</code>
      <button type="button" className="task-ref-btn" onClick={copyIt}>
        {copied ? copy.activity.copiedReference : copy.activity.copyReference}
      </button>
    </p>
  );
}

/** A draft the request produced, as a card with its title, kind and an Open draft button. */
function DraftCard({ id, fallback }: { id: string; fallback?: boolean }) {
  const { api, navigate } = useApp();
  const [draft, setDraft] = useState<Draft | null>(null);
  useEffect(() => {
    let live = true;
    api.getDraft(id).then((d) => live && setDraft(d)).catch(() => {});
    return () => {
      live = false;
    };
  }, [api, id]);
  return (
    <div className="draft-card">
      <span className="draft-card-icon"><Icon name={draft ? kindIcon(draft.kind) : 'drafts'} size={18} /></span>
      <span className="draft-card-text">
        <span className="draft-card-eyebrow">
          {draft && <SystemTag system={kindSystem(draft.kind)} label={kindSystem(draft.kind)} />}
          {draft ? copy.home.kindLabel(draft.kind) : copy.activity.draftCard}
        </span>
        <a className="draft-card-title" href={draftHref(id)}>{draft?.title ?? copy.activity.draftCard}</a>
        {draft && <span className="draft-card-meta">{draft.target.label}</span>}
        {fallback && <FallbackNote />}
      </span>
      <button type="button" className="btn btn-primary" onClick={() => navigate(`/d/${encodeURIComponent(id)}`)}>
        {copy.activity.openDraft}
        <Icon name="arrowRight" size={15} weight={2.2} />
      </button>
    </div>
  );
}

/**
 * One request as a short conversation: what the PM asked, then the bot's reply with a single
 * live status line, the steps on request, and the result, the failure, or the draft it made.
 */
export function Thread({ id }: { id: string }) {
  const { api, me, pollMs } = useApp();
  const { upsertTask, reloadDrafts, bots } = useData();
  const { start, asking, error: retryError } = useAsk();
  const [task, setTask] = useState<Task | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [steps, setSteps] = useState(false);

  useEffect(() => {
    let live = true;
    api
      .getTask(id)
      .then((t) => live && setTask(t))
      .catch((e) => live && setLoadError(e instanceof ApiError && e.code === 'not_found' ? copy.activity.notFound : messageOf(e, copy.errors.unknown)));
    return () => {
      live = false;
    };
  }, [api, id]);

  // Poll a working task until it settles: quick at first, slower later, paused in a hidden tab.
  usePoll(
    task !== null && !isSettled(task),
    async () => {
      try {
        setTask(await api.getTask(id));
      } catch (e) {
        setLoadError(messageOf(e, copy.errors.unknown));
      }
    },
    pollMs,
  );

  useEffect(() => {
    if (task) upsertTask(task);
  }, [task, upsertTask]);

  const settled = task ? isSettled(task) : false;
  useEffect(() => {
    if (settled) void reloadDrafts();
  }, [settled, reloadDrafts]);

  const rendered = useMemo(() => renderMarkdown(task?.result ?? ''), [task?.result]);

  const crumb = (
    <nav aria-label={copy.panel.breadcrumb} className="crumbs">
      <a href="#/activity">{copy.nav.activity}</a>
      <span className="crumb-sep" aria-hidden="true">/</span>
      <span className="crumb-current" aria-current="page">{task?.text ?? '…'}</span>
    </nav>
  );

  if (loadError && !task) {
    return (
      <div className="page page-narrow">
        <PageBar crumb={crumb} />
        <ErrorNote>{loadError}</ErrorNote>
      </div>
    );
  }
  if (!task) {
    return (
      <>
        <PageBar crumb={crumb} />
        <div className="page page-narrow">
          <Skeleton rows={3} label={copy.activity.loading} variant="doc" />
        </div>
      </>
    );
  }

  const shown = milestones(task);
  const latest = shown[shown.length - 1];
  const working = !settled;
  const liveLine = working ? (latest?.text ?? copy.task.status[task.status]) : copy.task.status[task.status];
  const progress = working ? usableProgress(task) : null;
  const nothingToShow = settled && !task.result && !task.error && task.draft_ids.length === 0 && task.status !== 'failed';
  const who = task.persona !== undefined ? identify(task.persona, task.persona_offered, bots?.bots) : null;

  return (
    <>
    <PageBar crumb={crumb}>
      <span className="pagebar-spacer" />
      <span className="pagebar-meta">{copy.activity.askedAt(relTime(task.created_at))}</span>
    </PageBar>
    <div className="page page-narrow thread">
      <h1 className="sr-only">{task.text}</h1>
      <section className="thread-list" aria-label={copy.activity.threadLabel}>
        <article className="msg msg-you" aria-label={copy.activity.you}>
          <div className="msg-head">
            <Avatar name={me.name} size="sm" />
            <span className="msg-who">{copy.activity.you}</span>
            <span className="msg-time">{relTime(task.created_at)}</span>
          </div>
          <div className="msg-bubble">{task.text}</div>
        </article>

        <article className="msg msg-bot" aria-label={who ? who.name : copy.activity.bot}>
          <div className="msg-head">
            {/* Which bot answered: from `persona` when the server says (an older one doesn't: PM OS). */}
            {who ? <BotFace mascot={who.mascot} size={24} /> : <Mark size={24} />}
            <span className="msg-who">{who ? who.name : copy.activity.bot}</span>
            {who?.preview && <PreviewTag short />}
            <span className="msg-time">{relTime(task.updated_at)}</span>
          </div>
          <div className="msg-body">
            <p className={`live-line ${working ? 'live-line-on' : ''}`} role="status">
              {working ? <span className="live-dot" aria-hidden="true" /> : <Icon name={task.status === 'failed' ? 'alert' : 'check'} size={16} className="live-icon" />}
              <span className="live-text">{liveLine}</span>
              {progress && <span className="live-count">{copy.task.progress(progress.done, progress.total)}</span>}
            </p>
            {working && (
              <div className="live-sub">
                <div
                  className={`progress ${progress ? '' : 'progress-off'}`}
                  role={progress ? 'progressbar' : undefined}
                  aria-label={progress ? copy.task.progressLabel : undefined}
                  aria-valuemin={progress ? 0 : undefined}
                  aria-valuemax={progress?.total}
                  aria-valuenow={progress?.done}
                  aria-valuetext={progress ? copy.task.progress(progress.done, progress.total) : undefined}
                  aria-hidden={progress ? undefined : true}
                >
                  <span className="progress-fill" style={{ width: progress ? `${(progress.done / progress.total) * 100}%` : '0%' }} />
                </div>
                <p className="live-note">{isRetrying(task) ? copy.modelBusy : ''}</p>
              </div>
            )}

            {shown.length > 0 && (
              <div className="steps">
                <button type="button" className="steps-toggle" aria-expanded={steps} aria-controls="steps-list" onClick={() => setSteps((s) => !s)}>
                  <Icon name={steps ? 'chevronDown' : 'chevronRight'} size={14} />
                  {steps ? copy.activity.hideSteps : copy.activity.showSteps}
                  <span className="steps-count">{shown.length}</span>
                </button>
                {steps && (
                  <ol className="steps-list" id="steps-list" aria-label={copy.activity.stepsLabel}>
                    {shown.map((e, i) => (
                      <li key={`${e.at}-${i}`} className={working && i === shown.length - 1 ? 'step-now' : ''}>
                        <span className="step-dot" aria-hidden="true" />
                        {e.text}
                      </li>
                    ))}
                  </ol>
                )}
                {steps && <TaskReference id={task.id} />}
              </div>
            )}

            {task.result && (
              <div className="result">
                <h2 className="sr-only">{copy.task.resultHeading}</h2>
                <Markdown rendered={rendered} className="prose" />
                <StrippedNote parts={rendered.stripped} what={copy.stripped.answer} />
                {task.fallback_used && <FallbackNote />}
              </div>
            )}

            {task.status === 'failed' && (
              <div className="note note-error" role="alert">
                <Icon name="alert" className="note-icon" />
                <div className="note-body">
                  <p>
                    {copy.task.failedWhat} {task.error ?? copy.errors.unknown}
                  </p>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={asking}
                    onClick={() => void start(() => api.createTask(task.text, task.persona ?? undefined), copy.errors.askFailed)}
                  >
                    {copy.task.tryAgain}
                  </button>
                </div>
              </div>
            )}
            {retryError && <ErrorNote>{retryError}</ErrorNote>}
            {nothingToShow && (
              <Empty
                icon="sparkle"
                action={
                  <button type="button" className="btn btn-sm" disabled={asking} onClick={() => void start(() => api.createTask(task.text, task.persona ?? undefined), copy.errors.askFailed)}>
                    {copy.task.tryAgain}
                  </button>
                }
              >
                {copy.task.noResult}
              </Empty>
            )}

            {task.draft_ids.map((d) => <DraftCard key={d} id={d} fallback={task.fallback_used} />)}
          </div>
        </article>
      </section>
    </div>
    </>
  );
}
