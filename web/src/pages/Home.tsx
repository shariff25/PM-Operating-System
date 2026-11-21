import { Suspense, lazy, useEffect, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent, ReactNode } from 'react';
import type { Brief, DraftSummary, FollowUp, Task } from '../api';
import { messageOf, useApp } from '../app-context';
import { briefTiming } from '../brief';
import type { BriefTiming } from '../brief';
import { Empty, ErrorNote, ExtLink, Skeleton, SystemTag, initials, kindSystem, systemTone } from '../components/bits';
import { Icon } from '../components/icons';
import { copy } from '../copy';
import { useAsk, useData } from '../data';
import { draftWhere, effectiveStatus } from '../drafts';
import { tourState } from '../onboarding';
import { usableProgress } from '../tasks';
import { relTime, timeLeft } from '../time';
import { draftHref, taskHref } from '../url';
import { TaskTable } from './Activity';
import { ApiError } from '../api';
import { cardOf, defaultPersona, identify, isUnknownOutcome, teamErrorText, teamOf } from '../bots';
import { BotBy } from '../components/BotParts';
import { BotPicker, pickerBot } from '../components/BotPicker';
import type { PickerBot } from '../components/BotPicker';
import { TeamNote } from '../components/TeamNote';

// The guided start is for new people only, so its card is a small chunk of its own.
const Welcome = lazy(() => import('../components/Welcome').then((m) => ({ default: m.Welcome })));

function dateLine(d: Date): string {
  return d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
}

/**
 * The composer: multi-line, grows with the text, Enter sends and Shift+Enter adds a line. With
 * more than Nudge on the team, a bot picker sits before the box (defaulting to the bot asked last,
 * `ask_persona`), the ask names that bot, and the example chips are its examples.
 */
function Composer() {
  const { api, me, refreshMe } = useApp();
  const { bots, setBots, reloadBots } = useData();
  const { start, asking, error, setError } = useAsk();
  const [text, setText] = useState('');
  const [persona, setPersona] = useState(() => defaultPersona(me));
  const [adding, setAdding] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);

  const team = teamOf(me);
  // A bot that left the team (removed elsewhere, or withdrawn) falls back to Nudge.
  const chosen = team.includes(persona) ? persona : 'nudge';
  const picker = team.length > 1;
  const teamBots: PickerBot[] = team.map((k) => {
    const card = cardOf(bots, k);
    if (card) return pickerBot(card);
    const who = identify(k === 'nudge' ? null : k, true, bots?.bots);
    return { key: k, name: who.name, mascot: who.mascot, title: '', summary: '', preview: false };
  });
  const others = (bots?.bots ?? []).filter((b) => !team.includes(b.key) && !b.always_on).map(pickerBot);
  const examples = cardOf(bots, chosen)?.examples ?? [];
  const chips = examples.length > 0 ? examples : copy.launch.suggestions;

  const send = () => {
    const q = text.trim();
    if (!q || asking) return;
    setNotice(null);
    void start(
      () => (picker ? api.createTask(q, chosen) : api.createTask(q)),
      copy.errors.askFailed,
      (e) => {
        // The bot left the team, or is no longer offered: read the team again, so the picker falls back to Nudge.
        if (e instanceof ApiError && (e.code === 'bot_not_on_team' || e.code === 'bot_unavailable')) {
          void refreshMe();
          void reloadBots();
        }
      },
    ).then((ok) => ok && setText(''));
  };

  const addBot = async (key: string): Promise<boolean> => {
    setAdding(key);
    setError(null);
    setNotice(null);
    try {
      setBots(await api.changeTeam({ add: [key], via: 'picker' }));
      await refreshMe();
      setPersona(key);
      const name = cardOf(bots, key)?.display_name;
      if (name) setNotice(copy.bots.picker.added(name));
      return true;
    } catch (e) {
      setError(teamErrorText(e));
      if (isUnknownOutcome(e)) {
        void refreshMe();
        void reloadBots();
      }
      return false;
    } finally {
      setAdding(null);
    }
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
    <section className="card composer" aria-label={copy.today.composerLabel}>
      <form onSubmit={onSubmit} className="composer-form">
        <label htmlFor="ask-box" className="micro-label">{copy.home.askLabel}</label>
        <div className="composer-row">
          {picker && (
            <BotPicker
              team={teamBots}
              others={others}
              value={chosen}
              onChange={(k) => {
                setPersona(k);
                setNotice(null);
              }}
              onAdd={addBot}
              adding={adding}
              disabled={asking}
            />
          )}
          <textarea
            id="ask-box"
            ref={box}
            rows={1}
            value={text}
            placeholder={copy.home.askPlaceholder}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKey}
            autoComplete="off"
            aria-describedby="ask-help"
          />
          <button type="submit" className="btn btn-primary btn-lg send-btn" disabled={asking || !text.trim()}>
            {asking ? copy.home.asking : copy.home.askButton}
            <Icon name="arrowRight" size={15} weight={2.2} />
          </button>
        </div>
        <p id="ask-help" className="sr-only">{copy.launch.newLine}</p>
      </form>
      {/* A chip fills the box and never sends: content can't send an ask on the PM's behalf. */}
      <ul className="chips" aria-label={copy.launch.suggestionsLabel}>
        {chips.map((s, i) => (
          <li key={`${i}-${s}`}>
            <button type="button" className="chip" onClick={() => pick(s)}>
              {s}
            </button>
          </li>
        ))}
      </ul>
      {notice && <p className="composer-notice" role="status">{notice}</p>}
      {me.team_unavailable && <p className="composer-notice muted">{copy.bots.picker.unavailableNote}</p>}
      <div className="composer-foot">
        <span className="composer-reads">{copy.today.readsFrom}</span>
        <ul className="composer-systems" aria-label={copy.today.readsFrom}>
          {copy.today.systems.map((s) => (
            <li key={s} className={`sys-pill sys-pill-${systemTone(s)}`}>{s}</li>
          ))}
        </ul>
        <span className="composer-safe">
          <Icon name="shield" size={14} />
          {copy.today.nothingSent}
        </span>
      </div>
      {error && <ErrorNote>{error}</ErrorNote>}
    </section>
  );
}

