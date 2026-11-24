import { useEffect, useRef } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { Draft } from '../api';
import { copy } from '../copy';
import type { Rendered } from '../markdown';
import { Icon } from './icons';
import { StrippedNote, TargetLink } from './bits';
import { recommendationLines } from './Recommendations';

interface Props {
  draft: Draft;
  rendered: Rendered;
  name: string;
  showExternal: boolean;
  externalOk: boolean;
  onExternalOk: (v: boolean) => void;
  approving: boolean;
  confirmLabel: string;
  note: ReactNode;
  /** The server will not post this where it goes (audience_blocked): no confirm, only the two real options. */
  blocked?: boolean;
  /** The server has just answered needs_external_confirm: the external notice says what to do next, as an alert. */
  externalAsked?: boolean;
  /** Blocked only: leave the dialog and start rejecting the draft. */
  onReject?: () => void;
  /** Blocked only: leave the dialog and ask for a version meant for somewhere else. */
  onAskElsewhere?: () => void;
  onConfirm: () => void;
  onCancel: () => void;
}

const FOCUSABLE = 'button:not(:disabled), input:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])';

/**
 * The last look before publishing. It is rendered into document.body, outside the draft's
 * container, so nothing in the draft can sit over it or restyle it. It says what will be
 * published, where, under whose name, and every link the text carries.
 */
export function ApproveDialog(p: Props) {
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    box.current?.focus();
    return () => before?.focus?.();
  }, []);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      p.onCancel();
    } else if (e.key === 'Tab' && box.current) {
      const els = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const first = els[0];
      const last = els[els.length - 1];
      if (!first || !last) return;
      if (e.shiftKey && (document.activeElement === first || document.activeElement === box.current)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  const blocked = p.blocked === true;
  const seen = new Set<string>();
  const links = p.rendered.links.filter((l) => (seen.has(l.href) ? false : (seen.add(l.href), true)));
  const { draft } = p;
  const slack = draft.kind === 'slack_reply' || draft.kind === 'slack_message';
  const aud = draft.audience;
  // Who will see a Slack post. The numbers are null until the server has looked at the channel.
  const audience = [
    aud?.members != null ? copy.approve.audienceVisible(aud.members) : null,
    aud?.external === true ? copy.approve.audienceExternal : aud?.external === false ? copy.approve.audienceInternal : null,
  ].filter((l): l is string => l !== null);
  if (audience.length === 0) audience.push(draft.external ? copy.approve.audienceExternal : copy.approve.audienceUnchecked);
  // The line the target will carry. Slack writes it in italics; here it is shown as plain words.
  const marker = draft.marker?.replace(/^[_*]+|[_*]+$/g, '').trim();
  const markerLine = marker ? (slack ? copy.approve.markerSlack(marker) : copy.approve.markerPage(marker)) : null;
  // Reviewers' advice, as facts beside the others. It informs; it never blocks the confirm.
  const advice = recommendationLines(draft.recommendations);

  return createPortal(
    <div className="modal-backdrop">
      <div
        ref={box}
        className="modal confirm"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-heading"
        tabIndex={-1}
        onKeyDown={onKey}
      >
        <div className="modal-scroll">
        <h2 id="confirm-heading">{copy.approve.confirmHeading}</h2>
        <p>{copy.approve.confirmText(draft.kind, draft.target.label, p.name)}</p>
        <dl className="facts">
          <dt>{copy.approve.confirmTitle}</dt>
          <dd>{draft.title}</dd>
          <dt>{copy.approve.confirmWhere}</dt>
          <dd>
            {draft.target.label} <TargetLink target={draft.target} />
          </dd>
          <dt>{copy.approve.confirmWho}</dt>
          <dd>{p.name}</dd>
          {slack && (
            <>
              <dt>{copy.approve.audienceHeading}</dt>
              <dd>
                <ul className="fact-lines">
                  {audience.map((line) => (
                    <li key={line}>{line}</li>
                  ))}
                </ul>
              </dd>
            </>
          )}
          {draft.restricted_sources && (
            <>
              <dt>{copy.approve.restrictedFact}</dt>
              <dd className="fact-warn">
                <Icon name="alert" size={15} className="fact-warn-icon" />
                <span>
                  <strong>{copy.approve.restricted}</strong>
                  <span className="muted block">{copy.approve.restrictedNote}</span>
                </span>
              </dd>
            </>
          )}
          {markerLine && (
            <>
              <dt>{copy.approve.markerHeading}</dt>
              <dd>{markerLine}</dd>
            </>
          )}
          {advice.length > 0 && (
            <>
              <dt>{copy.recommendations.factHeading}</dt>
              <dd>
                <ul className="fact-lines">
                  {advice.map((line, i) => (
                    <li key={i}>{line}</li>
                  ))}
                </ul>
              </dd>
            </>
          )}
        </dl>
        <h3>{copy.approve.confirmLinks}</h3>
        {links.length === 0 ? (
          <p className="muted">{copy.approve.confirmNoLinks}</p>
        ) : (
          <ul className="link-list" aria-label={copy.approve.confirmLinks}>
            {links.map((l) => (
              <li key={l.href}>
                <strong>{l.host}</strong> <span className="break">{l.href}</span>
                <span className="muted block">{copy.approve.confirmLinkLine(l.text)}</span>
              </li>
            ))}
          </ul>
        )}
        <StrippedNote parts={p.rendered.stripped} what={copy.stripped.draft} />
        {!blocked && <p>{draft.practice ? copy.approve.confirmPractice : copy.approve.confirmNothing(draft.kind)}</p>}
        {p.showExternal && !blocked && (
          <div className="note note-warn" role={p.externalAsked ? 'alert' : undefined}>
            <p>{copy.approve.externalWarning}</p>
            {p.externalAsked && <p className="note-next">{copy.approve.errors.needs_external_confirm.next}</p>}
            <label className="check">
              <input type="checkbox" checked={p.externalOk} onChange={(e) => p.onExternalOk(e.target.checked)} />
              {copy.approve.externalCheckbox}
            </label>
          </div>
        )}
        {p.note}
        </div>
        <div className="modal-actions">
          {blocked ? (
            <>
              <button type="button" className="btn btn-primary" onClick={p.onAskElsewhere}>
                {copy.approve.blockedAsk}
              </button>
              <button type="button" className="btn" onClick={p.onReject}>
                {copy.approve.blockedReject}
              </button>
            </>
          ) : (
            <button
              type="button"
              className="btn btn-primary"
              onClick={p.onConfirm}
              disabled={p.approving || (p.showExternal && !p.externalOk)}
            >
              {p.approving ? copy.approve.confirming(draft.kind) : p.confirmLabel}
            </button>
          )}
          <button type="button" className="btn" onClick={p.onCancel}>
            {copy.approve.cancel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
