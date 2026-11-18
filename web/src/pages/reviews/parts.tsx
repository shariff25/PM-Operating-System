import type { ReactNode } from 'react';
import { Empty } from '../../components/bits';
import { Icon } from '../../components/icons';
import type { IconName } from '../../components/icons';
import { PageBar } from '../../components/Shell';
import { copy } from '../../copy';
import type { ReviewState } from '../../reviews';
import { draftHref } from '../../url';

/** Why a draft can't be reviewed: each its own calm sentence. */
export type GoneCode = keyof typeof copy.reviews.gone;
export const GONE_CODES = Object.keys(copy.reviews.gone) as GoneCode[];
export const isGone = (code: string): code is GoneCode => (GONE_CODES as string[]).includes(code);

const GONE_ICON: Record<GoneCode, IconName> = {
  own_draft: 'drafts',
  not_found: 'inbox',
  not_pending: 'check',
  expired: 'clock',
  forbidden: 'lock',
};

export function ReviewsCrumb({ current }: { current?: ReactNode }) {
  return (
    <nav aria-label={copy.panel.breadcrumb} className="crumbs">
      <a href="#/reviews">{copy.nav.reviews}</a>
      {current !== undefined && (
        <>
          <span className="crumb-sep" aria-hidden="true">/</span>
          <span className="crumb-current" aria-current="page">{current}</span>
        </>
      )}
    </nav>
  );
}

/**
 * A draft (or the whole Reviews area) this person can't review, and why, in one calm sentence.
 * No error styling: nothing went wrong. `id` is the draft, for "Open it in Drafts" on their own.
 */
export function ReviewCalm({ code, id }: { code: GoneCode; id?: string }) {
  const area = code === 'forbidden' && id === undefined;
  return (
    <>
      <PageBar crumb={<ReviewsCrumb />} />
      <div className="page page-narrow">
        {!area && (
          <a href="#/reviews" className="back">
            <Icon name="arrowLeft" size={16} />
            {copy.reviews.back}
          </a>
        )}
        <h1 className="sr-only">{copy.reviews.title}</h1>
        <div className="card review-calm" data-reason={code}>
          <Empty
            icon={GONE_ICON[code]}
            action={
              code === 'own_draft' && id ? (
                <a className="btn btn-sm" href={draftHref(id)}>
                  {copy.reviews.openOwn}
                </a>
              ) : undefined
            }
          >
            {copy.reviews.gone[code]}
          </Empty>
        </div>
      </div>
    </>
  );
}

const STATE_TONE: Record<ReviewState, string> = {
  todo: 'warn',
  stale: 'warn',
  approve: 'good',
  changes: 'neutral',
  restricted: 'neutral',
};

/** Where a draft stands for this reviewer, as a quiet pill. */
export function AdvicePill({ state }: { state: ReviewState }) {
  return (
    <span className={`pill pill-${STATE_TONE[state]} advice-${state}`}>
      <span className="pill-dot" aria-hidden="true" />
      {copy.reviews.state[state]}
    </span>
  );
}
