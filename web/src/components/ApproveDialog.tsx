import { useEffect, useRef } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { Draft } from '../api';
import { copy } from '../copy';
import type { Rendered } from '../markdown';
import { StrippedNote } from './bits';

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
          <dd>{draft.target.label}</dd>
          <dt>{copy.approve.confirmWho}</dt>
          <dd>{p.name}</dd>
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
        <p>{draft.practice ? copy.approve.confirmPractice : copy.approve.confirmNothing}</p>
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
        <div className="row">
          <button
            type="button"
            className="btn btn-primary"
            onClick={p.onConfirm}
            disabled={p.approving || (p.showExternal && !p.externalOk)}
          >
            {p.approving ? copy.approve.confirming : p.confirmLabel}
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