function Stat({ label, value, sub, lead, warn, subAlways }: { label: string; value: number | null; sub: string; lead?: boolean; warn?: boolean; subAlways?: boolean }) {
  return (
    <div className={`stat ${lead ? 'stat-lead' : ''}`}>
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value ?? '–'}</span>
      <span className={`stat-sub ${warn ? 'stat-sub-warn' : ''}`}>{value === null && !subAlways ? '' : sub}</span>
    </div>
  );
}

/** The soonest expiry among drafts that still need a decision, as "5 h left". */
function soonest(pending: DraftSummary[], now: number): string | null {
  const next = pending
    .filter((d) => d.expires_at)
    .sort((a, b) => Date.parse(a.expires_at!) - Date.parse(b.expires_at!))[0];
  return next ? timeLeft(next.expires_at, now) : null;
}

function Stats({ pending, brief, briefOn, timing, tasks, draftsLoaded, now }: {
  pending: DraftSummary[];
  brief: Brief | null;
  briefOn: boolean;
  timing: BriefTiming | null;
  tasks: Task[] | null;
  draftsLoaded: boolean;
  now: number;
}) {
  const next = soonest(pending, now);
  const draftsSub = pending.length === 0 ? copy.today.statSub.allDecided : next ? copy.today.statSub.nextExpiry(next) : copy.today.statSub.noExpiry;
  const draftsTile = <Stat lead label={copy.today.stats.drafts} value={draftsLoaded ? pending.length : null} sub={draftsSub} />;

  if (briefOn) {
    const waiting = brief?.waiting_on_you ?? [];
    const waitedOn = brief?.you_wait_on ?? [];
    const changed = brief?.changed ?? [];
    const oldest = waiting.reduce((m, f) => Math.max(m, f.age_days), 0);
    const names = [...new Set(waitedOn.map((f) => f.who))];
    const bySystem = new Map<string, number>();
    for (const c of changed) bySystem.set(c.system, (bySystem.get(c.system) ?? 0) + 1);
    // No brief has ever run: the counts are not zero, they are not there yet.
    const never = timing === 'never';
    const systems = [...bySystem].map(([s, n]) => copy.today.statSub.inSystem(n, s.charAt(0).toUpperCase() + s.slice(1)));
    return (
      <section className="stats" aria-label={copy.today.statsLabel}>
        {draftsTile}
        <Stat
          label={copy.today.stats.waiting}
          value={brief && !never ? waiting.length : null}
          sub={never ? copy.today.briefNeverSub : waiting.length ? copy.today.statSub.oldest(copy.followups.age(oldest).toLowerCase()) : copy.today.statSub.nobody}
          subAlways={never}
          warn={oldest >= 2}
        />
        <Stat
          label={copy.today.stats.waitedOn}
          value={brief && !never ? waitedOn.length : null}
          sub={never ? copy.today.briefNeverSub : names.length ? names.slice(0, 3).join(', ') : copy.today.statSub.nobody}
          subAlways={never}
        />
        <Stat
          label={copy.today.stats.changed}
          value={brief && !never ? changed.length : null}
          sub={never ? copy.today.briefNeverSub : systems.length ? copy.today.statSub.bySystem(systems) : copy.today.statSub.quiet}
          subAlways={never}
        />
      </section>
    );
  }

  // Without the brief, the row is built from what exists: drafts and the request list.
  const day = new Date(now).toDateString();
  const running = (tasks ?? []).filter((t) => t.status === 'queued' || t.status === 'working').length;
  const doneToday = (tasks ?? []).filter((t) => (t.status === 'done' || t.status === 'needs_approval') && new Date(t.updated_at).toDateString() === day).length;
  return (
    <section className="stats stats-3" aria-label={copy.today.statsLabel}>
      {draftsTile}
      <Stat label={copy.today.stats.running} value={tasks ? running : null} sub={running ? copy.today.statSub.running : copy.today.statSub.nothingRunning} />
      <Stat label={copy.today.stats.done} value={tasks ? doneToday : null} sub={doneToday ? copy.today.statSub.done : copy.today.statSub.noneDone} />
    </section>
  );
}

