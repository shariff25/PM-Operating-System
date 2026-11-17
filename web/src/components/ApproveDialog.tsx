import { useEffect, useRef } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { Draft } from '../api';
import { copy } from '../copy';
import type { Rendered } from '../markdown';
import { Icon } from './icons';
import { StrippedNote, TargetLink } from './bits';

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
  /** The server will not post this where it goes (audience_blocked): only rejecting is left. */
  blocked?: boolean;
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
          {markerLine && (
            <>
              <dt>{copy.approve.markerHeading}</dt>
              <dd>{markerLine}</dd>
            </>
          )}
        </dl>
        {draft.restricted_sources && (
          <div className="note note-warn note-restricted">
            <Icon name="alert" className="note-icon" />
            <div className="note-body">
              <p><strong>{copy.approve.restricted}</strong></p>
              <p>{copy.approve.restrictedNote}</p>
            </div>
          </div>
        )}
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
        <p>{draft.practice ? copy.approve.confirmPractice : copy.approve.confirmNothing(draft.kind)}</p>
        {p.showExternal && (
          <div className="note note-warn">
            <p>{copy.approve.externalWarning}</p>
            <label className="check">
              <input type="checkbox" checked={p.externalOk} onChange={(e) => p.onExternalOk(e.target.checked)} />
              {copy.approve.externalCheckbox}
            </label>
          </div>
        )}
        {p.note}
        <div className="modal-actions">
          <button
            type="button"
            className="btn btn-primary"
            onClick={p.onConfirm}
            disabled={p.approving || p.blocked || (p.showExternal && !p.externalOk)}
          >
            {p.approving ? copy.approve.confirming(draft.kind) : p.confirmLabel}
          </button>
          <button type="button" className="btn" onClick={p.onCancel}>
            {copy.approve.cancel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
