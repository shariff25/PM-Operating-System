import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';

const T = copy.admin.team;
const FOCUSABLE =
  'button:not(:disabled), input:not(:disabled):not([type="hidden"]), textarea:not(:disabled), select:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])';

export interface Problem {
  text: string;
  /** `warn` when the list was read again (stale, or an answer that was lost); `error` for a refusal. */
  tone: 'error' | 'warn';
}

/**
 * A Team dialog: a form with a heading, the body, the server's answer when it refused, and the
 * actions. Focus starts on the element marked `data-autofocus` (else the first field, else
 * Cancel), stays inside, and Escape closes it unless a request is out.
 */
export function TeamDialog({
  title,
  intro,
  children,
  onClose,
  onSubmit,
  busy,
  canSubmit = true,
  submitLabel,
  busyLabel,
  hint,
  problem,
  danger,
  testId,
  hideCancel,
}: {
  title: string;
  intro?: ReactNode;
  children?: ReactNode;
  onClose: () => void;
  onSubmit: () => void;
  busy: boolean;
  canSubmit?: boolean;
  submitLabel: string;
  busyLabel: string;
  /** Why the button is off, beside it. */
  hint?: string | null;
  problem: Problem | null;
  /** A change that takes something away: the confirm button is red, and the dialog is an alert dialog. */
  danger?: boolean;
  testId?: string;
  /** Only one way out (a result shown once): the submit button closes it. */
  hideCancel?: boolean;
}) {
  const box = useRef<HTMLDivElement>(null);
  const id = useId();

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const first =
      el.querySelector<HTMLElement>('[data-autofocus]') ??
      el.querySelector<HTMLElement>('input:not(:disabled):not([readonly]), textarea:not(:disabled)') ??
      el.querySelector<HTMLElement>('[data-cancel]') ??
      el.querySelector<HTMLElement>('button[type="submit"]');
    first?.focus();
  }, []);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (!busy) onClose();
    } else if (e.key === 'Tab' && box.current) {
      const els = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const first = els[0];
      const last = els[els.length - 1];
      if (!first || !last) return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  return createPortal(
    <div className="modal-backdrop">
      <div
        ref={box}
        className="modal confirm team-dialog"
        role={danger ? 'alertdialog' : 'dialog'}
        aria-modal="true"
        aria-labelledby={`${id}-h`}
        aria-describedby={intro ? `${id}-d` : undefined}
        onKeyDown={onKey}
        data-testid={testId}
      >
        <form
          className="modal-form"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            if (canSubmit && !busy) onSubmit();
          }}
        >
          <div className="modal-scroll">
            <h2 id={`${id}-h`}>{title}</h2>
            {intro && <div id={`${id}-d`} className="muted team-dialog-intro">{intro}</div>}
            {children}
            {problem && (
              <div className={`note ${problem.tone === 'warn' ? 'note-warn' : 'note-error'}`} role="alert">
                <Icon name={problem.tone === 'warn' ? 'refresh' : 'alert'} className="note-icon" />
                <div className="note-body"><p>{problem.text}</p></div>
              </div>
            )}
          </div>
          <div className="modal-actions">
            <button type="submit" className={`btn ${danger ? 'btn-danger' : 'btn-primary'}`} disabled={!canSubmit || busy}>
              {busy ? busyLabel : submitLabel}
            </button>
            {!hideCancel && (
              <button type="button" className="btn" data-cancel disabled={busy} onClick={onClose}>
                {T.cancel}
              </button>
            )}
            {hint && !busy && <span className="modal-hint">{hint}</span>}
          </div>
        </form>
      </div>
    </div>,
    document.body,
  );
}

/** A labelled text field with a hint below it. */
export function TextField({
  label,
  value,
  onChange,
  hint,
  optional,
  readOnly,
  type = 'text',
  autoComplete = 'off',
  mono,
  invalid,
  autoFocus,
}: {
  label: string;
  value: string;
  onChange?: (v: string) => void;
  hint?: ReactNode;
  optional?: boolean;
  readOnly?: boolean;
  type?: 'text' | 'email';
  autoComplete?: string;
  mono?: boolean;
  invalid?: boolean;
  autoFocus?: boolean;
}) {
  const id = useId();
  return (
    <div className="field">
      <div className="field-row">
        <label className="field-label" htmlFor={id}>{label}</label>
        {optional && <span className="field-count">{T.invite.optional}</span>}
        {readOnly && (
          <span className="field-count field-locked">
            <Icon name="lock" size={12} />
          </span>
        )}
      </div>
      <input
        id={id}
        type={type}
        value={value}
        readOnly={readOnly}
        aria-readonly={readOnly || undefined}
        aria-invalid={invalid || undefined}
        aria-describedby={hint ? `${id}-h` : undefined}
        className={`${mono ? 'input-mono' : ''} ${readOnly ? 'input-readonly' : ''}`}
        autoComplete={autoComplete}
        spellCheck={false}
        data-autofocus={autoFocus || undefined}
        onChange={(e) => onChange?.(e.target.value)}
      />
      {hint && <p id={`${id}-h`} className={`field-hint ${invalid ? 'field-error' : ''}`}>{hint}</p>}
    </div>
  );
}

