import { useState } from 'react';
import type { ReactNode } from 'react';
import type { HintName } from '../api';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';
import { renderMarkdown } from '../markdown';

export function Markdown({ text, className }: { text: string; className?: string }) {
  return <div className={`md ${className ?? ''}`} dangerouslySetInnerHTML={{ __html: renderMarkdown(text) }} />;
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

/** A one-time hint, shown in place. Hidden when nudges are off or it was already dismissed. */
export function Hint({ name }: { name: HintName }) {
  const { api, me, setMe } = useApp();
  const [error, setError] = useState<string | null>(null);
  if (!me.nudges || me.hints_seen.includes(name)) return null;
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
