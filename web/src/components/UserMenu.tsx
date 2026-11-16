import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';
import type { Theme } from '../theme';
import { Avatar } from './bits';
import { Icon } from './icons';
import type { IconName } from './icons';

const THEMES: { value: Theme; icon: IconName; label: string }[] = [
  { value: 'system', icon: 'monitor', label: copy.theme.system },
  { value: 'light', icon: 'sun', label: copy.theme.light },
  { value: 'dark', icon: 'moon', label: copy.theme.dark },
];
const COUNT = THEMES.length + 3;

/**
 * The user's menu: theme, nudges, hints and sign out. A menu button: Enter, Space or the arrow
 * keys open it; arrows, Home and End move; Escape closes and returns focus to the button.
 */
export function UserMenu({
  onSignOut,
  variant,
  collapsed = false,
}: {
  onSignOut: () => void;
  /** rail: avatar, name and handle in the dark rail, opens upwards. compact: avatar only, opens downwards (phone). */
  variant: 'rail' | 'compact';
  collapsed?: boolean;
}) {
  const { api, me, setMe, theme, setTheme } = useApp();
  const [open, setOpen] = useState(false);
  const [focusIdx, setFocusIdx] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    if (open) items.current[focusIdx]?.focus();
  }, [open, focusIdx]);

  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [open]);

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  const run = async (fn: () => Promise<void>, closeAfter: boolean) => {
    setError(null);
    try {
      await fn();
      if (closeAfter) close(true);
    } catch (e) {
      setError(messageOf(e, copy.errors.settingFailed));
    }
  };

  const openAt = (i: number) => {
    setFocusIdx(i);
    setOpen(true);
  };

  const onButtonKey = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      openAt(0);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      openAt(COUNT - 1);
    }
  };

  const onMenuKey = (e: KeyboardEvent) => {
    const move = (i: number) => {
      e.preventDefault();
      const next = (i + COUNT) % COUNT;
      setFocusIdx(next);
      items.current[next]?.focus();
    };
    const cur = items.current.findIndex((el) => el === document.activeElement);
    if (e.key === 'ArrowDown') move(cur + 1);
    else if (e.key === 'ArrowUp') move(cur - 1);
    else if (e.key === 'ArrowRight' && cur < THEMES.length) move(Math.min(cur + 1, THEMES.length - 1));
    else if (e.key === 'ArrowLeft' && cur < THEMES.length) move(Math.max(cur - 1, 0));
    else if (e.key === 'Home') move(0);
    else if (e.key === 'End') move(COUNT - 1);
    else if (e.key === 'Escape') {
      e.preventDefault();
      close(true);
    } else if (e.key === 'Tab') close(false);
  };

  const ref = (i: number) => (el: HTMLButtonElement | null) => {
    items.current[i] = el;
  };

  return (
    <div className={`menu menu-${variant} ${collapsed ? 'menu-collapsed' : ''}`} ref={box}>
      <button
        ref={button}
        type="button"
        className={`user-button user-button-${variant}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? 'user-menu' : undefined}
        aria-label={copy.menu.button(me.name)}
        onClick={() => (open ? close(false) : openAt(0))}
        onKeyDown={onButtonKey}
      >
        <Avatar name={me.name} size={variant === 'compact' ? 'sm' : 'md'} />
        {variant === 'rail' && !collapsed && (
          <>
            <span className="user-text">
              <span className="user-name">{me.name}</span>
              <span className="user-handle">@{me.handle}</span>
            </span>
            <Icon name="chevronUpDown" size={16} className="user-caret" />
          </>
        )}
      </button>
      {open && (
        <div id="user-menu" className="menu-panel" role="menu" aria-label={copy.menu.button(me.name)} onKeyDown={onMenuKey}>
          <div className="menu-head" aria-hidden="true">
            <Avatar name={me.name} />
            <span>
              <span className="menu-head-name">{me.name}</span>
              <span className="menu-head-handle">@{me.handle}</span>
            </span>
          </div>
          <div className="menu-sep" role="separator" />
          <div className="menu-label" aria-hidden="true">{copy.theme.label}</div>
          <div className="menu-segment" role="group" aria-label={copy.theme.label}>
            {THEMES.map((t, i) => (
              <button
                key={t.value}
                ref={ref(i)}
                type="button"
                role="menuitemradio"
                aria-checked={theme === t.value}
                tabIndex={-1}
                className="segment-item"
                onClick={() => setTheme(t.value)}
              >
                <Icon name={t.icon} size={16} />
                <span>{t.label}</span>
              </button>
            ))}
          </div>
          <div className="menu-sep" role="separator" />
          <button
            ref={ref(THEMES.length)}
            type="button"
            role="menuitemcheckbox"
            aria-checked={me.nudges}
            tabIndex={-1}
            className="menu-item"
            onClick={() => void run(async () => setMe(await api.setNudges(!me.nudges)), false)}
          >
            <Icon name="bell" size={16} />
            <span className="menu-item-label">{copy.menu.nudges}</span>
            <span className={`switch ${me.nudges ? 'switch-on' : ''}`} aria-hidden="true">
              <span className="switch-knob" />
            </span>
            <span className="sr-only">{me.nudges ? copy.menu.nudgesOn : copy.menu.nudgesOff}</span>
          </button>
          <button
            ref={ref(THEMES.length + 1)}
            type="button"
            role="menuitem"
            tabIndex={-1}
            className="menu-item"
            onClick={() => void run(async () => setMe(await api.resetHints()), true)}
          >
            <Icon name="lightbulb" size={16} />
            <span className="menu-item-label">{copy.menu.showHints}</span>
          </button>
          <div className="menu-sep" role="separator" />
          <button
            ref={ref(THEMES.length + 2)}
            type="button"
            role="menuitem"
            tabIndex={-1}
            className="menu-item"
            onClick={() =>
              void run(async () => {
                await api.logout();
                onSignOut();
              }, false)
            }
          >
            <Icon name="logout" size={16} />
            <span className="menu-item-label">{copy.menu.signOut}</span>
          </button>
          {error && <p role="alert" className="menu-error">{error}</p>}
        </div>
      )}
    </div>
  );
}
