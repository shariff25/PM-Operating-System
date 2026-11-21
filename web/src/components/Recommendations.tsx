import type { Recommendation } from '../api';
import { copy } from '../copy';
import { longTime, relTime } from '../time';
import { Icon } from './icons';

// What the PM's reviewers advise on a draft (docs/web-api.md, "Reviews"). Advice only: it never
// blocks or changes the owner's own buttons. Names and notes are untrusted text, so they are only
// ever React text nodes: no Markdown, no HTML, no links made from them.

/** One plain line per recommendation, for the approve dialog's facts. */
export function recommendationLines(recs: readonly Recommendation[] | undefined): string[] {
  return (recs ?? []).map((r) => copy.recommendations.line(r.reviewer_name || r.reviewer, r.verdict, r.stale));
}

/** The compact Reviews block in the draft workspace. Renders nothing when there is no advice. */
export function Recommendations({ recs }: { recs: readonly Recommendation[] | undefined }) {
  if (!recs || recs.length === 0) return null;
  const R = copy.recommendations;
  return (
    <section className="side-section recs" aria-labelledby="h-recs">
      <div className="side-head">
        <h2 id="h-recs">{R.heading}</h2>
        <span className="side-count">{recs.length}</span>
      </div>
      <ul className="recs-list">
        {recs.map((r) => (
          <li key={r.reviewer} className={`rec ${r.stale ? 'rec-stale' : ''}`} data-verdict={r.verdict}>
            <span className={`rec-icon rec-icon-${r.stale ? 'stale' : r.verdict}`} aria-hidden="true">
              <Icon name={r.verdict === 'approve' ? 'check' : 'edit'} size={13} weight={2.2} />
            </span>
            <div className="rec-body">
              <p className="rec-line">{R.line(r.reviewer_name || r.reviewer, r.verdict, r.stale)}</p>
              <p className="rec-meta">
                <time dateTime={r.created_at} title={longTime(r.created_at)}>{relTime(r.created_at)}</time>
                {r.stale && <span className="tag">{R.earlier}</span>}
              </p>
              {r.note !== null && r.note !== '' && <p className="rec-note">{r.note}</p>}
              {r.note_removed && <p className="rec-removed">{R.noteRemoved}</p>}
            </div>
          </li>
        ))}
      </ul>
      <p className="rec-foot">{R.adviceOnly}</p>
    </section>
  );
}