function CardHead({ id, title, href, linkLabel, count }: { id: string; title: string; href?: string; linkLabel?: string; count?: number }) {
  return (
    <div className="card-head">
      <h2 id={id}>
        {title}
        {count !== undefined && count > 0 && <span className="card-count" aria-hidden="true">{count}</span>}
      </h2>
      {href && linkLabel && (
        <a className="card-link" href={href}>
          {linkLabel}
          <Icon name="chevronRight" size={14} />
        </a>
      )}
    </div>
  );
}

function DecideRow({ d, now }: { d: DraftSummary; now: number }) {
  const left = timeLeft(d.expires_at, now);
  const where = draftWhere(d);
  const whereText = [copy.home.kindLabel(where.kind), where.channel].filter(Boolean).join(' · ');
  return (
    <li>
      <a className="row decide-row" href={draftHref(d.id)}>
        <SystemTag system={kindSystem(d.kind)} label={kindSystem(d.kind)} />
        <span className="row-main">
          <span className="row-title">{d.title}</span>
          <span className="row-meta">{whereText}</span>
        </span>
        {d.practice && <span className="tag">{copy.home.practiceBadge}</span>}
        <BotBy persona={d.persona} offered={d.persona_offered} className="row-bot" />
        <span className={`row-when ${left ? 'row-when-warn' : ''}`}>
          {left ? <><Icon name="clock" size={13} />{left}</> : relTime(d.created_at, now)}
        </span>
        <span className="row-action" aria-hidden="true">{copy.today.review}</span>
      </a>
    </li>
  );
}

/** A request that is still working, at the foot of the decision list. */
function RunningRow({ t }: { t: Task }) {
  const p = usableProgress(t);
  return (
    <a className="run-row" href={taskHref(t.id)}>
      <span className="live-dot" aria-hidden="true" />
      <span className="run-text">{copy.today.working(t.text)}</span>
      {p && (
        <span
          className="run-bar"
          role="progressbar"
          aria-label={copy.task.progressLabel}
          aria-valuemin={0}
          aria-valuemax={p.total}
          aria-valuenow={p.done}
          aria-valuetext={copy.task.progress(p.done, p.total)}
        >
          <span className="run-fill" style={{ width: `${(p.done / p.total) * 100}%` }} />
        </span>
      )}
      {p && <span className="run-count" aria-hidden="true">{copy.task.progress(p.done, p.total)}</span>}
    </a>
  );
}

