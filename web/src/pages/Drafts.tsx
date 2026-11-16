import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import type { DraftStatus, DraftSummary } from '../api';
import { DraftPill, Empty, ErrorNote, NewRequestButton, PageHeader, Skeleton } from '../components/bits';
import { Icon, kindIcon } from '../components/icons';
import { copy } from '../copy';
import { useData } from '../data';
import { effectiveStatus } from '../drafts';
import { relTime, timeLeft } from '../time';
import { draftHref } from '../url';

type Filter = 'pending' | 'approved' | 'expired' | 'all';
const FILTERS: Filter[] = ['pending', 'approved', 'expired', 'all'];

const matches = (f: Filter, s: DraftStatus) => f === 'all' || s === f;

/** Keyboard support shared by every tab list: arrows move and select, Home and End jump. */
export function tabKeys<T>(e: KeyboardEvent, list: readonly T[], current: T, pick: (t: T) => void, vertical = false) {
  const i = list.indexOf(current);
  const next = e.key === (vertical ? 'ArrowDown' : 'ArrowRight') ? i + 1
    : e.key === (vertical ? 'ArrowUp' : 'ArrowLeft') ? i - 1
    : e.key === 'Home' ? 0
    : e.key === 'End' ? list.length - 1
    : null;
  if (next === null) return;
  e.preventDefault();
  pick(list[(next + list.length) % list.length]!);
}

function DraftRow({ d, now }: { d: DraftSummary; now: number }) {
  const status = effectiveStatus(d, now);
  const left = status === 'pending' ? timeLeft(d.expires_at, now) : null;
  return (
    <li>
      <a className="trow drafts-grid" href={draftHref(d.id)}>
        <span className="tcell tcell-title">
          <span className="row-icon"><Icon name={kindIcon(d.kind)} size={15} /></span>
          <span className="row-title">{d.title}</span>
          {d.practice && <span className="tag">{copy.home.practiceBadge}</span>}
        </span>
        <span className="tcell tcell-kind">{copy.home.kindLabel(d.kind)}</span>
        <span className="tcell tcell-status"><DraftPill status={status} /></span>
        <span className="tcell tcell-left">
          {left ? (
            <span className="row-left"><Icon name="clock" size={13} />{left}</span>
          ) : (
            <span className="tcell-none" aria-hidden="true">—</span>
          )}
        </span>
        <span className="tcell tcell-time">{relTime(d.created_at, now)}</span>
      </a>
    </li>
  );
}

/** Every draft, filtered by what it needs: Needs you, Approved, Expired, All. */
export function Drafts() {
  const { drafts, draftsError, reloadDrafts } = useData();
  const [filter, setFilter] = useState<Filter>('pending');
  const tabs = useRef<Record<string, HTMLButtonElement | null>>({});

  useEffect(() => {
    void reloadDrafts();
  }, [reloadDrafts]);

  const now = Date.now();
  const withStatus = (drafts ?? []).map((d) => ({ d, s: effectiveStatus(d, now) }));
  const count = (f: Filter) => withStatus.filter((x) => matches(f, x.s)).length;
  const shown = withStatus.filter((x) => matches(filter, x.s)).map((x) => x.d);

  const pick = (f: Filter) => {
    setFilter(f);
    tabs.current[f]?.focus();
  };

  const empty = {
    pending: drafts && drafts.length === 0 ? copy.emptyDrafts : copy.draftsPage.emptyPending,
    approved: copy.draftsPage.emptyApproved,
    expired: copy.draftsPage.emptyExpired,
    all: copy.emptyDrafts,
  }[filter];

  return (
    <div className="page">
      <PageHeader title={copy.draftsPage.title} lede={copy.draftsPage.lede}>
        <NewRequestButton />
      </PageHeader>
      <div className="tabs tabs-scroll" role="tablist" aria-label={copy.draftsPage.tabsLabel} onKeyDown={(e) => tabKeys(e, FILTERS, filter, pick)}>
        {FILTERS.map((f) => (
          <button
            key={f}
            ref={(el) => { tabs.current[f] = el; }}
            type="button"
            role="tab"
            id={`tab-${f}`}
            aria-selected={filter === f}
            aria-controls="drafts-panel"
            tabIndex={filter === f ? 0 : -1}
            className="tab"
            onClick={() => setFilter(f)}
          >
            {copy.draftsPage.tabs[f]}
            {drafts && <span className="tab-count">{count(f)}</span>}
          </button>
        ))}
      </div>
      <section id="drafts-panel" role="tabpanel" aria-labelledby={`tab-${filter}`} className="tabpanel">
        {draftsError && (
          <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadDrafts()}>
            {draftsError}
          </ErrorNote>
        )}
        {!drafts && !draftsError && <Skeleton rows={4} label={copy.home.loadingDrafts} />}
        {drafts && shown.length === 0 && <Empty icon="drafts">{empty}</Empty>}
        {shown.length > 0 && (
          <div className="table">
            <div className="thead drafts-grid" aria-hidden="true">
              <span>{copy.draftsPage.columns.title}</span>
              <span className="tcell-kind">{copy.draftsPage.columns.kind}</span>
              <span>{copy.draftsPage.columns.status}</span>
              <span className="tcell-left">{copy.draftsPage.columns.expires}</span>
              <span className="tcell-time">{copy.draftsPage.columns.created}</span>
            </div>
            <ul className="tbody">
              {shown.map((d) => <DraftRow key={d.id} d={d} now={now} />)}
            </ul>
          </div>
        )}
      </section>
    </div>
  );
}
