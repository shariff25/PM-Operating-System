import { useState } from 'react';
import type { FollowUp } from '../api';
import { messageOf, useApp } from '../app-context';
import { Empty, ErrorNote, ExtLink, NewRequestButton, PageHeader, Skeleton, initials, systemTone } from '../components/bits';
import { Icon, systemIcon } from '../components/icons';
import { useToast } from '../components/Toasts';
import { copy } from '../copy';
import { briefTiming } from '../brief';
import { useAsk, useData } from '../data';
import { relTime } from '../time';

/** Who is waiting on the PM, who the PM waits on, and what changed, from the daily brief. */
export function FollowUps() {
  const { api, openPalette } = useApp();
  const { brief, setBrief } = useData();
  const { start, asking, error } = useAsk();
  const toast = useToast();
  const [rowError, setRowError] = useState<string | null>(null);

  const dismiss = async (f: FollowUp) => {
    setRowError(null);
    try {
      await api.dismissFollowup(f.id);
      setBrief((b) => ({
        ...b,
        waiting_on_you: b.waiting_on_you.filter((x) => x.id !== f.id),
        you_wait_on: b.you_wait_on.filter((x) => x.id !== f.id),
      }));
      toast(copy.toast.dismissed, 'good');
    } catch (e) {
      setRowError(messageOf(e, copy.errors.dismissFailed));
    }
  };

  const timing = brief ? briefTiming(brief) : null;
  const eyebrow =
    timing === 'never'
      ? copy.followups.countLineNever
      : timing === 'stale' && brief
        ? copy.followups.countLineStale(relTime(brief.delivered_at ?? brief.date))
        : copy.followups.countLine;
  // The calm one-liners are only true of a brief that came today.
  const calm = (joke: string) => (timing === 'today' || timing === 'unknown' ? joke : copy.today.briefStaleCard);

  const rows = (list: FollowUp[], empty: string, action: string) =>
    list.length === 0 ? (
      <Empty icon="inbox" small>{empty}</Empty>
    ) : (
      <div className="table">
        <div className="thead follow-grid" aria-hidden="true">
          <span>{copy.followups.columns.who}</span>
          <span>{copy.followups.columns.what}</span>
          <span className="tcell-time">{copy.followups.columns.age}</span>
          <span />
        </div>
        <ul className="tbody">
          {list.map((f) => (
            <li key={f.id} className="trow follow-grid">
              <span className="tcell follow-who">
                <span className={`wait-avatar wait-avatar-${systemTone(f.system)}`} aria-hidden="true">{initials(f.who).slice(0, 1)}</span>
                <strong>{f.who}</strong>
              </span>
              <div className="tcell follow-main">
                <span className="follow-line">{f.summary}</span>
                <span className="follow-meta">
                  <span className="follow-sys"><Icon name={systemIcon(f.system)} size={13} />{f.system}</span>
                  <span aria-hidden="true">·</span>
                  <ExtLink href={f.url} className="follow-link" label={` for ${f.who}: ${f.summary}`}>
                    {copy.home.openSource}
                    <Icon name="external" size={12} />
                  </ExtLink>
                </span>
              </div>
              <span className="tcell tcell-time">{copy.followups.age(f.age_days)}</span>
              <div className="follow-actions">
                <button type="button" className="btn btn-sm" onClick={() => void start(() => api.draftFollowup(f.id), copy.errors.followupFailed)} disabled={asking}>
                  {action}<span className="sr-only"> to {f.who}</span>
                </button>
                <button type="button" className="btn btn-sm btn-ghost" onClick={() => void dismiss(f)}>
                  {copy.home.notMine}<span className="sr-only">: {f.who}, {f.summary}</span>
                </button>
              </div>
            </li>
          ))}
        </ul>
      </div>
    );

  return (
    <div className="page">
      <PageHeader title={copy.followups.title} lede={copy.followups.lede} eyebrow={eyebrow}>
        <NewRequestButton />
      </PageHeader>
      {error && <ErrorNote>{error}</ErrorNote>}
      {rowError && <ErrorNote>{rowError}</ErrorNote>}
      {brief?.slack_withheld && <p className="lede" role="status">{copy.today.slackWithheld}</p>}
      {!brief ? (
        <Skeleton rows={3} label={copy.activity.loading} />
      ) : brief.waiting_on_you.length === 0 && brief.you_wait_on.length === 0 && brief.changed.length === 0 ? (
        <Empty
          icon="inbox"
          action={
            <button type="button" className="btn btn-sm" onClick={() => openPalette(copy.emptyActions.askOpenText)}>
              {copy.emptyActions.askOpen}
            </button>
          }
        >
          {timing === 'never' ? copy.followups.emptyNever : timing === 'stale' ? copy.followups.emptyStale : copy.followups.emptyAll}
        </Empty>
      ) : (
        <div className="stack">
          <section className="card card-table" aria-labelledby="fu-you">
            <div className="card-head">
              <h2 id="fu-you">
                {copy.home.waitingOnYou}
                <span className="card-count" aria-hidden="true">{brief.waiting_on_you.length}</span>
              </h2>
            </div>
            {rows(brief.waiting_on_you, calm(copy.nothingWaiting), copy.home.draftReply)}
          </section>
          <section className="card card-table" aria-labelledby="fu-them">
            <div className="card-head">
              <h2 id="fu-them">
                {copy.home.youWaitOn}
                <span className="card-count" aria-hidden="true">{brief.you_wait_on.length}</span>
              </h2>
            </div>
            {rows(brief.you_wait_on, calm(copy.nothingWaitedOn), copy.home.draftNudge)}
          </section>
          {brief.changed.length > 0 && (
            <section className="card card-table" aria-labelledby="fu-changed">
              <div className="card-head">
                <h2 id="fu-changed">{copy.home.changed}</h2>
              </div>
              <ul className="tbody">
                {brief.changed.map((c) => (
                  <li key={c.url} className="trow change-row">
                    <span className="timeline-when">{c.system}</span>
                    <div className="follow-main">
                      <p className="follow-line"><strong>{c.title}</strong></p>
                      <p className="follow-meta">
                        <span>{c.summary}</span>
                        <span aria-hidden="true">·</span>
                        <ExtLink href={c.url} className="follow-link" label={`: ${c.title}`}>
                          {copy.home.openSource}
                          <Icon name="external" size={13} />
                        </ExtLink>
                      </p>
                    </div>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      )}
    </div>
  );
}
