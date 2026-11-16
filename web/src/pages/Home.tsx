import { useEffect, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent, ReactNode } from 'react';
import type { DraftSummary, FollowUp } from '../api';
import { messageOf, useApp } from '../app-context';
import { Empty, ErrorNote, Skeleton } from '../components/bits';
import { Icon, kindIcon, systemIcon } from '../components/icons';
import { copy } from '../copy';
import { useAsk, useData } from '../data';
import { effectiveStatus } from '../drafts';
import { relTime, timeLeft } from '../time';
import { draftHref } from '../url';
import { TaskTable } from './Activity';

function dateLine(d: Date): string {
  return d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
}

/** The composer: multi-line, grows with the text, Enter sends and Shift+Enter adds a line. */
function Composer() {
  const { api } = useApp();
  const { start, asking, error } = useAsk();
  const [text, setText] = useState('');
  const box = useRef<HTMLTextAreaElement>(null);

  const send = () => {
    const q = text.trim();
    if (!q || asking) return;
    void start(() => api.createTask(q), copy.errors.askFailed).then((ok) => ok && setText(''));
  };

  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    send();
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    }
  };

  const pick = (s: string) => {
    setText(s);
    box.current?.focus();
  };

  return (
    <div className="composer-wrap">
      <form onSubmit={onSubmit} className="composer" aria-label="Ask">
        <label htmlFor="ask-box" className="sr-only">{copy.home.askLabel}</label>
        <textarea
          id="ask-box"
          ref={box}
          rows={2}
          value={text}
          placeholder={copy.home.askPlaceholder}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          autoComplete="off"
          aria-describedby="ask-help"
        />
        <div className="composer-foot">
          <span id="ask-help" className="composer-help">{copy.launch.newLine}</span>
          <button type="submit" className="send-btn" disabled={asking || !text.trim()} aria-label={asking ? copy.home.asking : copy.home.askButton}>
            <Icon name="arrowUp" size={18} />
          </button>
        </div>
      </form>
      <ul className="chips" aria-label={copy.launch.suggestionsLabel}>
        {copy.launch.suggestions.map((s) => (
          <li key={s}>
            <button type="button" className="chip" onClick={() => pick(s)}>
              {s}
            </button>
          </li>
        ))}
      </ul>
      {error && <ErrorNote>{error}</ErrorNote>}
    </div>
  );
}

function SummaryCard({
  title,
  count,
  href,
  linkLabel,
  icon,
  children,
  wide,
}: {
  title: string;
  count: number | null;
  href: string;
  linkLabel: string;
  icon: Parameters<typeof Icon>[0]['name'];
  children: ReactNode;
  wide?: boolean;
}) {
  return (
    <section className={`summary ${wide ? 'summary-wide' : ''}`} aria-label={title}>
      <header className="summary-head">
        <span className="summary-icon"><Icon name={icon} size={16} /></span>
        <h2>{title}</h2>
        {count !== null && <span className="summary-count">{count}</span>}
      </header>
      <div className="summary-body">{children}</div>
      <a className="summary-link" href={href}>
        {linkLabel}
        <Icon name="chevronRight" size={14} />
      </a>
    </section>
  );
}

function DraftMini({ d, now }: { d: DraftSummary; now: number }) {
  const left = timeLeft(d.expires_at, now);
  return (
    <li>
      <a className="mini-row" href={draftHref(d.id)}>
        <span className="mini-icon"><Icon name={kindIcon(d.kind)} size={16} /></span>
        <span className="mini-text">
          <span className="mini-title">{d.title}</span>
          <span className="mini-meta">
            {copy.home.kindLabel(d.kind)} · {left ?? relTime(d.created_at, now)}
          </span>
        </span>
      </a>
    </li>
  );
}

function FollowMini({ f }: { f: FollowUp }) {
  return (
    <li>
      <a className="mini-row" href="#/followups">
        <span className="mini-icon"><Icon name={systemIcon(f.system)} size={16} /></span>
        <span className="mini-text">
          <span className="mini-title">
            <strong>{f.who}</strong> {f.summary}
          </span>
          <span className="mini-meta">{copy.followups.age(f.age_days)}</span>
        </span>
      </a>
    </li>
  );
}

