import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';

/** A menu button: Enter, Space or the arrow keys open it; arrows, Home and End move; Escape closes. */
export function UserMenu({ onSignOut }: { onSignOut: () => void }) {
  const { api, me, setMe } = useApp();
  const [open, setOpen] = useState(false);
  const [focusIdx, setFocusIdx] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);
  const COUNT = 3;

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
      setFocusIdx((i + COUNT) % COUNT);
      items.current[(i + COUNT) % COUNT]?.focus();
    };
    const cur = items.current.findIndex((el) => el === document.activeElement);
    if (e.key === 'ArrowDown') move(cur + 1);
    else if (e.key === 'ArrowUp') move(cur - 1);
    else if (e.key === 'Home') move(0);
    else if (e.key === 'End') move(COUNT - 1);
    else if (e.key === 'Escape') {
      e.preventDefault();
      close(true);
    } else if (e.key === 'Tab') close(false);
  };

  return (
    <div className="menu" ref={box}>
      <button
        ref={button}
        type="button"
        className="btn btn-quiet menu-button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? 'user-menu' : undefined}
        aria-label={copy.menu.button(me.name)}
        onClick={() => (open ? close(false) : openAt(0))}
        onKeyDown={onButtonKey}
      >
        <span>{me.name}</span>
        <span aria-hidden="true" className="menu-caret">▾</span>
      </button>
      {open && (
        <div id="user-menu" className="menu-panel" role="menu" aria-label={copy.menu.button(me.name)} onKeyDown={onMenuKey}>
          <button
            ref={(el) => { items.current[0] = el; }}
            type="button"
            role="menuitemcheckbox"
            aria-checked={me.nudges}
            tabIndex={-1}
            className="menu-item menu-row"
            onClick={() => void run(async () => setMe(await api.setNudges(!me.nudges)), false)}
          >
            <span>{copy.menu.nudges}</span>
            <span className={`pill ${me.nudges ? 'pill-on' : ''}`}>{me.nudges ? copy.menu.nudgesOn : copy.menu.nudgesOff}</span>
          </button>
          <button
            ref={(el) => { items.current[1] = el; }}
            type="button"
            role="menuitem"
            tabIndex={-1}
            className="menu-item"
            onClick={() => void run(async () => setMe(await api.resetHints()), true)}
          >
            {copy.menu.showHints}
          </button>
          <button
            ref={(el) => { items.current[2] = el; }}
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
            {copy.menu.signOut}
          </button>
          {error && <p role="alert" className="hint-error">{error}</p>}
        </div>
      )}
    </div>
  );
}
