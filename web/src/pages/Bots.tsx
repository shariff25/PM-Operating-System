import { useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import type { BotCard, BotRoutine, WithdrawnBot } from '../api';
import { useApp } from '../app-context';
import { isUnknownOutcome, skillLabel, teamErrorText } from '../bots';
import { BotFace, NeedBadges, PreviewTag, ReadsPills, RoleLine, Words } from '../components/BotParts';
import { ErrorNote, PageHeader, Skeleton } from '../components/bits';
import { Mascot } from '../components/Mascot';
import { useShowNudge } from '../components/Nudge';
import { useToast } from '../components/Toasts';
import { copy } from '../copy';
import { useData } from '../data';
import { TeamDialog } from './admin/teamParts';
import type { Problem } from './admin/teamParts';
import { PHONE_QUERY, useMediaQuery } from '../theme';
import { dayOf, longTime, relTime } from '../time';

const B = copy.bots.page;

/** A selected row: an offered bot by key, or a withdrawn one. */
type Pick = { kind: 'bot'; key: string } | { kind: 'withdrawn'; key: string };

/** "6 drafts this month · last used today"; a null part is left out. */
function usageLine(u: { drafts_this_month: number; last_used_at: string | null }): string {
  return [copy.bots.draftsThisMonth(u.drafts_this_month), u.last_used_at ? copy.bots.lastUsed(relTime(u.last_used_at)) : null].filter(Boolean).join(' · ');
}

/** The name, a button that shows the bot's details beside the lists. */
function NameButton({ name, title, selected, onSelect }: { name: string; title?: string; selected: boolean; onSelect: () => void }) {
  return (
    <button type="button" className="bot-row-name" aria-pressed={selected} onClick={onSelect}>
      <span className="sr-only">{B.details(name)}</span>
      <strong aria-hidden="true">{name}</strong>
      {title && <span className="bot-row-title" aria-hidden="true"> · {title}</span>}
    </button>
  );
}

function TeamRow({ bot, selected, onSelect, onSettings, onRemove, busy }: {
  bot: BotCard;
  selected: boolean;
  onSelect: () => void;
  onSettings: (el: HTMLElement) => void;
  onRemove: (el: HTMLElement) => void;
  busy: boolean;
}) {
  return (
    <li className={`row bot-row ${selected ? 'bot-row-on' : ''}`} data-bot={bot.key}>
      <BotFace mascot={bot.mascot} size={40} />
      <span className="row-main">
        <NameButton name={bot.display_name} title={bot.title} selected={selected} onSelect={onSelect} />
        <span className="row-meta">{usageLine(bot)}</span>
      </span>
      {bot.preview && <PreviewTag short />}
      {bot.always_on ? (
        <span className="bot-always">{copy.bots.alwaysOn}</span>
      ) : (
        <span className="bot-actions">
          <button type="button" className="btn btn-sm" onClick={(e) => onSettings(e.currentTarget)}>
            <Words shown={B.settings} full={B.settingsLabel(bot.display_name)} />
          </button>
          <button type="button" className="btn btn-sm btn-danger-text" id={`remove-${bot.key}`} onClick={(e) => onRemove(e.currentTarget)} disabled={busy}>
            <Words shown={B.remove} full={B.removeLabel(bot.display_name)} />
          </button>
        </span>
      )}
    </li>
  );
}

function AvailableRow({ bot, selected, onSelect, onAdd, busy }: { bot: BotCard; selected: boolean; onSelect: () => void; onAdd: () => void; busy: boolean }) {
  return (
    <li className={`row bot-row ${selected ? 'bot-row-on' : ''}`} data-bot={bot.key}>
      <BotFace mascot={bot.mascot} size={40} />
      <span className="row-main">
        <NameButton name={bot.display_name} title={bot.title} selected={selected} onSelect={onSelect} />
        <span className="row-meta row-meta-wrap">{bot.summary}</span>
        <span className="bot-row-tags">
          {bot.new && <span className="tag">{B.newLabel}</span>}
          {bot.preview && <PreviewTag />}
          <NeedBadges needs={bot.needs} />
        </span>
      </span>
      <button type="button" className="btn btn-sm btn-primary bot-add" id={`add-${bot.key}`} onClick={onAdd} disabled={busy}>
        <Words shown={B.add} full={B.addLabel(bot.display_name)} />
      </button>
    </li>
  );
}

function WithdrawnRow({ w, selected, onSelect, onRemove, busy }: { w: WithdrawnBot; selected: boolean; onSelect: () => void; onRemove: (el: HTMLElement) => void; busy: boolean }) {
  return (
    <li className={`row bot-row bot-row-gone ${selected ? 'bot-row-on' : ''}`} data-bot={w.key}>
      <BotFace mascot="unknown" size={40} />
      <span className="row-main">
        <NameButton name={copy.bots.gone} selected={selected} onSelect={onSelect} />
        <span className="row-meta">{usageLine(w)}</span>
      </span>
      <span className="tag">{B.withdrawnHeading}</span>
      <button type="button" className="btn btn-sm btn-danger-text" onClick={(e) => onRemove(e.currentTarget)} disabled={busy}>
        <Words shown={B.remove} full={B.removeGoneLabel} />
      </button>
    </li>
  );
}

/** The detail panel: everything the card says, at more length. */
function Detail({ bot, headingRef }: { bot: BotCard; headingRef: RefObject<HTMLHeadingElement | null> }) {
  const showArt = useShowNudge();
  const unmet = bot.needs.filter((n) => !n.met);
  const since = bot.always_on ? copy.bots.alwaysOnTeam : bot.on_team && bot.added_at ? copy.bots.onTeamSince(dayOf(bot.added_at)) : copy.bots.picker.notOnTeam;
  return (
    <>
      <div className="bot-detail-head">
        {showArt && (
          <div className="bot-detail-art">
            <Mascot bot={bot.mascot} size={84} />
          </div>
        )}
        <div className="bot-detail-id">
          <RoleLine bot={bot} />
          <h2 ref={headingRef} tabIndex={-1} id="bot-detail-name">{bot.display_name}</h2>
          <p className="muted">{since}</p>
          {bot.preview && <PreviewTag />}
        </div>
      </div>
      <p className="bot-detail-summary">{bot.summary}</p>
      {unmet.length > 0 && (
        <ul className="bot-needs">
          {unmet.map((n, i) => (
            <li key={`${n.source}-${i}`} className="note note-warn">
              <div className="note-body">
                <p className="note-title">{copy.bots.needs(n.label)}</p>
                <p>{n.without}</p>
              </div>
            </li>
          ))}
        </ul>
      )}
      {bot.can_do.length > 0 && (
        <section className="bot-detail-part" aria-labelledby="bot-can">
          <h3 id="bot-can" className="micro-label">{B.canDo}</h3>
          <ul className="bot-skills">
            {bot.can_do.map((s) => (
              <li key={s} className="bot-skill">{skillLabel(s)}</li>
            ))}
          </ul>
        </section>
      )}
      {bot.reads.length > 0 && (
        <section className="bot-detail-part" aria-labelledby="bot-reads">
          <h3 id="bot-reads" className="micro-label">{B.mayRead}</h3>
          <ReadsPills reads={bot.reads} label={B.mayRead} />
        </section>
      )}
      <section className="bot-detail-part" aria-labelledby="bot-drafts">
        <h3 id="bot-drafts" className="micro-label">{B.mayDraft}</h3>
        <p className="bot-detail-text">{B.draftsSentence(bot.drafts)}</p>
      </section>
      {bot.examples.length > 0 && (
        <section className="bot-detail-part" aria-labelledby="bot-examples">
          <h3 id="bot-examples" className="micro-label">{B.examples}</h3>
          <ul className="bot-examples">
            {bot.examples.map((x, i) => (
              <li key={i}>“{x}”</li>
            ))}
          </ul>
        </section>
      )}
    </>
  );
}

function WithdrawnDetail({ w, headingRef }: { w: WithdrawnBot; headingRef: RefObject<HTMLHeadingElement | null> }) {
  return (
    <>
      <div className="bot-detail-head">
        <div className="bot-detail-art">
          <Mascot bot="unknown" size={64} />
        </div>
        <div className="bot-detail-id">
          <h2 ref={headingRef} tabIndex={-1} id="bot-detail-name">{copy.bots.gone}</h2>
          {w.added_at && <p className="muted">{copy.bots.onTeamSince(dayOf(w.added_at))}</p>}
        </div>
      </div>
      <p className="bot-detail-summary">{B.withdrawnLine}</p>
      <p className="muted">{usageLine(w)}</p>
    </>
  );
}

function RoutineList({ routines, name }: { routines: BotRoutine[] | null; name: string }) {
  if (routines === null) return <p className="muted">{copy.bots.settings.unknown}</p>;
  if (routines.length === 0) return <p className="muted">{copy.bots.settings.none(name)}</p>;
  return (
    <ul className="bot-routines">
      {routines.map((r) => (
        <li key={r.id}>
          <span className="bot-routine-prompt">“{r.prompt ?? ''}”</span>
          <span className="bot-routine-meta">
            {[r.cron && `${r.cron}${r.tz ? ` (${r.tz})` : ''}`, r.enabled ? (r.next_run_at ? copy.bots.settings.next(longTime(r.next_run_at)) : null) : copy.bots.settings.paused]
              .filter(Boolean)
              .join(' · ')}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** What a removal is about to remove. */
interface Removing {
  key: string;
  name: string;
  routines: BotRoutine[] | null;
}

/**
 * The Bots page: the person's own team, what else is offered, and bots no longer offered that are
 * still on the team. Add and Remove act at once (`PUT /api/me/team`, `via: "bots_page"`); removing
 * a bot that runs routines (or whose routines can't be read) first asks what to do with them.
 */
export function Bots() {
  const { api, refreshMe, me } = useApp();
  const { bots, botsError, reloadBots, setBots } = useData();
  const toast = useToast();
  const phone = useMediaQuery(PHONE_QUERY);
  const [pick, setPick] = useState<Pick | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<{ text: string; unknown: boolean } | null>(null);
  const [removing, setRemoving] = useState<Removing | null>(null);
  const [routineChoice, setRoutineChoice] = useState<'nudge' | 'pause'>('nudge');
  const [dialogProblem, setDialogProblem] = useState<Problem | null>(null);
  const [settings, setSettings] = useState<BotCard | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const detailHeading = useRef<HTMLHeadingElement | null>(null);

  useEffect(() => {
    void reloadBots();
  }, [reloadBots]);

  const all = bots?.bots ?? [];
  const team = all.filter((b) => b.on_team);
  const available = all.filter((b) => !b.on_team);
  const withdrawn = bots?.withdrawn ?? [];

  // The detail panel shows the chosen row, else the first bot on the team besides Nudge.
  const fallback: Pick | null = team.find((b) => !b.always_on) ? { kind: 'bot', key: team.find((b) => !b.always_on)!.key } : all[0] ? { kind: 'bot', key: all[0].key } : null;
  const current = pick && (pick.kind === 'bot' ? all.some((b) => b.key === pick.key) : withdrawn.some((w) => w.key === pick.key)) ? pick : fallback;
  const currentBot = current?.kind === 'bot' ? all.find((b) => b.key === current.key) ?? null : null;
  const currentGone = current?.kind === 'withdrawn' ? withdrawn.find((w) => w.key === current.key) ?? null : null;

  const select = (p: Pick) => {
    setPick(p);
    // On a phone the details are below the lists: take the reader there.
    if (phone) requestAnimationFrame(() => detailHeading.current?.focus());
  };

  const focusLater = (id: string) => requestAnimationFrame(() => document.getElementById(id)?.focus());

  const failed = (e: unknown) => {
    const unknown = isUnknownOutcome(e);
    setProblem({ text: teamErrorText(e), unknown });
    if (unknown) void reloadBots();
  };

  const add = async (bot: BotCard) => {
    setBusy(true);
    setProblem(null);
    try {
      setBots(await api.changeTeam({ add: [bot.key], via: 'bots_page' }));
      void refreshMe();
      toast(B.added(bot.display_name), 'good');
      setPick({ kind: 'bot', key: bot.key });
      focusLater(`remove-${bot.key}`);
    } catch (e) {
      failed(e);
    } finally {
      setBusy(false);
    }
  };

  const askRemove = (r: Removing, el: HTMLElement) => {
    opener.current = el;
    setRoutineChoice('nudge');
    setDialogProblem(null);
    setRemoving(r);
  };

  const closeRemove = () => {
    setRemoving(null);
    opener.current?.focus();
  };

  // Ask about routines when the bot has any, and when they couldn't be read.
  const asksRoutines = (r: Removing) => r.routines === null || r.routines.length > 0;

  const remove = async () => {
    if (!removing) return;
    const r = removing;
    setBusy(true);
    setDialogProblem(null);
    try {
      setBots(await api.changeTeam({ remove: [r.key], via: 'bots_page', ...(asksRoutines(r) ? { routines: routineChoice } : {}) }));
      void refreshMe();
      toast(B.removed(r.name), 'good');
      setRemoving(null);
      setProblem(null);
      // It is under Available now (or gone, if it was withdrawn).
      if (document.getElementById(`add-${r.key}`) || all.some((b) => b.key === r.key)) focusLater(`add-${r.key}`);
      else requestAnimationFrame(() => document.querySelector<HTMLElement>('.page-bots h1')?.focus());
    } catch (e) {
      const unknown = isUnknownOutcome(e);
      setDialogProblem({ text: teamErrorText(e), tone: unknown ? 'warn' : 'error' });
      if (unknown) void reloadBots();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page page-bots">
      <PageHeader eyebrow={B.eyebrow} title={B.title} lede={B.lede} />
      {me.team_unavailable && (
        <div className="note note-info" role="status">
          <div className="note-body"><p>{B.teamUnavailable}</p></div>
        </div>
      )}
      {problem && (
        <ErrorNote actionLabel={problem.unknown ? B.reload : undefined} onAction={problem.unknown ? () => { setProblem(null); void reloadBots(); void refreshMe(); } : undefined}>
          {problem.text}
        </ErrorNote>
      )}
      {!bots && botsError === null && <Skeleton rows={4} label={B.loading} />}
      {!bots && botsError !== null && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadBots()}>
          {teamErrorText(botsError, B.loadFailed)}
        </ErrorNote>
      )}
      {bots && (
        <div className="bots-grid">
          <div className="bots-lists">
            <section className="card" aria-labelledby="h-team">
              <div className="card-head">
                <h2 id="h-team">
                  {B.onTeam}
                  <span className="card-count">{team.length}</span>
                </h2>
              </div>
              <ul className="rows">
                {team.map((b) => (
                  <TeamRow
                    key={b.key}
                    bot={b}
                    busy={busy}
                    selected={current?.kind === 'bot' && current.key === b.key}
                    onSelect={() => select({ kind: 'bot', key: b.key })}
                    onSettings={(el) => {
                      opener.current = el;
                      setSettings(b);
                    }}
                    onRemove={(el) => askRemove({ key: b.key, name: b.display_name, routines: b.routines }, el)}
                  />
                ))}
              </ul>
            </section>

            <section className="card" aria-labelledby="h-available">
              <div className="card-head">
                <h2 id="h-available">
                  {B.available}
                  <span className="card-count">{available.length}</span>
                </h2>
              </div>
              {available.length === 0 ? (
                <p className="card-body muted">{B.noneAvailable}</p>
              ) : (
                <ul className="rows">
                  {available.map((b) => (
                    <AvailableRow
                      key={b.key}
                      bot={b}
                      busy={busy}
                      selected={current?.kind === 'bot' && current.key === b.key}
                      onSelect={() => select({ kind: 'bot', key: b.key })}
                      onAdd={() => void add(b)}
                    />
                  ))}
                </ul>
              )}
            </section>

            {withdrawn.length > 0 && (
              <section className="card" aria-labelledby="h-withdrawn">
                <div className="card-head">
                  <h2 id="h-withdrawn">
                    {B.withdrawnHeading}
                    <span className="card-count">{withdrawn.length}</span>
                  </h2>
                </div>
                <ul className="rows">
                  {withdrawn.map((w) => (
                    <WithdrawnRow
                      key={w.key}
                      w={w}
                      busy={busy}
                      selected={current?.kind === 'withdrawn' && current.key === w.key}
                      onSelect={() => select({ kind: 'withdrawn', key: w.key })}
                      onRemove={(el) => askRemove({ key: w.key, name: copy.bots.gone, routines: w.routines }, el)}
                    />
                  ))}
                </ul>
              </section>
            )}
          </div>

          {(currentBot || currentGone) && (
            <aside className="card bot-detail" aria-labelledby="bot-detail-name">
              {currentBot && <Detail bot={currentBot} headingRef={detailHeading} />}
              {currentGone && <WithdrawnDetail w={currentGone} headingRef={detailHeading} />}
            </aside>
          )}
        </div>
      )}

      {removing && (
        <TeamDialog
          title={copy.bots.remove.title(removing.name)}
          intro={copy.bots.remove.intro}
          onClose={closeRemove}
          onSubmit={() => void remove()}
          busy={busy}
          submitLabel={copy.bots.remove.confirm}
          busyLabel={copy.bots.remove.removing}
          problem={dialogProblem}
          danger
          testId="remove-bot"
        >
          {asksRoutines(removing) && (
            <fieldset className="reject bot-routine-choice">
              <legend>{copy.bots.remove.routinesHeading(removing.name, removing.routines?.length ?? null)}</legend>
              <label className="radio radio-hint">
                <input type="radio" name="routines" value="nudge" checked={routineChoice === 'nudge'} onChange={() => setRoutineChoice('nudge')} />
                <span>
                  {copy.bots.remove.toNudge}
                  <span className="muted block small">{copy.bots.remove.toNudgeHint}</span>
                </span>
              </label>
              <label className="radio radio-hint">
                <input type="radio" name="routines" value="pause" checked={routineChoice === 'pause'} onChange={() => setRoutineChoice('pause')} />
                <span>
                  {copy.bots.remove.pause}
                  <span className="muted block small">{copy.bots.remove.pauseHint}</span>
                </span>
              </label>
            </fieldset>
          )}
        </TeamDialog>
      )}

      {settings && (
        <TeamDialog
          title={copy.bots.settings.title(settings.display_name)}
          intro={copy.bots.settings.intro}
          onClose={() => {
            setSettings(null);
            opener.current?.focus();
          }}
          onSubmit={() => {
            setSettings(null);
            opener.current?.focus();
          }}
          busy={false}
          submitLabel={copy.bots.settings.close}
          busyLabel={copy.bots.settings.close}
          problem={null}
          hideCancel
          testId="bot-settings"
        >
          <h3>{copy.bots.settings.routines}</h3>
          <RoutineList routines={settings.routines} name={settings.display_name} />
        </TeamDialog>
      )}
    </div>
  );
}
