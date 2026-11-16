import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import type { Brief, DraftSummary, FollowUp, Task } from '../api';
import { messageOf, useApp } from '../app-context';
import { ErrorNote } from '../components/bits';
import { TaskPanel, isSettled } from '../components/TaskPanel';
import { copy } from '../copy';

function expiry(d: DraftSummary, now: number): string | null {
  if (!d.expires_at || d.status !== 'pending') return null;
  const ms = Date.parse(d.expires_at) - now;
  if (ms <= 0) return copy.home.expired;
  return copy.home.expiresIn(Math.floor(ms / 3600_000), Math.max(1, Math.floor(ms / 60_000)));
}

export function Home() {
  const { api, me, pollMs, navigate } = useApp();
  const firstVisit = useRef(me.first_visit).current;

  const [drafts, setDrafts] = useState<DraftSummary[] | null>(null);
  const [draftsError, setDraftsError] = useState<string | null>(null);
  const [brief, setBrief] = useState<Brief | null>(null);
  const [text, setText] = useState('');
  const [asking, setAsking] = useState(false);
  const [task, setTask] = useState<Task | null>(null);
  const [askError, setAskError] = useState<string | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);
  const practiceStarted = useRef(false);

  const loadDrafts = useCallback(async () => {
    try {
      setDrafts(await api.listDrafts());
      setDraftsError(null);
    } catch (e) {
      setDraftsError(messageOf(e, copy.errors.loadHomeFailed));
    }
  }, [api]);

  const loadBrief = useCallback(async () => {
    try {
      setBrief(await api.brief());
    } catch {
      // 501 not_built hides the brief entirely. Any other failure hides it too, quietly,
      // because the page works without it.
      setBrief(null);
    }
  }, [api]);

  useEffect(() => {
    void loadDrafts();
    void loadBrief();
  }, [loadDrafts, loadBrief]);

  // Onboarding: a first visit with no drafts opens the practice draft.
  useEffect(() => {
    if (!drafts || practiceStarted.current) return;
    if (me.first_visit && drafts.length === 0) {
      practiceStarted.current = true;
      api
        .createPractice()
        .then((d) => navigate(`/d/${d.id}`))
        .catch((e) => setDraftsError(messageOf(e, copy.errors.loadHomeFailed)));
    }
  }, [drafts, me.first_visit, api, navigate]);

  // Poll a working task until it settles.
  useEffect(() => {
    if (!task || isSettled(task)) return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const next = await api.getTask(task.id);
        if (!cancelled) setTask(next);
      } catch (e) {
        if (!cancelled) setAskError(messageOf(e, copy.errors.unknown));
      }
    }, pollMs);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [task, api, pollMs]);

  const settled = task ? isSettled(task) : false;
  useEffect(() => {
    if (settled) void loadDrafts();
  }, [settled, loadDrafts]);

  const start = async (run: () => Promise<Task>, fallback: string) => {
    setAsking(true);
    setAskError(null);
    try {
      setTask(await run());
    } catch (e) {
      setAskError(messageOf(e, fallback));
    } finally {
      setAsking(false);
    }
  };

  const ask = (e: FormEvent) => {
    e.preventDefault();
    const q = text.trim();
    if (!q) return;
    setText('');
    void start(() => api.createTask(q), copy.errors.askFailed);
  };

  const followupDraft = (f: FollowUp) => {
    setRowError(null);
    void start(() => api.draftFollowup(f.id), copy.errors.followupFailed);
  };

  const dismiss = async (f: FollowUp) => {
    setRowError(null);
    try {
      await api.dismissFollowup(f.id);
      setBrief((b) =>
        b && {
          ...b,
          waiting_on_you: b.waiting_on_you.filter((x) => x.id !== f.id),
          you_wait_on: b.you_wait_on.filter((x) => x.id !== f.id),
        },
      );
    } catch (e) {
      setRowError(messageOf(e, copy.errors.dismissFailed));
    }
  };

  const now = Date.now();
  const firstName = me.name.split(' ')[0] ?? me.name;

  const followRows = (rows: FollowUp[], empty: string, actionLabel: string) =>
    rows.length === 0 ? (
      <p className="muted">{empty}</p>
    ) : (
      <ul className="list">
        {rows.map((f) => (
          <li key={f.id} className="row-item">
            <div>
              <strong>{f.who}</strong> <span className="muted">({copy.home.ageDays(f.age_days)})</span>
              <div>{f.summary}</div>
              <a href={f.url} target="_blank" rel="noopener noreferrer">
                {copy.home.openSource}<span className="sr-only"> for {f.who}: {f.summary}</span>
              </a>
            </div>
            <div className="row">
              <button type="button" className="btn" onClick={() => followupDraft(f)} disabled={asking}>
                {actionLabel}<span className="sr-only"> to {f.who}</span>
              </button>
              <button type="button" className="btn btn-quiet" onClick={() => dismiss(f)}>
                {copy.home.notMine}<span className="sr-only">: {f.who}, {f.summary}</span>
              </button>
            </div>
          </li>
        ))}
      </ul>
    );

  return (
    <div className="stack">
      <header>
        <h1>{copy.greeting(firstName, new Date().getHours())}</h1>
        {firstVisit && drafts && drafts.length > 0 && <p className="lede">{copy.welcomeWithDrafts}</p>}
      </header>

      <form onSubmit={ask} className="ask" aria-label="Ask">
        <label htmlFor="ask-box" className="label-big">{copy.home.askLabel}</label>
        <div className="ask-row">
          <input
            id="ask-box"
            type="text"
            value={text}
            placeholder={copy.home.askPlaceholder}
            onChange={(e) => setText(e.target.value)}
            autoComplete="off"
          />
          <button type="submit" className="btn btn-primary" disabled={asking || !text.trim()}>
            {asking ? copy.home.asking : copy.home.askButton}
          </button>
        </div>
      </form>
      {askError && <ErrorNote>{askError}</ErrorNote>}
      {task && (
        <TaskPanel
          task={task}
          onRetry={() => void start(() => api.createTask(task.text), copy.errors.askFailed)}
          onClear={() => setTask(null)}
        />
      )}

      {brief && (
        <section aria-label="Follow-ups" className="stack">
          <div>
            <h2>{copy.home.waitingOnYou}</h2>
            {followRows(brief.waiting_on_you, copy.nothingWaiting, copy.home.draftReply)}
          </div>
          <div>
            <h2>{copy.home.youWaitOn}</h2>
            {followRows(brief.you_wait_on, copy.nothingWaitedOn, copy.home.draftNudge)}
          </div>
          {brief.changed.length > 0 && (
            <div>
              <h2>{copy.home.changed}</h2>
              <ul className="list">
                {brief.changed.map((c) => (
                  <li key={c.url} className="row-item">
                    <div>
                      <strong>{c.title}</strong>
                      <div>{c.summary}</div>
                      <a href={c.url} target="_blank" rel="noopener noreferrer">
                        {copy.home.openSource}<span className="sr-only">: {c.title}</span>
                      </a>
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {rowError && <ErrorNote>{rowError}</ErrorNote>}
        </section>
      )}

      <section aria-label="Drafts" className="stack">
        <h2>{copy.home.draftsHeading}</h2>
        {draftsError && (
          <ErrorNote actionLabel={copy.errors.retry} onAction={() => void loadDrafts()}>
            {draftsError}
          </ErrorNote>
        )}
        {!drafts && !draftsError && <p className="muted">{copy.home.loadingDrafts}</p>}
        {drafts && drafts.length === 0 && !me.first_visit && <p className="muted">{copy.emptyDrafts}</p>}
        {drafts && drafts.length > 0 && (
          <ul className="list">
            {drafts.map((d) => {
              const exp = expiry(d, now);
              return (
                <li key={d.id}>
                  <a className="draft-link" href={`#/d/${d.id}`}>
                    <span className="draft-title">{d.title}</span>
                    <span className="draft-meta">
                      <span className={`badge badge-${d.status}`}>{copy.home.statusLabel(d.status)}</span>
                      {d.practice && <span className="badge badge-practice">{copy.home.practiceBadge}</span>}
                      <span className="muted">{copy.home.kindLabel(d.kind)}</span>
                      {exp && <span className="muted">{exp}</span>}
                    </span>
                  </a>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}
