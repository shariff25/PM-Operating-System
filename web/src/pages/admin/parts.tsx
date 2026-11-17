import type { ReactNode } from 'react';
import { ADMIN_SECTIONS, canSee, sectionHref } from '../../admin/access';
import type { AdminSection } from '../../admin/access';
import { useApp } from '../../app-context';
import { Icon } from '../../components/icons';
import { useShell } from '../../components/Shell';
import { copy } from '../../copy';

/** The calm page for someone whose role does not include this part, or whose request was refused (403). */
export function NoAccess() {
  return (
    <div className="page page-narrow">
      <div className="card no-access">
        <div className="empty">
          <span className="empty-icon"><Icon name="shield" size={20} /></span>
          <h1 className="no-access-title">{copy.admin.noAccess.title}</h1>
          <p>{copy.admin.noAccess.body}</p>
          <a className="btn btn-sm" href="#/">{copy.admin.noAccess.back}</a>
        </div>
      </div>
    </div>
  );
}

/** On a phone the sections are a row of tabs under the page title; on a desktop the rail lists them. */
export function AdminTabs({ current }: { current: AdminSection }) {
  const { isPhone } = useShell();
  const { me } = useApp();
  if (!isPhone) return null;
  return (
    <nav aria-label={copy.admin.subnavLabel} className="tabs tabs-scroll admin-tabs">
      {ADMIN_SECTIONS.filter((s) => canSee(me, s)).map((s) => (
        <a key={s} href={sectionHref(s)} className="tab" aria-current={s === current ? 'page' : undefined}>
          {copy.admin.sections[s]}
        </a>
      ))}
    </nav>
  );
}

/** The heading block every Admin page starts with, and the phone tabs under it. */
export function AdminHead({ section, title, lede, children }: { section: AdminSection; title: string; lede: string; children?: ReactNode }) {
  return (
    <>
      <header className="page-head">
        <div className="page-head-text">
          <p className="eyebrow">{copy.admin.area}</p>
          <h1>{title}</h1>
          <p className="lede">{lede}</p>
        </div>
        {children}
      </header>
      <AdminTabs current={section} />
    </>
  );
}

/** A yes or no that does not rely on colour: a mark and words for screen readers. */
export function YesNo({ yes, yesLabel, noLabel }: { yes: boolean | null; yesLabel: string; noLabel: string }) {
  if (yes === null) {
    return (
      <span className="yn yn-unknown">
        <span aria-hidden="true">–</span>
        <span className="sr-only">{copy.admin.team.unknown}</span>
      </span>
    );
  }
  return yes ? (
    <span className="yn yn-yes">
      <Icon name="check" size={14} weight={2.25} />
      <span className="sr-only">{yesLabel}</span>
    </span>
  ) : (
    <span className="yn yn-no">
      <span aria-hidden="true">–</span>
      <span className="sr-only">{noLabel}</span>
    </span>
  );
}

const OUTCOME_TONE: Record<string, string> = {
  ok: 'neutral',
  denied: 'bad',
  pending: 'warn',
  approved: 'good',
  rejected: 'neutral',
  granted: 'good',
  started: 'accent',
  error: 'bad',
  failed: 'bad',
  expired: 'neutral',
};

/** An audit outcome as a quiet pill. A value this app does not know is shown as it came. */
export function OutcomePill({ outcome }: { outcome: string }) {
  return (
    <span className={`pill pill-${OUTCOME_TONE[outcome] ?? 'neutral'} pill-outcome-${outcome}`}>
      <span className="pill-dot" aria-hidden="true" />
      {copy.admin.audit.outcomes[outcome] ?? outcome}
    </span>
  );
}
