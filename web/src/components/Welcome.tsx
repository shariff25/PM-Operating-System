import { useEffect, useRef } from 'react';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { useTour } from '../onboarding';
import type { TourStep } from '../onboarding';
import { PHONE_QUERY, useMediaQuery } from '../theme';
import { draftHref } from '../url';
import { Icon } from './icons';
import { Nudge, useShowNudge } from './Nudge';

/** Put the cursor in the composer on Today, which is right below this card. */
function focusComposer() {
  const box = document.getElementById('ask-box');
  if (!box) return;
  box.scrollIntoView({ block: 'center' });
  box.focus();
}

function StepAction({ step, practiceId }: { step: TourStep; practiceId: string | null }) {
  const words = copy.tour.steps[step];
  if (step === 'team') {
    return (
      <a className="btn btn-sm welcome-action" href="#/start/team">
        {words.action}
        <Icon name="chevronRight" size={14} />
      </a>
    );
  }
  if (step === 'ask') {
    return (
      <button type="button" className="btn btn-sm welcome-action" onClick={focusComposer}>
        {words.action}
      </button>
    );
  }
  if (!practiceId) return null;
  return (
    <a className="btn btn-sm welcome-action" href={draftHref(practiceId)}>
      {words.action}
      <Icon name="chevronRight" size={14} />
    </a>
  );
}

/**
 * The guided start on Today: Nudge waves, says two sentences, and lists at most three steps.
 * Each step is done by doing it (src/onboarding.ts); progress comes from the server, so it
 * survives a reload. "Skip the tour" ends it; "Show hints again" in the user menu restarts it.
 */
export function Welcome({ practiceId }: { practiceId: string | null }) {
  const { me } = useApp();
  const { state, mark } = useTour();
  const showNudge = useShowNudge();
  const phone = useMediaQuery(PHONE_QUERY);
  const started = useRef(false);

  // A first visit records that the guided start began, so opening a draft (which ends the
  // first visit) does not end it too.
  useEffect(() => {
    if (!state.active || started.current || me.hints_seen.includes('tour_started')) return;
    started.current = true;
    void mark('tour_started');
  }, [state.active, me.hints_seen, mark]);

  if (!state.active) return null;
  const skip = () => void mark('tour_skipped');
  const firstName = me.name.split(' ')[0] || me.name;

  if (state.variant === 'auditor') {
    return (
      <section className="card welcome welcome-line" aria-label={copy.tour.label}>
        {showNudge && <Nudge pose="hello" size={32} />}
        <p className="welcome-line-text">{copy.tour.auditor.text}</p>
        <button type="button" className="btn btn-sm" onClick={skip}>
          {copy.tour.auditor.dismiss}
        </button>
      </section>
    );
  }

  const total = state.steps.length;
  const pointer =
    state.variant === 'reviewer'
      ? { text: copy.tour.reviewer.text, action: copy.tour.reviewer.action, href: '#/reviews', icon: 'review' as const }
      : state.variant === 'admin'
        ? { text: copy.tour.admin.text, action: copy.tour.admin.action, href: '#/admin/team', icon: 'shield' as const }
        : null;

  return (
    <section className={`card welcome ${showNudge ? '' : 'welcome-plain'}`} aria-labelledby="welcome-title">
      {showNudge && (
        <div className="welcome-art">
          <Nudge pose="hello" size={phone ? 44 : 76} />
        </div>
      )}
      <div className="welcome-main">
        <div className="welcome-head">
          <div className="welcome-head-text">
            <p className="welcome-eyebrow">
              <span className="eyebrow" aria-hidden="true">{copy.tour.label}</span>
              <span className="sr-only">{copy.tour.progressLabel(state.done, total)}</span>
              <span className="welcome-count" aria-hidden="true">{copy.tour.progress(state.done, total)}</span>
              <span className="welcome-bar" aria-hidden="true">
                {state.steps.map((s) => (
                  <span key={s.key} className={s.done ? 'welcome-bar-on' : ''} />
                ))}
              </span>
            </p>
            <h2 id="welcome-title">{copy.tour.hello(firstName)}</h2>
            <p className="welcome-lede">{total === 4 ? copy.tour.ledeFour : copy.tour.lede}</p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm welcome-skip" onClick={skip}>
            {copy.tour.skip}
          </button>
        </div>
        <ol className={`welcome-steps ${total === 4 ? 'welcome-steps-4' : ''}`} aria-label={copy.tour.stepsLabel}>
          {state.steps.map((s, i) => {
            const current = s.key === state.current;
            const words = copy.tour.steps[s.key];
            return (
              <li
                key={s.key}
                className={`welcome-step ${s.done ? 'welcome-step-done' : ''} ${current ? 'welcome-step-current' : ''}`}
                data-step={s.key}
                data-done={s.done ? 'true' : 'false'}
              >
                <span className="welcome-num" aria-hidden="true">
                  {s.done ? <Icon name="check" size={14} weight={2.6} /> : i + 1}
                </span>
                <div className="welcome-step-body">
                  <p className="welcome-step-title">
                    {s.done ? <span className="sr-only">{copy.tour.stepDone}</span> : current ? <span className="sr-only">{copy.tour.stepNext}</span> : null}
                    {words.title}
                  </p>
                  <p className="welcome-step-detail">{words.detail}</p>
                  {current && <StepAction step={s.key} practiceId={practiceId} />}
                </div>
              </li>
            );
          })}
        </ol>
        {pointer && (
          <p className="welcome-pointer">
            <Icon name={pointer.icon} size={15} />
            <span>{pointer.text}</span>
            <a href={pointer.href} className="welcome-pointer-link">
              {pointer.action}
              <Icon name="chevronRight" size={14} />
            </a>
          </p>
        )}
      </div>
    </section>
  );
}
