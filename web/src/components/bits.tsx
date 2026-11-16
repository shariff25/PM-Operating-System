import { useLayoutEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { DraftStatus, HintName, TaskStatus } from '../api';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';
import type { Rendered } from '../markdown';
import { PHONE_QUERY, useMediaQuery } from '../theme';
import { safeUrl } from '../url';
import { Icon } from './icons';

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
      <Icon name="alert" className="note-icon" />
      <div className="note-body">
        <p>
          <strong>{copy.stripped.warning(parts.length, what)}</strong> {copy.stripped.why}
        </p>
        <button type="button" className="btn btn-sm" aria-expanded={shown} onClick={() => setShown((s) => !s)}>
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
      <Icon name="alert" className="note-icon" />
      <div className="note-body">
        <p>{children}</p>
        {next && <p className="note-next">{next}</p>}
        {actionLabel && onAction && (
          <button type="button" className="btn btn-sm" onClick={onAction}>
            {actionLabel}
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * A one-time hint, drawn as a small coach mark next to the thing it describes. Hints appear one
 * at a time: sources, then ask, then approve. The next one shows only after the one before it
 * was dismissed. Hidden when nudges are off.
 *
 * It sits in the page flow (never over content), with a small pointer toward the element named
 * by `anchor` (an element id). On a phone it is a compact banner without a pointer.
 */
export function Hint({ name, anchor }: { name: HintName; anchor?: string }) {
  const { api, me, setMe } = useApp();
  const phone = useMediaQuery(PHONE_QUERY);
  const box = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const active = copy.hintOrder.find((h) => !me.hints_seen.includes(h));
  const show = me.nudges && active === name;

  // The pointer is aimed at the middle of the anchor, set straight on the element.
  useLayoutEffect(() => {
    if (!show || phone || !anchor) return;
    const aim = () => {
      const target = document.getElementById(anchor);
      const el = box.current;
      if (!target || !el) return;
      const t = target.getBoundingClientRect();
      const h = el.getBoundingClientRect();
      const x = Math.min(Math.max(t.left + t.width / 2 - h.left, 20), Math.max(h.width - 20, 20));
      el.style.setProperty('--caret-x', `${Math.round(x)}px`);
    };
    aim();
    window.addEventListener('resize', aim);
    return () => window.removeEventListener('resize', aim);
  }, [show, phone, anchor]);

  if (!show) return null;
  const dismiss = async () => {
    try {
      setMe(await api.markHintSeen(name));
    } catch (e) {
      setError(messageOf(e, copy.errors.settingFailed));
    }
  };
  const step = copy.hintOrder.indexOf(name) + 1;
  return (
    <div
      ref={box}
      role="note"
      className={`coach ${phone ? 'coach-banner' : 'coach-inline'}`}
      aria-label={copy.hintLabel}
      data-hint={name}
    >
      <div className="coach-text">
        {!phone && <span className="coach-step">{copy.hintStep(step, copy.hintOrder.length)}</span>}
        <p>{phone ? copy.hintsShort[name] : copy.hints[name]}</p>
        {error && <p className="coach-error" role="alert">{error}</p>}
      </div>
      <button type="button" className="coach-btn" onClick={dismiss}>
        {copy.hintDismiss}
      </button>
    </div>
  );
}

const DRAFT_TONE: Record<DraftStatus, string> = {
  pending: 'warn',
  approved: 'good',
  rejected: 'neutral',
  expired: 'neutral',
  failed: 'bad',
};

/** A quiet status pill with a coloured dot. */
export function DraftPill({ status }: { status: DraftStatus }) {
  return (
    <span className={`pill pill-${DRAFT_TONE[status]} pill-${status}`}>
      <span className="pill-dot" aria-hidden="true" />
      {copy.home.statusLabel(status)}
    </span>
  );
}

const TASK_TONE: Record<TaskStatus, string> = {
  queued: 'neutral',
  working: 'accent',
  needs_approval: 'warn',
  done: 'good',
  failed: 'bad',
  cancelled: 'neutral',
};

export function TaskPill({ status }: { status: TaskStatus }) {
  return (
    <span className={`pill pill-${TASK_TONE[status]} pill-task-${status}`}>
      <span className={`pill-dot ${status === 'working' || status === 'queued' ? 'pill-dot-live' : ''}`} aria-hidden="true" />
      {copy.activity.status[status]}
    </span>
  );
}

/** Slack gets violet; every other system (Jira, Confluence, docs) gets the system blue. */
export function systemTone(system: string): 'slack' | 'sys' {
  return system.toLowerCase().includes('slack') ? 'slack' : 'sys';
}

/** A short mono label for the system a draft goes to or a source came from. */
export function SystemTag({ system, label }: { system: string; label: string }) {
  return <span className={`sys-tag sys-tag-${systemTone(system)}`}>{label}</span>;
}

/** The system a draft kind belongs to, for its tag. */
export function kindSystem(kind: string): string {
  if (kind === 'confluence_page') return 'Confluence';
  if (kind === 'jira_issue') return 'Jira';
  return 'Slack';
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  const first = parts[0]?.[0] ?? '?';
  const last = parts.length > 1 ? parts[parts.length - 1]![0] : '';
  return (first + (last ?? '')).toUpperCase();
}

export function Avatar({ name, size = 'md' }: { name: string; size?: 'sm' | 'md' }) {
  return (
    <span className={`avatar avatar-${size}`} aria-hidden="true">
      {initials(name)}
    </span>
  );
}

/** Grey bars where content is still loading, with a sentence for screen readers. */
export function Skeleton({ rows = 3, label, variant = 'rows' }: { rows?: number; label: string; variant?: 'rows' | 'doc' | 'cards' }) {
  return (
    <div className={`skeleton skeleton-${variant}`} role="status">
      <span className="sr-only">{label}</span>
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="skeleton-item" aria-hidden="true">
          <span className="skeleton-bar skeleton-bar-a" />
          <span className="skeleton-bar skeleton-bar-b" />
        </div>
      ))}
    </div>
  );
}

/** A friendly empty state: an icon, one line of copy, and an optional action. */
export function Empty({
  icon,
  children,
  action,
  small,
}: {
  icon: Parameters<typeof Icon>[0]['name'];
  children: ReactNode;
  action?: ReactNode;
  small?: boolean;
}) {
  return (
    <div className={`empty ${small ? 'empty-sm' : ''}`}>
      <span className="empty-icon"><Icon name={icon} size={small ? 18 : 20} /></span>
      <p>{children}</p>
      {action}
    </div>
  );
}

/** The primary page action: opens the palette ready to ask. */
export function NewRequestButton() {
  const { openPalette } = useApp();
  return (
    <button type="button" className="btn btn-primary" onClick={() => openPalette()}>
      <Icon name="sparkle" size={15} />
      {copy.nav.newRequest}
    </button>
  );
}

/** The page heading block every top-level page starts with. */
export function PageHeader({ title, lede, eyebrow, children }: { title: string; lede?: string; eyebrow?: string; children?: ReactNode }) {
  return (
    <header className="page-head">
      <div className="page-head-text">
        {eyebrow && <p className="eyebrow">{eyebrow}</p>}
        <h1>{title}</h1>
        {lede && <p className="lede">{lede}</p>}
      </div>
      {children}
    </header>
  );
}
