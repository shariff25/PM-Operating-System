import { useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { useAsk, useData } from '../data';
import { effectiveStatus } from '../drafts';
import { relTime } from '../time';
import { DraftPill } from './bits';
import { Icon, kindIcon } from './icons';
import type { IconName } from './icons';
import { sectionHref, sectionsFor } from '../admin/access';
import type { AdminSection } from '../admin/access';

const ADMIN_ICONS: Record<AdminSection, IconName> = {
  overview: 'shield',
  audit: 'activity',
  team: 'followups',
  connectors: 'globe',
  policy: 'shield',
  content: 'layers',
};

interface Option {
  id: string;
  group: 'ask' | 'pages' | 'drafts';
  label: string;
  meta?: string;
  icon: IconName;
  draftStatus?: Parameters<typeof DraftPill>[0]['status'];
  run: () => void;
}

/**
 * Ctrl or Cmd + K: ask the bot from anywhere, or jump to a page or a draft by title.
 * Focus stays in the input (the list is driven by aria-activedescendant), Escape closes, and
 * focus goes back to where it was.
 */
export function Palette({ initial, onClose }: { initial: string; onClose: () => void }) {
  const { api, me, navigate } = useApp();
  const { drafts, briefState } = useData();
  const { start, asking, error } = useAsk();
  const [query, setQuery] = useState(initial);
  const [active, setActive] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    input.current?.focus();
    return () => before?.focus?.();
  }, []);

  const q = query.trim();
  const ql = q.toLowerCase();

  const options = useMemo<Option[]>(() => {
    const go = (path: string) => () => {
      onClose();
      navigate(path);
    };
    const pages: Option[] = [
      { id: 'p-home', group: 'pages', label: copy.nav.home, icon: 'home', run: go('/') },
      { id: 'p-drafts', group: 'pages', label: copy.nav.drafts, icon: 'drafts', run: go('/drafts') },
      ...(me.reviewer === true
        ? [{ id: 'p-reviews', group: 'pages' as const, label: copy.nav.reviews, icon: 'review' as const, run: go('/reviews') }]
        : []),
      ...(briefState === 'ready'
        ? [{ id: 'p-followups', group: 'pages' as const, label: copy.nav.followups, icon: 'followups' as const, run: go('/followups') }]
        : []),
      { id: 'p-activity', group: 'pages', label: copy.nav.activity, icon: 'activity', run: go('/activity') },
      // The Admin parts this person may open; the server checks each again.
      ...sectionsFor(me).map((s): Option => ({
        id: `p-admin-${s}`,
        group: 'pages',
        label: s === 'overview' ? copy.nav.admin : copy.admin.sections[s],
        meta: s === 'overview' ? undefined : copy.admin.area,
        icon: ADMIN_ICONS[s],
        run: go(sectionHref(s).slice(1)),
      })),
    ];
    const now = Date.now();
    const draftOpts: Option[] = (drafts ?? []).map((d) => ({
      id: `d-${d.id}`,
      group: 'drafts',
      label: d.title,
      meta: `${copy.home.kindLabel(d.kind)} · ${relTime(d.created_at, now)}`,
      icon: kindIcon(d.kind),
      draftStatus: effectiveStatus(d, now),
      run: go(`/d/${encodeURIComponent(d.id)}`),
    }));
    const matchPages = pages.filter((p) => !ql || p.label.toLowerCase().includes(ql));
    const matchDrafts = (ql ? draftOpts.filter((d) => d.label.toLowerCase().includes(ql)) : draftOpts.filter((d) => d.draftStatus === 'pending')).slice(0, 6);
    if (!q) return [...matchPages, ...matchDrafts];
    const askOpt: Option = {
      id: 'ask',
      group: 'ask',
      label: copy.palette.ask(q),
      icon: 'sparkle',
      run: () => {
        void start(() => api.createTask(q), copy.errors.askFailed).then((ok) => ok && onClose());
      },
    };
    // A query that is the start of a page or draft name jumps first; anything else asks first.
    const prefix = [...matchPages, ...matchDrafts].some((o) => o.label.toLowerCase().startsWith(ql));
    return prefix ? [...matchPages, ...matchDrafts, askOpt] : [askOpt, ...matchPages, ...matchDrafts];
  }, [q, ql, drafts, briefState, navigate, onClose, start, api, me]);

  useEffect(() => setActive(0), [q]);

  useEffect(() => {
    list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' });
  }, [active]);

  const onKey = (e: KeyboardEvent) => {
    const n = options.length;
    if (e.key === 'Escape') {
      e.preventDefault();
      onClose();
    } else if (e.key === 'ArrowDown' && n) {
      e.preventDefault();
      setActive((a) => (a + 1) % n);
    } else if (e.key === 'ArrowUp' && n) {
      e.preventDefault();
      setActive((a) => (a - 1 + n) % n);
    } else if (e.key === 'Home' && n && e.ctrlKey) {
      e.preventDefault();
      setActive(0);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (asking) return;
      const opt = options[active];
      if (opt) opt.run();
    } else if (e.key === 'Tab') {
      // Focus is trapped: the input is the only stop in the palette.
      e.preventDefault();
    }
  };

  const groupLabel: Record<Option['group'], string | null> = {
    ask: null,
    pages: copy.palette.pages,
    drafts: copy.palette.drafts,
  };
  const ordered = options;
  const activeId = ordered[active] ? `pal-${ordered[active]!.id}` : undefined;

  return createPortal(
    <div
      className="palette-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="palette" role="dialog" aria-modal="true" aria-label={copy.palette.label} onKeyDown={onKey}>
        <div className="palette-input">
          <Icon name="search" size={18} className="palette-search" />
          <input
            ref={input}
            type="text"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={activeId}
            aria-autocomplete="list"
            aria-label={copy.palette.inputLabel}
            placeholder={copy.palette.placeholder}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
          <kbd className="kbd">Esc</kbd>
        </div>
        {error && <p className="palette-error" role="alert">{error}</p>}
        <ul className="palette-list" id="palette-list" role="listbox" aria-label={copy.palette.label} ref={list}>
          {ordered.map((o, i) => {
            const label = groupLabel[o.group];
            const header = label && ordered[i - 1]?.group !== o.group;
            return [
              header ? (
                <li key={`h-${o.group}`} role="presentation" className="palette-group">
                  {label}
                </li>
              ) : null,
              <li
                key={o.id}
                id={`pal-${o.id}`}
                role="option"
                aria-selected={i === active}
                className={`palette-item ${o.group === 'ask' ? 'palette-ask' : ''}`}
                onMouseMove={() => setActive(i)}
                onClick={() => o.run()}
              >
                <span className="palette-icon"><Icon name={o.icon} size={16} /></span>
                <span className="palette-text">
                  <span className="palette-label">{o.label}</span>
                  {o.meta && <span className="palette-meta">{o.meta}</span>}
                </span>
                {o.draftStatus && o.draftStatus !== 'pending' && <DraftPill status={o.draftStatus} />}
                {i === active && <kbd className="kbd palette-enter" aria-hidden="true">↵</kbd>}
              </li>,
            ];
          })}
          {ordered.length === 0 && (
            <li role="presentation" className="palette-empty">{copy.palette.askEmpty}</li>
          )}
        </ul>
        <div className="palette-foot" aria-hidden="true">
          <span><kbd className="kbd">↑</kbd><kbd className="kbd">↓</kbd> {copy.palette.hintMove}</span>
          <span><kbd className="kbd">↵</kbd> {copy.palette.hintPick}</span>
          <span><kbd className="kbd">Esc</kbd> {copy.palette.hintClose}</span>
        </div>
      </div>
    </div>,
    document.body,
  );
}