function WaitingRow({ f, onDraft, busy }: { f: FollowUp; onDraft: () => void; busy: boolean }) {
  const system = f.system.charAt(0).toUpperCase() + f.system.slice(1);
  return (
    <li className="row wait-row">
      <span className={`wait-avatar wait-avatar-${systemTone(f.system)}`} aria-hidden="true">{initials(f.who).slice(0, 1)}</span>
      <span className="row-main">
        <span className="row-title row-title-wrap">{f.who} {f.summary}</span>
        <span className={`row-meta ${f.age_days >= 2 ? 'row-meta-warn' : ''}`}>
          {system}, {copy.followups.age(f.age_days).toLowerCase()}
        </span>
      </span>
      <button type="button" className="btn btn-sm" onClick={onDraft} disabled={busy}>
        {copy.home.draftReply}<span className="sr-only"> to {f.who}</span>
      </button>
    </li>
  );
}

function Card({ id, className, children }: { id: string; className?: string; children: ReactNode }) {
  return (
    <section className={`card ${className ?? ''}`} aria-labelledby={id}>
      {children}
    </section>
  );
}

export function Home() {
  const { api, me, openPalette, navigate } = useApp();
  const { drafts, draftsError, reloadDrafts, brief, briefState, tasks, reloadTasks, takeTeamStep } = useData();
  const { start, asking, error: followError } = useAsk();
  const [practiceError, setPracticeError] = useState<string | null>(null);
  const practiceStarted = useRef(false);
  const tour = tourState(me);

  // "Pick your team" comes first in the guided start: Today sends the person there once. Coming
  // back to Today without picking is never blocked; the welcome card points at the step instead.
  const toTeamStep = me.show_team_step === true && tour.active;
  useEffect(() => {
    if (toTeamStep && takeTeamStep()) navigate('/start/team');
    // navigate only writes to location.hash
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toTeamStep, takeTeamStep]);

  useEffect(() => {
    void reloadDrafts();
    void reloadTasks();
  }, [reloadDrafts, reloadTasks]);

  // The guided start practises on the practice draft, so nothing real is sent: make one when
  // it is on (or on a first visit with no drafts) and there is none yet. It is not opened by
  // itself; the welcome card's steps point at it.
  const practice = (drafts ?? []).find((d) => d.practice && d.status === 'pending') ?? (drafts ?? []).find((d) => d.practice) ?? null;
  const wantsPractice = tour.variant !== 'auditor' && (tour.active || (me.first_visit && drafts?.length === 0));
  useEffect(() => {
    if (!drafts || practiceStarted.current || !wantsPractice || practice) return;
    practiceStarted.current = true;
    api
      .createPractice()
      .then(() => reloadDrafts())
      .catch((e) => setPracticeError(messageOf(e, copy.errors.loadHomeFailed)));
  }, [drafts, wantsPractice, practice, api, reloadDrafts]);

  const now = Date.now();
  const firstName = me.name.split(' ')[0] ?? me.name;
  const pending = (drafts ?? []).filter((d) => effectiveStatus(d, now) === 'pending');
  const briefOn = briefState !== 'off';
  const timing = brief ? briefTiming(brief, now) : null;
  const running = (tasks ?? []).filter((t) => t.status === 'queued' || t.status === 'working');
  const recent = (tasks ?? []).slice(0, 4);
  // The headline counts what needs the PM: drafts to decide, and people waiting on them.
  const known = drafts !== null && briefState !== 'loading';
  const needs = pending.length + (brief?.waiting_on_you.length ?? 0);
  const greeting = copy.greeting(firstName, new Date().getHours());

  return (
    <div className="page page-today">
      <header className="today-head">
        <div className="today-head-text">
          <p className="eyebrow">{dateLine(new Date())}</p>
          <h1>{known ? `${greeting} ${copy.today.needs(needs)}` : greeting}</h1>
        </div>
        {brief && briefState === 'ready' && timing !== 'unknown' && (
          <p className={`today-brief ${timing === 'today' ? '' : 'today-brief-quiet'}`}>
            <span className="accent-dot" aria-hidden="true" />
            {timing === 'today'
              ? copy.today.briefReady
              : timing === 'stale'
                ? copy.today.briefStale(relTime(brief.delivered_at ?? brief.date, now))
                : copy.today.briefNever}
          </p>
        )}
        {brief?.slack_withheld && (
          <p className="today-brief today-brief-quiet" role="status">
            {copy.today.slackWithheld}
          </p>
        )}
      </header>

      {tour.active && (
        <Suspense fallback={null}>
          <Welcome practiceId={practice?.id ?? null} />
        </Suspense>
      )}

      {me.show_team_note === true && !tour.active && <TeamNote />}

      <Composer />

      <Stats pending={pending} brief={brief} briefOn={briefOn} timing={timing} tasks={tasks} draftsLoaded={drafts !== null} now={now} />

      <div className={`today-grid ${briefOn ? '' : 'today-grid-even'}`}>
        <Card id="h-decide" className="card-decide">
          <CardHead id="h-decide" title={copy.today.decideHeading} href="#/drafts" linkLabel={copy.today.allDrafts} count={pending.length} />
          {draftsError && (
            <div className="card-body">
              <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadDrafts()}>
                {draftsError}
              </ErrorNote>
            </div>
          )}
          {practiceError && <div className="card-body"><ErrorNote>{practiceError}</ErrorNote></div>}
          {!drafts && !draftsError && <div className="card-body"><Skeleton rows={3} label={copy.home.loadingDrafts} /></div>}
          {drafts && pending.length === 0 && (
            <Empty
              icon="check"
              action={
                <button type="button" className="btn btn-sm" onClick={() => openPalette(copy.emptyActions.askDraftText)}>
                  {copy.emptyActions.askDraft}
                </button>
              }
            >
              {drafts.length === 0 && !me.first_visit ? copy.emptyDrafts : copy.launch.noPendingDrafts}
            </Empty>
          )}
          {pending.length > 0 && (
            <ul className="rows" aria-label={copy.today.decideHeading}>
              {pending.slice(0, 6).map((d) => <DecideRow key={d.id} d={d} now={now} />)}
            </ul>
          )}
          {running.length > 0 && (
            <div className="run-foot">
              {running.slice(0, 2).map((t) => <RunningRow key={t.id} t={t} />)}
            </div>
          )}
        </Card>

        <div className="today-side">
          {briefOn && (
            <Card id="h-wait">
              <CardHead id="h-wait" title={copy.today.waitingHeading} href="#/followups" linkLabel={copy.today.allFollowups} count={brief?.waiting_on_you.length} />
              {followError && <div className="card-body"><ErrorNote>{followError}</ErrorNote></div>}
              {!brief ? (
                <div className="card-body"><Skeleton rows={2} label={copy.home.loadingDrafts} /></div>
              ) : brief.waiting_on_you.length === 0 ? (
                <Empty icon="inbox" small nudge={timing === 'today' && !tour.active ? 'calm' : undefined}>
                  {timing === 'today' ? copy.nothingWaiting : timing === 'stale' ? copy.today.briefStaleCard : copy.today.briefNeverCard}
                </Empty>
              ) : (
                <ul className="rows" aria-label={copy.today.waitingHeading}>
                  {brief.waiting_on_you.slice(0, 4).map((f) => (
                    <WaitingRow key={f.id} f={f} busy={asking} onDraft={() => void start(() => api.draftFollowup(f.id), copy.errors.followupFailed)} />
                  ))}
                </ul>
              )}
            </Card>
          )}
          {briefOn && (
            <Card id="h-changed">
              <CardHead id="h-changed" title={copy.today.changedHeading} />
              {!brief ? (
                <div className="card-body"><Skeleton rows={2} label={copy.home.loadingDrafts} /></div>
              ) : brief.changed.length === 0 ? (
                <Empty icon="activity" small>{timing === 'today' ? copy.today.statSub.quiet : timing === 'stale' ? copy.today.briefStaleCard : copy.today.briefNeverCard}</Empty>
              ) : (
                <ol className="timeline">
                  {brief.changed.map((c) => (
                    <li key={c.url}>
                      <span className="timeline-when">{c.system}</span>
                      <span className="timeline-body">
                        <span className="timeline-title">{c.title}</span>
                        <span className="timeline-meta">
                          {c.summary}{' '}
                          <ExtLink href={c.url} className="timeline-link" label={`: ${c.title}`}>
                            {copy.home.openSource}
                          </ExtLink>
                        </span>
                      </span>
                    </li>
                  ))}
                </ol>
              )}
            </Card>
          )}
          <Card id="h-recent">
            <CardHead id="h-recent" title={copy.today.recentHeading} href={recent.length > 0 ? '#/activity' : undefined} linkLabel={copy.today.allActivity} />
            {!tasks && <div className="card-body"><Skeleton rows={3} label={copy.activity.loading} /></div>}
            {tasks && recent.length === 0 && <Empty icon="sparkle" small>{copy.launch.noActivity}</Empty>}
            {recent.length > 0 && <TaskTable tasks={recent} label={copy.launch.recent} compact />}
          </Card>
        </div>
      </div>
    </div>
  );
}