/** The role picker: one checkbox per role, with what it means beside it. */
export function RoleChecks({
  legend,
  roles,
  value,
  onChange,
  disabled,
  notes,
}: {
  legend: string;
  roles: readonly string[];
  value: readonly string[];
  onChange: (next: string[]) => void;
  disabled?: Partial<Record<string, boolean>>;
  notes?: Partial<Record<string, string>>;
}) {
  const id = useId();
  return (
    <fieldset className="role-checks">
      <legend className="field-label">{legend}</legend>
      {roles.map((r) => {
        const on = value.includes(r);
        const off = disabled?.[r] ?? false;
        const note = notes?.[r];
        return (
          <label key={r} className={`role-check ${off ? 'role-check-off' : ''}`} htmlFor={`${id}-${r}`}>
            <input
              id={`${id}-${r}`}
              type="checkbox"
              checked={on}
              disabled={off}
              aria-describedby={`${id}-${r}-h`}
              onChange={() => onChange(on ? value.filter((x) => x !== r) : [...value, r])}
            />
            <span className="role-check-text">
              <span className="role-check-name">{T.roleLabels[r] ?? r}</span>
              <span id={`${id}-${r}-h`} className="role-check-hint">{note ?? T.roleHints[r]}</span>
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

export interface MenuItem {
  label: string;
  onSelect: () => void;
  icon: Parameters<typeof Icon>[0]['name'];
  danger?: boolean;
}

/**
 * The actions for one person: a button that opens a menu. Arrows, Home and End move; Escape closes and returns
 * focus to the button; Tab closes.
 */
export function RowMenu({ label, items, note, handle }: { label: string; items: MenuItem[]; note?: string; handle: string }) {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const id = useId();

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const b = button.current?.getBoundingClientRect();
      const p = panel.current;
      if (!b || !p) return;
      const h = p.offsetHeight;
      const below = b.bottom + 6 + h <= window.innerHeight - 8;
      p.style.top = `${Math.round(below ? b.bottom + 6 : Math.max(8, b.top - 6 - h))}px`;
      p.style.left = `${Math.round(Math.max(8, Math.min(b.right - p.offsetWidth, window.innerWidth - p.offsetWidth - 8)))}px`;
      p.style.visibility = 'visible';
    };
    place();
    refs.current[0]?.focus({ preventScroll: true });
    // It stays with its button while the page or the table scrolls.
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!panel.current?.contains(t) && !button.current?.contains(t)) setOpen(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [open]);

  const shut = (refocus: boolean) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  const onMenuKey = (e: KeyboardEvent) => {
    const n = items.length;
    const cur = refs.current.findIndex((el) => el === document.activeElement);
    const move = (i: number) => {
      e.preventDefault();
      refs.current[(i + n) % n]?.focus();
    };
    if (e.key === 'ArrowDown') move(cur + 1);
    else if (e.key === 'ArrowUp') move(cur - 1);
    else if (e.key === 'Home') move(0);
    else if (e.key === 'End') move(n - 1);
    else if (e.key === 'Escape') {
      e.preventDefault();
      shut(true);
    } else if (e.key === 'Tab') shut(false);
  };

  return (
    <>
      <button
        ref={button}
        type="button"
        className="row-toggle row-menu-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-label={label}
        data-actions={handle}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <Icon name="more" size={18} weight={2.5} />
      </button>
      {/* Fixed, so the table's own scrolling never cuts it off; in place, so it stays inside the page's landmarks. */}
      {open && (
          <div ref={panel} className="menu-panel row-menu" style={{ visibility: 'hidden' }}>
            <div id={id} role="menu" aria-label={label} aria-describedby={note ? `${id}-n` : undefined} className="row-menu-items" onKeyDown={onMenuKey}>
            {items.map((it, i) => (
              <Fragment key={it.label}>
                {/* The changes that take something away sit apart from the rest. */}
                {it.danger && i > 0 && !items[i - 1]!.danger && <div className="menu-sep" role="separator" />}
                <button
                  ref={(el) => {
                    refs.current[i] = el;
                  }}
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={`menu-item ${it.danger ? 'menu-item-danger' : ''}`}
                  onClick={() => {
                    setOpen(false);
                    it.onSelect();
                  }}
                >
                  <Icon name={it.icon} size={16} />
                  <span className="menu-item-label">{it.label}</span>
                </button>
              </Fragment>
            ))}
            </div>
            {note && <p id={`${id}-n`} className="row-menu-note">{note}</p>}
          </div>
      )}
    </>
  );
}
