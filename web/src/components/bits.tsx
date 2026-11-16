import { useState } from 'react';
import type { ReactNode } from 'react';
import type { HintName } from '../api';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';
import type { Rendered } from '../markdown';
import { safeUrl } from '../url';

/** The formatted text. `rendered` comes from renderMarkdown, which keeps only plain formatting. */
export function Markdown({ rendered, className }: { rendered: Rendered; className?: string }) {
  return <div className={`md ${className ?? ''}`} dangerouslySetInnerHTML={{ __html: rendered.html }} />;
}

/** A plain warning when parts of the text were left out of the formatted view, with a way to read them. */
export function StrippedNote({ parts, what }: { parts: string[]; what: string }) {
  const [shown, setShown] = useState(false);
  if (parts.length === 0) return null;
  return (
    <div className="note note-warn stripped" role="group" aria-label={copy.stripped.listLabel}>
      <p>
        <strong>{copy.stripped.warning(parts.length, what)}</strong> {copy.stripped.why}
      </p>
      <button type="button" className="btn" aria-expanded={shown} onClick={() => setShown((s) => !s)}>
        {shown ? copy.stripped.hide : copy.stripped.show}
      </button>
      {shown && (
        <ol className="stripped-list">
          {parts.map((p, i) => (
            <li key={i}>
              <code>{p}</code>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/** A link only when the address is http or https. Anything else is shown as text, never as a link. */
export function ExtLink({
  href,
  children,
  className,
  label,
}: {
  href: string | null | undefined;
  children: ReactNode;
  className?: string;
  /** Screen-reader-only extra words after the link text. */
  label?: string;
}) {
  const safe = safeUrl(href);
  if (!safe) {
    return (
      <span className="unsafe-url">
        {copy.draft.sourceUnsafe} <code>{href ?? ''}</code>
      </span>
    );
  }
  return (
    <a href={safe} target="_blank" rel="noopener noreferrer" className={className}>
      {children}
      {label && <span className="sr-only">{label}</span>}
    </a>
  );
}

/** A plain error: what happened, and one next step as a button when there is an action. */
export function ErrorNote({
  children,
  next,
  actionLabel,
  onAction,
}: {
  children: ReactNode;
  next?: string;
  actionLabel?: string;
  onAction?: () => void;
}) {
  return (
    <div className="note note-error" role="alert">
      <p>{children}</p>
      {next && <p className="note-next">{next}</p>}
      {actionLabel && onAction && (
        <button type="button" className="btn" onClick={onAction}>
          {actionLabel}
        </button>
      )}
    </div>
  );
}

/**
 * A one-time hint, shown in place. Hints appear one at a time: sources, then ask, then approve.
 * The next one shows only after the one before it was dismissed. Hidden when nudges are off.
 */
export function Hint({ name }: { name: HintName }) {
  const { api, me, setMe } = useApp();
  const [error, setError] = useState<string | null>(null);
  const active = copy.hintOrder.find((h) => !me.hints_seen.includes(h));
  if (!me.nudges || active !== name) return null;
  const dismiss = async () => {
    try {
      setMe(await api.markHintSeen(name));
    } catch (e) {
      setError(messageOf(e, copy.errors.settingFailed));
    }
  };
  return (
    <aside className="hint" aria-label="Hint" data-hint={name}>
      <p>{copy.hints[name]}</p>
      <button type="button" className="btn btn-quiet" onClick={dismiss}>
        {copy.hintDismiss}
      </button>
      {error && <p className="hint-error" role="alert">{error}</p>}
    </aside>
  );
}
