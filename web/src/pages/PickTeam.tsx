import { useEffect, useRef, useState } from 'react';
import type { BotCard } from '../api';
import { useApp } from '../app-context';
import { isUnknownOutcome, teamErrorText } from '../bots';
import { NeedBadges, PreviewTag, ReadsPills, RoleLine, Words } from '../components/BotParts';
import { ErrorNote, Skeleton } from '../components/bits';
import { Icon } from '../components/icons';
import { Mascot } from '../components/Mascot';
import { useShowNudge } from '../components/Nudge';
import { copy } from '../copy';
import { useData } from '../data';
import { tourState } from '../onboarding';

const P = copy.bots.pick;

/** One bot's card: its mascot, role line, name, summary, examples, what it reads, and the toggle. */
function PickCard({ bot, chosen, onToggle, busy }: { bot: BotCard; chosen: boolean; onToggle: () => void; busy: boolean }) {
  const showArt = useShowNudge();
  const id = `pick-${bot.key}`;
  const fixed = bot.always_on || bot.on_team;
  return (
    <li className={`pick-card ${chosen ? `pick-card-on pick-card-on-${bot.mascot}` : ''}`} aria-labelledby={`${id}-name`} data-bot={bot.key}>
      {showArt && (
        <div className="pick-art">
          <Mascot bot={bot.mascot} size={84} />
          {chosen && (
            <span className={`pick-check pick-check-${bot.mascot}`} aria-hidden="true">
              <Icon name="check" size={13} weight={2.6} />
            </span>
          )}
        </div>
      )}
      <div className="pick-body">
        <RoleLine bot={bot} className="pick-role" />
        <h2 id={`${id}-name`} className="pick-name">{bot.display_name}</h2>
        {bot.preview && <PreviewTag />}
        <p className="pick-summary">{bot.summary}</p>
        {bot.examples.length > 0 && (
          <ul className="pick-examples" aria-label={P.examplesLabel(bot.display_name)}>
            {bot.examples.slice(0, 3).map((x, i) => (
              <li key={i}>“{x}”</li>
            ))}
          </ul>
        )}
      </div>
      <div className="pick-foot">
        <div className="pick-reads">
          <span className="pick-reads-label" aria-hidden="true">{P.readsLabel}</span>
          <ReadsPills reads={bot.reads} label={P.readsLabel} />
        </div>
        {bot.needs.some((n) => !n.met) && (
          <div className="pick-needs">
            <NeedBadges needs={bot.needs} />
          </div>
        )}
        {fixed ? (
          // Nudge is always on; a bot already on the team stays (removing one is the Bots page's, which asks about its routines).
          <button type="button" className="btn btn-block pick-toggle pick-toggle-fixed" disabled>
            <Words shown={bot.always_on ? copy.bots.alwaysOnTeam : P.onTeam} full={copy.bots.named(bot.display_name, bot.always_on ? copy.bots.alwaysOnTeam : P.onTeam)} />
          </button>
        ) : (
          <button
            type="button"
            className={`btn btn-block pick-toggle ${chosen ? 'btn-primary' : ''}`}
            aria-pressed={chosen}
            onClick={onToggle}
            disabled={busy}
          >
            {chosen && <Icon name="check" size={15} weight={2.4} />}
            <Words shown={chosen ? P.added : P.add} full={copy.bots.named(bot.display_name, chosen ? P.added : P.add)} />
          </button>
        )}
      </div>
    </li>
  );
}

/**
 * "Pick your team": the first of the guided start's four steps, shown once when the server says
 * so (`show_team_step`). Nudge is always on; the others toggle. "Continue" saves the team with
 * `via: "onboarding"` and then marks `team_picked`; "Skip for now" marks `team_skipped` only.
 * Nothing here blocks the rest of the guided start: a failure keeps the page and says so, and
 * Skip always works.
 */
