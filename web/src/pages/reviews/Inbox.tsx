import { useEffect } from 'react';
import { ApiError } from '../../api';
import type { ReviewSummary } from '../../api';
import { Avatar, Empty, ErrorNote, PageHeader, Skeleton, SystemTag, kindSystem } from '../../components/bits';
import { BotBy } from '../../components/BotParts';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { useData } from '../../data';
import { errorText } from '../../errors';
import { isRestricted, reviewState } from '../../reviews';
import { relTime, timeLeft } from '../../time';
import { reviewHref } from '../../url';
import { AdvicePill, ReviewCalm } from './parts';

function listOf(handles: readonly string[]): string {
  try {
    return new Intl.ListFormat('en', { style: 'long', type: 'conjunction' }).format(handles);
  } catch {
    return handles.join(', ');
  }
}

function ReviewRow({ r, now }: { r: ReviewSummary; now: number }) {
  const restricted = isRestricted(r);
  const left = timeLeft(r.expires_at, now);
  const meta = [copy.home.kindLabel(r.kind), !restricted && r.edited ? copy.reviews.editedByOwner : null].filter(Boolean).join(' · ');
  return (
    <li>
      <a className={`trow reviews-grid ${restricted ? 'trow-restricted' : ''}`} href={reviewHref(r.id)}>
        <span className="tcell tcell-title">
          <SystemTag system={kindSystem(r.kind)} label={kindSystem(r.kind)} />
          <span className="row-main">
            {restricted ? (
              <span className="row-title row-title-restricted">
                <Icon name="lock" size={14} />
                {copy.reviews.restrictedTitle}
              </span>
            ) : (
              <span className="row-title">{r.title || copy.home.kindLabel(r.kind)}</span>
            )}
            <span className="row-meta row-meta-bot">
              <BotBy persona={r.persona} offered={r.persona_offered} size={16} className="row-bot-inline" />
              <span aria-hidden="true"> · </span>
              {restricted ? copy.reviews.restrictedRow : meta}
              {left && (
                <span className="row-when-warn">
                  <span aria-hidden="true"> · </span>
                  <span className="sr-only">, </span>
                  {left}
                </span>
              )}
            </span>
          </span>
        </span>
        <span className="tcell tcell-owner">
          <span className="sr-only">{copy.reviews.columns.owner}: </span>
          <Avatar name={r.owner_name || r.owner} size="sm" />
          <span className="owner-name">{r.owner_name || r.owner}</span>
        </span>
        <span className="tcell tcell-advice">
          <AdvicePill state={reviewState(r)} />
        </span>
        <span className="tcell tcell-time">{relTime(r.created_at, now)}</span>
      </a>
    </li>
  );
}

/** The drafts of the PMs assigned to this reviewer, newest first. */
export function ReviewsInbox() {
  const { reviews, reviewsError, reloadReviews, reviewCount } = useData();

  useEffect(() => {
    void reloadReviews();
  }, [reloadReviews]);

  if (reviewsError instanceof ApiError && reviewsError.code === 'forbidden') return <ReviewCalm code="forbidden" />;

  const now = Date.now();
  const drafts = reviews?.drafts ?? [];
  return (
    <div className="page">
      <PageHeader title={copy.reviews.title} lede={copy.reviews.lede} eyebrow={reviews ? copy.reviews.countLine(reviewCount) : undefined} />
      <div className="card card-table">
        {reviewsError !== null && (
          <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reloadReviews()}>
            {errorText(reviewsError, copy.reviews.loadFailed)}
          </ErrorNote>
        )}
        {!reviews && reviewsError === null && <Skeleton rows={4} label={copy.reviews.loading} />}
        {reviews?.truncated && <p className="admin-hint drafts-cut">{copy.reviews.truncated(reviews.total, drafts.length)}</p>}
        {reviews && drafts.length === 0 && (
          <Empty icon="review" nudge="calm">{reviews.pms.length === 0 ? copy.reviews.emptyNobody : copy.reviews.empty}</Empty>
        )}
        {drafts.length > 0 && (
          <div className="table">
            <div className="thead reviews-grid" aria-hidden="true">
              <span>{copy.reviews.columns.draft}</span>
              <span>{copy.reviews.columns.owner}</span>
              <span>{copy.reviews.columns.advice}</span>
              <span className="tcell-time">{copy.reviews.columns.created}</span>
            </div>
            <ul className="tbody" aria-label={copy.reviews.title}>
              {drafts.map((r) => <ReviewRow key={r.id} r={r} now={now} />)}
            </ul>
          </div>
        )}
      </div>
      {reviews && reviews.pms.length > 0 && <p className="admin-hint reviews-who">{copy.reviews.who(listOf(reviews.pms))}</p>}
    </div>
  );
}
