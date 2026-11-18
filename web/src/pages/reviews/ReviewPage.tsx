import { useCallback, useEffect, useMemo, useState } from 'react';
import { ApiError } from '../../api';
import type { MyRecommendation, ReviewDraft, Verdict } from '../../api';
import { useApp } from '../../app-context';
import { ErrorNote, Markdown, Skeleton, StrippedNote, SystemTag, TargetLink, kindSystem } from '../../components/bits';
import { Icon } from '../../components/icons';
import { PageBar } from '../../components/Shell';
import { SourceList, useSourceJump } from '../../components/Sources';
import { useToast } from '../../components/Toasts';
import { copy } from '../../copy';
import { useData } from '../../data';
import { errorText, isDefiniteRefusal } from '../../errors';
import { renderMarkdown } from '../../markdown';
import { NOTE_MAX, isRestricted, noteLength, noteToSend, versionOf } from '../../reviews';
import { longTime, relTime, timeLeft } from '../../time';
import { ReviewCalm, ReviewsCrumb, isGone } from './parts';
import type { GoneCode } from './parts';

const P = copy.reviews.panel;

/** What the panel says after a send or a check. */
type PanelMsg =
  | { kind: 'changed' }
  | { kind: 'changedNoReload' }
  | { kind: 'unsure' }
  | { kind: 'checkFailed' }
  | { kind: 'checked'; some: boolean }
  | { kind: 'refused'; text: string };

/** Who the draft is from, as plain text. */
const ownerOf = (r: ReviewDraft) => r.owner_name || r.owner;

/** The reviewer's own recommendation, as recorded, and when. */
function Mine({ mine }: { mine: MyRecommendation }) {
  return (
    <div className={`mine ${mine.stale ? 'mine-stale' : ''}`} data-verdict={mine.verdict}>
      <p className="mine-line">
        <span className={`rec-icon rec-icon-${mine.stale ? 'stale' : mine.verdict}`} aria-hidden="true">
          <Icon name={mine.verdict === 'approve' ? 'check' : 'edit'} size={13} weight={2.2} />
        </span>
        <span>{mine.stale ? P.mineStale[mine.verdict] : P.mine[mine.verdict]}</span>
      </p>
      <p className="rec-meta">
        <time dateTime={mine.created_at} title={longTime(mine.created_at)}>{P.when(relTime(mine.created_at))}</time>
        {mine.stale && <span className="tag">{copy.recommendations.earlier}</span>}
      </p>
      {mine.note !== null && mine.note !== '' && (
        <div className="mine-note">
          <span className="sr-only">{P.yourNote}: </span>
          <p className="rec-note">{mine.note}</p>
        </div>
      )}
      {mine.note_removed && <p className="rec-removed">{P.noteRemoved}</p>}
      <p className="mine-next">{mine.stale ? P.staleNext : P.replaces}</p>
    </div>
  );
}