export function PickTeam() {
  const { api, me, setMe, navigate } = useApp();
  const { bots, botsError, reloadBots, setBots } = useData();
  const [chosen, setChosen] = useState<Set<string> | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const tour = tourState(me);
  const total = Math.max(tour.steps.length, 4);

  useEffect(() => {
    void reloadBots();
  }, [reloadBots]);

  useEffect(() => {
    heading.current?.focus();
  }, []);

  // The cards start as the team is now: Nudge, and anyone already added.
  useEffect(() => {
    if (bots && chosen === null) setChosen(new Set(bots.bots.filter((b) => b.on_team).map((b) => b.key)));
  }, [bots, chosen]);

  const list = bots?.bots ?? [];
  const picked = list.filter((b) => b.always_on || chosen?.has(b.key));
  const names = picked.map((b) => b.display_name);

  const toggle = (key: string) =>
    setChosen((c) => {
      const next = new Set(c ?? []);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const mark = async (seen: 'team_picked' | 'team_skipped') => {
    try {
      setMe(await api.markHintSeen(seen));
    } catch {
      /* the mark is a help: the step shows again on Today, never blocking it */
    }
  };

  const onContinue = async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    const add = list.filter((b) => !b.always_on && !b.on_team && chosen?.has(b.key)).map((b) => b.key);
    try {
      // With nothing new chosen there is nothing to save: a change names at least one bot.
      if (add.length > 0) setBots(await api.changeTeam({ add, via: 'onboarding' }));
    } catch (e) {
      // A failed save keeps the page and marks nothing.
      setError(teamErrorText(e, P.failed));
      // The answer was lost: read the team again, so the cards show what is saved.
      if (isUnknownOutcome(e)) {
        setChosen(null);
        void reloadBots();
      }
      setSaving(false);
      return;
    }
    await mark('team_picked');
    setSaving(false);
    navigate('/');
  };

  const onSkip = async () => {
    await mark('team_skipped');
    navigate('/');
  };

  return (
    <div className="page page-pick">
      <header className="pick-head">
        <div className="pick-head-text">
          <p className="pick-eyebrow">
            <span className="eyebrow">{P.eyebrow} · {P.step(1, total)}</span>
            <span className="welcome-bar pick-bar" aria-hidden="true">
              {Array.from({ length: total }, (_, i) => (
                <span key={i} className={i === 0 ? 'welcome-bar-on' : ''} />
              ))}
            </span>
          </p>
          <h1 ref={heading} tabIndex={-1}>{P.title}</h1>
          <p className="lede">{P.lede}</p>
        </div>
        <button type="button" className="btn btn-ghost pick-skip" onClick={() => void onSkip()} disabled={saving}>
          {P.skip}
        </button>
      </header>

      {!bots && !botsError && <Skeleton rows={3} variant="cards" label={P.loading} />}
      {!bots && botsError !== null && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadBots()}>
          {P.loadFailed}
        </ErrorNote>
      )}

      {bots && (
        <>
          <ul className="pick-grid" aria-label={P.cardsLabel}>
            {list.map((b) => (
              <PickCard key={b.key} bot={b} chosen={b.always_on || !!chosen?.has(b.key)} onToggle={() => toggle(b.key)} busy={saving} />
            ))}
          </ul>
          {error && <ErrorNote>{error}</ErrorNote>}
          <section className="card pick-summary-bar" aria-label={P.footHead(picked.length)}>
            <div className="pick-faces" aria-hidden="true">
              {picked.slice(0, 4).map((b) => (
                <Mascot key={b.key} bot={b.mascot} size={32} />
              ))}
            </div>
            <p className="pick-summary-text" aria-live="polite">
              <strong>{P.footHead(picked.length)}</strong> {P.footNames(names)} <span className="muted">{P.footSafe}</span>
            </p>
            <button type="button" className="btn btn-accent btn-lg pick-continue" onClick={() => void onContinue()} disabled={saving || chosen === null}>
              {saving ? P.saving : P.continue(picked.length)}
              {!saving && <Icon name="arrowRight" size={15} weight={2.2} />}
            </button>
          </section>
        </>
      )}
    </div>
  );
}