export function Home() {
  const { api, me, navigate } = useApp();
  const { drafts, draftsError, reloadDrafts, brief, briefState, tasks, reloadTasks } = useData();
  const firstVisit = useRef(me.first_visit).current;
  const [practiceError, setPracticeError] = useState<string | null>(null);
  const practiceStarted = useRef(false);

  useEffect(() => {
    void reloadDrafts();
    void reloadTasks();
  }, [reloadDrafts, reloadTasks]);

  // Onboarding: a first visit with no drafts opens the practice draft.
  useEffect(() => {
    if (!drafts || practiceStarted.current) return;
    if (me.first_visit && drafts.length === 0) {
      practiceStarted.current = true;
      api
        .createPractice()
        .then((d) => navigate(`/d/${d.id}`))
        .catch((e) => setPracticeError(messageOf(e, copy.errors.loadHomeFailed)));
    }
  }, [drafts, me.first_visit, api, navigate]);

  const now = Date.now();
  const firstName = me.name.split(' ')[0] ?? me.name;
  const pending = (drafts ?? []).filter((d) => effectiveStatus(d, now) === 'pending');
  const showBrief = briefState !== 'off';
  const recent = (tasks ?? []).slice(0, 4);

  return (
    <div className="page page-home">
      <header className="hero">
        <p className="hero-date">{dateLine(new Date())}</p>
        <h1>{copy.greeting(firstName, new Date().getHours())}</h1>
        {firstVisit && drafts && drafts.length > 0 && <p className="lede">{copy.welcomeWithDrafts}</p>}
      </header>

      <Composer />

      <div className={`summaries ${showBrief ? '' : 'summaries-single'}`} aria-label={copy.launch.summaryLabel} role="group">
        <SummaryCard
          title={copy.launch.draftsCard}
          count={drafts ? pending.length : null}
          href="#/drafts"
          linkLabel={copy.launch.viewDrafts}
          icon="drafts"
          wide={!showBrief}
        >
          {draftsError && (
            <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadDrafts()}>
              {draftsError}
            </ErrorNote>
          )}
          {practiceError && <ErrorNote>{practiceError}</ErrorNote>}
          {!drafts && !draftsError && <Skeleton rows={2} label={copy.home.loadingDrafts} />}
          {drafts && drafts.length === 0 && !me.first_visit && <p className="summary-empty">{copy.emptyDrafts}</p>}
          {drafts && drafts.length > 0 && pending.length === 0 && <p className="summary-empty">{copy.launch.noPendingDrafts}</p>}
          {pending.length > 0 && (
            <ul className="mini-list">
              {pending.slice(0, showBrief ? 2 : 3).map((d) => (
                <DraftMini key={d.id} d={d} now={now} />
              ))}
            </ul>
          )}
        </SummaryCard>

        {showBrief && (
          <SummaryCard
            title={copy.launch.waitingCard}
            count={brief ? brief.waiting_on_you.length : null}
            href="#/followups"
            linkLabel={copy.launch.viewFollowups}
            icon="inbox"
          >
            {!brief ? (
              <Skeleton rows={2} label={copy.home.loadingDrafts} />
            ) : brief.waiting_on_you.length === 0 ? (
              <p className="summary-empty">{copy.nothingWaiting}</p>
            ) : (
              <ul className="mini-list">
                {brief.waiting_on_you.slice(0, 2).map((f) => <FollowMini key={f.id} f={f} />)}
              </ul>
            )}
          </SummaryCard>
        )}
        {showBrief && (
          <SummaryCard
            title={copy.launch.waitedOnCard}
            count={brief ? brief.you_wait_on.length : null}
            href="#/followups"
            linkLabel={copy.launch.viewFollowups}
            icon="clock"
          >
            {!brief ? (
              <Skeleton rows={2} label={copy.home.loadingDrafts} />
            ) : brief.you_wait_on.length === 0 ? (
              <p className="summary-empty">{copy.nothingWaitedOn}</p>
            ) : (
              <ul className="mini-list">
                {brief.you_wait_on.slice(0, 2).map((f) => <FollowMini key={f.id} f={f} />)}
              </ul>
            )}
          </SummaryCard>
        )}
      </div>

      <section className="section" aria-labelledby="recent-heading">
        <div className="section-head">
          <h2 id="recent-heading">{copy.launch.recent}</h2>
          {recent.length > 0 && (
            <a className="section-link" href="#/activity">
              {copy.launch.viewActivity}
              <Icon name="chevronRight" size={14} />
            </a>
          )}
        </div>
        {!tasks && <Skeleton rows={3} label={copy.activity.loading} />}
        {tasks && recent.length === 0 && <Empty icon="activity">{copy.launch.noActivity}</Empty>}
        {recent.length > 0 && <TaskTable tasks={recent} label={copy.launch.recent} />}
      </section>
    </div>
  );
}