/** A reviewer's read of one draft: the text, its sources, and the recommend panel. Never approve, edit or send. */
export function ReviewPage({ id }: { id: string }) {
  const { api } = useApp();
  const { reloadReviews } = useData();
  const toast = useToast();

  const [review, setReview] = useState<ReviewDraft | null>(null);
  const [gone, setGone] = useState<GoneCode | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // The reviewer's note survives every reload of the draft: only a recorded recommendation clears it.
  const [note, setNote] = useState('');
  const [sending, setSending] = useState<Verdict | null>(null);
  const [msg, setMsg] = useState<PanelMsg | null>(null);
  // A recommendation whose answer was lost: no second send until the reviewer has checked.
  const [uncertain, setUncertain] = useState(false);
  const { selected, flash, pick, toSource } = useSourceJump(null);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const r = await api.getReview(id);
      setReview(r);
      setGone(null);
      return r;
    } catch (e) {
      if (e instanceof ApiError && isGone(e.code)) setGone(e.code);
      else setLoadError(errorText(e, copy.reviews.loadDraftFailed));
      return null;
    }
  }, [api, id]);

  useEffect(() => {
    void load();
  }, [load]);

  const rendered = useMemo(
    () => renderMarkdown(review?.body ?? '', { sources: review?.sources ?? [] }),
    [review?.body, review?.sources],
  );

  if (gone) return <ReviewCalm code={gone} id={id} />;

  if (loadError) {
    return (
      <>
        <PageBar crumb={<ReviewsCrumb current="…" />} />
        <div className="page page-narrow">
          <a href="#/reviews" className="back">
            <Icon name="arrowLeft" size={16} />
            {copy.reviews.back}
          </a>
          <ErrorNote actionLabel={copy.errors.retry} onAction={() => void load()}>
            {loadError}
          </ErrorNote>
        </div>
      </>
    );
  }

  if (!review) {
    return (
      <>
        <PageBar crumb={<ReviewsCrumb current="…" />} />
        <div className="workspace">
          <div className="doc-col">
            <div className="sheet">
              <Skeleton rows={4} label={copy.reviews.loadingDraft} variant="doc" />
            </div>
          </div>
        </div>
      </>
    );
  }

  const owner = ownerOf(review);
  const left = timeLeft(review.expires_at);

  // ---- A restricted draft: who, what kind, when, and why its text is not shown. Nothing to recommend on. ----
  if (isRestricted(review)) {
    return (
      <>
        <PageBar crumb={<ReviewsCrumb current={copy.reviews.breadcrumbRestricted} />} />
        <div className="page page-narrow">
          <a href="#/reviews" className="back">
            <Icon name="arrowLeft" size={16} />
            {copy.reviews.back}
          </a>
          <section className="card review-restricted" aria-labelledby="h-restricted">
            <div className="card-head">
              <h1 id="h-restricted" className="review-restricted-title">
                <Icon name="lock" size={16} />
                {copy.reviews.restrictedTitle}
              </h1>
              <SystemTag system={kindSystem(review.kind)} label={copy.home.kindLabel(review.kind)} />
            </div>
            <div className="card-body">
              <p className="review-restricted-line">{copy.reviews.restrictedLine}</p>
              <dl className="facts">
                <dt>{copy.reviews.details.owner}</dt>
                <dd>{owner}</dd>
                <dt>{copy.reviews.details.kind}</dt>
                <dd>{copy.home.kindLabel(review.kind)}</dd>
                <dt>{copy.reviews.details.created}</dt>
                <dd>{longTime(review.created_at)} <span className="muted">· {relTime(review.created_at)}</span></dd>
                <dt>{copy.reviews.details.expires}</dt>
                <dd>{review.expires_at ? <>{longTime(review.expires_at)}{left && <span className="muted"> · {left}</span>}</> : copy.reviews.details.never}</dd>
              </dl>
            </div>
          </section>
        </div>
      </>
    );
  }

  const version = versionOf(review);
  const length = noteLength(note);
  const tooLong = length > NOTE_MAX;
  const blocked = sending !== null || uncertain || tooLong || version === null;

  const send = async (verdict: Verdict) => {
    if (!version || blocked) return;
    setSending(verdict);
    setMsg(null);
    try {
      const r = await api.recommend(id, { verdict, note: noteToSend(note), ...version });
      setReview({ ...r.draft, my_recommendation: r.recommendation });
      setNote('');
      toast(P.recorded[verdict], 'good');
      void reloadReviews();
    } catch (e) {
      if (e instanceof ApiError && e.code === 'draft_changed') {
        // Its owner saved other text since it was read: show the new version, keep the note.
        const fresh = await load();
        setMsg(fresh ? { kind: 'changed' } : { kind: 'changedNoReload' });
        void reloadReviews();
      } else if (!isDefiniteRefusal(e, 'decision')) {
        // The answer was lost: it may or may not be recorded. Say neither; offer to look.
        setUncertain(true);
        setMsg({ kind: 'unsure' });
      } else if (isGone(e.code) || e.code === 'restricted_draft') {
        // The draft can't take advice any more (decided, expired, restricted): show where it stands.
        await load();
        void reloadReviews();
      } else {
        setMsg({ kind: 'refused', text: e.code === 'upstream_unavailable' ? P.upstream : errorText(e, copy.errors.unknown) });
      }
    } finally {
      setSending(null);
    }
  };

  /** "Check again" after a lost answer: read what the server holds now, and show it. */
  const check = async () => {
    try {
      const fresh = await api.getReview(id);
      setReview(fresh);
      setUncertain(false);
      setMsg({ kind: 'checked', some: !!fresh.my_recommendation });
      void reloadReviews();
    } catch (e) {
      if (e instanceof ApiError && isGone(e.code)) {
        setGone(e.code);
        return;
      }
      setMsg({ kind: 'checkFailed' });
    }
  };

  const message = (() => {
    if (!msg) return null;
    switch (msg.kind) {
      case 'changed':
        return (
          <div className="note note-warn" role="alert">
            <Icon name="refresh" className="note-icon" />
            <div className="note-body"><p>{P.changed(owner)}</p></div>
          </div>
        );
      case 'changedNoReload':
        return <ErrorNote actionLabel={P.reload} onAction={() => void load().then((r) => r && setMsg({ kind: 'changed' }))}>{P.changedNoReload}</ErrorNote>;
      case 'unsure':
      case 'checkFailed':
        return (
          <ErrorNote next={P.unsureNext} actionLabel={P.checkAgain} onAction={() => void check()}>
            {msg.kind === 'unsure' ? P.unsure : P.checkFailed}
          </ErrorNote>
        );
      case 'checked':
        return (
          <p className="note note-info" role="status">
            {msg.some ? P.checkedSome : P.checkedNone}
          </p>
        );
      case 'refused':
        return <ErrorNote>{msg.text}</ErrorNote>;
    }
  })();

  const panel = (
    <section className="side-section review-panel" aria-labelledby="h-advice">
      <h2 id="h-advice">{P.heading}</h2>
      <p className="review-intro">{P.intro(owner)}</p>
      {review.my_recommendation && <Mine mine={review.my_recommendation} />}
      {message}
      {version === null ? (
        <p className="note note-info">{P.cannot}</p>
      ) : (
        <form
          className="review-form"
          aria-labelledby="h-advice"
          onSubmit={(e) => e.preventDefault()}
        >
          <div className="field">
            <div className="field-row">
              <label className="field-label" htmlFor="review-note">{P.noteLabel(owner)}</label>
              <span className="field-count">{P.optional}</span>
            </div>
            <textarea
              id="review-note"
              rows={4}
              value={note}
              placeholder={P.notePlaceholder}
              aria-invalid={tooLong || undefined}
              aria-describedby={tooLong ? 'review-note-count review-note-error' : 'review-note-count'}
              onChange={(e) => setNote(e.target.value)}
            />
            <p id="review-note-count" className={`note-counter ${tooLong ? 'note-counter-over' : ''}`}>
              <span aria-hidden="true">{P.counter(length, NOTE_MAX)}</span>
              <span className="sr-only">{P.counterLabel(length, NOTE_MAX)}</span>
            </p>
            {tooLong && <p id="review-note-error" className="field-hint field-error">{P.tooLong(NOTE_MAX)}</p>}
          </div>
          <div className="review-actions">
            <button type="button" className="btn btn-primary" disabled={blocked} onClick={() => void send('approve')}>
              <Icon name="check" size={15} weight={2.4} />
              {sending === 'approve' ? P.sending : P.approve}
            </button>
            <button type="button" className="btn" disabled={blocked} onClick={() => void send('changes')}>
              <Icon name="edit" size={15} />
              {sending === 'changes' ? P.sending : P.changes}
            </button>
          </div>
        </form>
      )}
    </section>
  );

  const sources = review.sources ?? [];
  const target = review.target;

  return (
    <>
      <PageBar crumb={<ReviewsCrumb current={review.title || copy.home.kindLabel(review.kind)} />}>
        <span className="pagebar-spacer" />
        <span className="review-from">{copy.reviews.from(owner)}</span>
      </PageBar>
      <div className="workspace review-workspace">
        <div className="doc-col">
          <article className="sheet" aria-label={copy.draft.formattedLabel}>
            <div className="doc-meta">
              <SystemTag system={kindSystem(review.kind)} label={copy.home.kindLabel(review.kind)} />
              {target && <span className="doc-target">{target.label}</span>}
              {target && <TargetLink target={target} />}
              <span className="doc-who">{copy.reviews.from(owner)}</span>
              {left && <span className="doc-left"><Icon name="clock" size={14} />{left}</span>}
              {review.edited && <span className="doc-edited">{copy.reviews.editedByOwner}</span>}
            </div>
            <h1 className="doc-title">{review.title || copy.home.kindLabel(review.kind)}</h1>
            <Markdown rendered={rendered} className="prose" onCite={toSource} highlight={selected} />
            <StrippedNote parts={rendered.stripped} what={copy.stripped.draft} />
          </article>
          <p className="muted small read-only">{copy.reviews.readOnly}</p>
        </div>
        <aside className="panel" aria-label={copy.panel.label}>
          {panel}
          <section className="side-section" aria-labelledby="h-src">
            <div className="side-head">
              <h2 id="h-src">{copy.draft.sourcesHeading}</h2>
              <span className="side-count">{copy.draft.sourcesCount(sources.length)}</span>
            </div>
            <SourceList sources={sources} places={rendered.cites} selected={selected} flash={flash} onPick={pick} />
          </section>
          <section className="side-section side-details" aria-labelledby="h-det">
            <h2 id="h-det">{copy.reviews.details.heading}</h2>
            <dl className="details">
              <dt>{copy.reviews.details.owner}</dt>
              <dd>{owner}</dd>
              <dt>{copy.reviews.details.kind}</dt>
              <dd>{copy.home.kindLabel(review.kind)}</dd>
              {target && (
                <>
                  <dt>{copy.reviews.details.target}</dt>
                  <dd>{target.label} <TargetLink target={target} /></dd>
                </>
              )}
              <dt>{copy.reviews.details.created}</dt>
              <dd>{longTime(review.created_at)} <span className="muted">· {relTime(review.created_at)}</span></dd>
              <dt>{copy.reviews.details.expires}</dt>
              <dd>{review.expires_at ? <>{longTime(review.expires_at)}{left && <span className="muted"> · {left}</span>}</> : copy.reviews.details.never}</dd>
              <dt>{copy.reviews.details.id}</dt>
              <dd><code className="mono-id">{review.id}</code></dd>
            </dl>
          </section>
        </aside>
      </div>
    </>
  );
}
