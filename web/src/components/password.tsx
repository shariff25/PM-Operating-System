import { forwardRef, useEffect, useId, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { ApiError } from '../api';
import { copy } from '../copy';
import { errorText } from '../errors';
import { Icon } from './icons';

// The pieces the password screens share: the sign-in card (main bundle), the setup page and the
// security page (their own chunks). Nothing here logs, stores or puts in a URL what is typed.

const P = copy.password;

/** Six digits from whatever was typed or pasted: "123 456", "123-456" and "123456" are all 123456. */
export function cleanCode(raw: string): string {
  return raw.replace(/\D/g, '').slice(0, 6);
}

/** How many characters the server counts (Unicode scalar values, not UTF-16 units). */
export function charCount(s: string): number {
  return [...s].length;
}

/** A refusal shown on a password screen: the sentence, and a number that changes on every refusal so focus moves to it again. */
export interface Refusal {
  text: string;
  seq: number;
}

/** A plain sentence for a refusal on the password screens, never the server's own text, never a code. */
export function refusalText(e: unknown): string {
  if (e instanceof ApiError) {
    const own = P.refusals[e.code as keyof typeof P.refusals];
    if (typeof own === 'string') return own;
  }
  return errorText(e, P.refusals.failed);
}

/** `Retry-After` of a `429 too_many_attempts`, in seconds, when the server sent one. */
export function retryAfterOf(e: unknown): number | null {
  return e instanceof ApiError && e.code === 'too_many_attempts' && e.retryAfter !== null && e.retryAfter > 0 ? e.retryAfter : null;
}

/** "about 2 minutes" or "45 seconds": how long a wait is, said once. */
export function aboutWait(seconds: number): string {
  if (seconds < 60) return P.wait.seconds(Math.ceil(seconds));
  return P.wait.minutes(Math.max(1, Math.round(seconds / 60)));
}

/** "1:05": the clock for a wait. */
export function waitClock(seconds: number): string {
  const s = Math.max(0, Math.ceil(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** A refusal: announced, and focused so the person hears it and starts from it. */
export function RefusalNote({ refusal, children }: { refusal: Refusal; children?: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    box.current?.focus();
  }, [refusal.seq]);
  return (
    <div ref={box} className="note note-error pw-note" role="alert" tabIndex={-1}>
      <Icon name="alert" className="note-icon" />
      <div className="note-body">
        <p>{refusal.text}</p>
        {children}
      </div>
    </div>
  );
}

/**
 * A wait after `429 too_many_attempts`. The sentence is announced once; the clock ticks for the
 * eye only, so screen readers are not told every second; "You can try again now" is said at the end.
 */
export function WaitNote({ until, onOver }: { until: number; onOver: () => void }) {
  const [now, setNow] = useState(() => Date.now());
  const box = useRef<HTMLDivElement>(null);
  const [first] = useState(() => Math.max(0, (until - Date.now()) / 1000));
  const left = Math.max(0, (until - now) / 1000);
  const over = left <= 0;
  const done = useRef(onOver);
  done.current = onOver;

  useEffect(() => {
    box.current?.focus();
  }, [until]);
  useEffect(() => {
    if (over) {
      done.current();
      return;
    }
    const t = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(t);
  }, [over]);

  return (
    <div ref={box} className="note note-warn pw-note pw-wait" tabIndex={-1} data-testid="wait-note">
      <Icon name="clock" className="note-icon" />
      <div className="note-body">
        <p role="alert">{P.wait.start(aboutWait(first))}</p>
        <p className="pw-wait-clock" aria-hidden="true">{P.wait.clock(waitClock(left))}</p>
      </div>
    </div>
  );
}

/** Said once a wait is over, politely. */
export function WaitOver() {
  return (
    <p className="pw-wait-over" role="status">
      <Icon name="check" size={14} weight={2.25} />
      {P.wait.over}
    </p>
  );
}

interface FieldProps {
  label: string;
  value: string;
  onChange: (v: string) => void;
  hint?: ReactNode;
  invalid?: boolean;
  autoComplete: string;
  disabled?: boolean;
}

/** A password field with a Show or Hide button. Nothing typed here leaves the field except in the request. */
export const PasswordField = forwardRef<HTMLInputElement, FieldProps & { name?: string }>(function PasswordField(
  { label, value, onChange, hint, invalid, autoComplete, disabled, name },
  ref,
) {
  const id = useId();
  const [shown, setShown] = useState(false);
  return (
    <div className="pw-field">
      <label className="field-label" htmlFor={id}>{label}</label>
      <div className="pw-input">
        <input
          ref={ref}
          id={id}
          name={name}
          type={shown ? 'text' : 'password'}
          value={value}
          autoComplete={autoComplete}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          disabled={disabled}
          aria-invalid={invalid || undefined}
          aria-describedby={hint ? `${id}-h` : undefined}
          onChange={(e) => onChange(e.target.value)}
        />
        <button
          type="button"
          className="pw-toggle"
          aria-controls={id}
          aria-label={shown ? P.hideLabel : P.showLabel}
          onClick={() => setShown((s) => !s)}
        >
          {shown ? P.hide : P.show}
        </button>
      </div>
      {hint && <p id={`${id}-h`} className={`field-hint ${invalid ? 'field-error' : ''}`}>{hint}</p>}
    </div>
  );
});

/** The 6-digit authenticator code: a number pad on phones, paste with spaces works, and nothing submits on its own. */
export const CodeField = forwardRef<HTMLInputElement, Omit<FieldProps, 'autoComplete'>>(function CodeField(
  { label, value, onChange, hint, invalid, disabled },
  ref,
) {
  const id = useId();
  return (
    <div className="pw-field">
      <label className="field-label" htmlFor={id}>{label}</label>
      <input
        ref={ref}
        id={id}
        type="text"
        inputMode="numeric"
        pattern="[0-9 ]*"
        autoComplete="one-time-code"
        className="pw-code"
        value={value}
        disabled={disabled}
        aria-invalid={invalid || undefined}
        aria-describedby={hint ? `${id}-h` : undefined}
        onChange={(e) => onChange(cleanCode(e.target.value))}
      />
      {hint && <p id={`${id}-h`} className="field-hint">{hint}</p>}
    </div>
  );
});

/** A plain text field for the password screens: a sign-in name, a setup code or a recovery code. */
export const PlainField = forwardRef<HTMLInputElement, FieldProps & { mono?: boolean; type?: 'text' | 'email' }>(function PlainField(
  { label, value, onChange, hint, invalid, autoComplete, disabled, mono, type = 'text' },
  ref,
) {
  const id = useId();
  return (
    <div className="pw-field">
      <label className="field-label" htmlFor={id}>{label}</label>
      <input
        ref={ref}
        id={id}
        type={type}
        value={value}
        autoComplete={autoComplete}
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        disabled={disabled}
        className={mono ? 'input-mono' : undefined}
        aria-invalid={invalid || undefined}
        aria-describedby={hint ? `${id}-h` : undefined}
        onChange={(e) => onChange(e.target.value)}
      />
      {hint && <p id={`${id}-h`} className="field-hint">{hint}</p>}
    </div>
  );
});
