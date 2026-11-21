import { useEffect, useId, useRef, useState } from 'react';
import type { FocusEvent, KeyboardEvent } from 'react';
import type { BotCard } from '../api';
import { copy } from '../copy';
import { BotFace, Words } from './BotParts';
import { Icon } from './icons';

const P = copy.bots.picker;

/** A bot the picker can show: the catalog's card, or Nudge before the catalog has loaded. */
export interface PickerBot {
  key: string;
  name: string;
  mascot: string;
  title: string;
  summary: string;
  preview: boolean;
}

export const pickerBot = (b: BotCard): PickerBot => ({
  key: b.key,
  name: b.display_name,
  mascot: b.mascot,
  title: b.title,
  summary: b.summary,
  preview: b.preview === true,
});

/**
 * The bot picker in the ask box (docs/bot-marketplace-spec.md §8, mock-up 2): a button showing the
 * selected bot's face and name, opening a list of the bots on the team (a `listbox`: arrow keys,
 * Home and End, type-ahead, Enter or Space to choose, Escape to close), then the offered bots not
 * on the team, greyed, each with "Add", and a link to the Bots page. Choosing never sends anything.
 */
export function BotPicker({
  team,
  others,
  value,
  onChange,
  onAdd,
  adding,
  disabled,
}: {
  /** The bots on the team, Nudge first. */
  team: PickerBot[];
  /** Offered bots not on the team. */
  others: PickerBot[];
  value: string;
  onChange: (key: string) => void;
  /** Add a bot to the team (`via: "picker"`); resolves true when it was added. */
  onAdd: (key: string) => Promise<boolean>;
  adding: string | null;
  disabled?: boolean;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const typed = useRef({ text: '', at: 0 });
  const selected = team.find((b) => b.key === value) ?? team[0];

  const show = () => {
    setActive(Math.max(0, team.findIndex((b) => b.key === value)));
    setOpen(true);
  };
  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  useEffect(() => {
    if (open) list.current?.focus();
  }, [open]);

  // A click outside closes it.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrap.current && e.target instanceof Node && !wrap.current.contains(e.target)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  const choose = (i: number) => {
    const b = team[i];
    if (!b) return;
    onChange(b.key);
    close(true);
  };

  const onButtonKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      show();
    }
  };

  const onListKey = (e: KeyboardEvent<HTMLUListElement>) => {
    const last = team.length - 1;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        setActive((a) => Math.min(last, a + 1));
        return;
      case 'ArrowUp':
        e.preventDefault();
        setActive((a) => Math.max(0, a - 1));
        return;
      case 'Home':
        e.preventDefault();
        setActive(0);
        return;
      case 'End':
        e.preventDefault();
        setActive(last);
        return;
      case 'Enter':
      case ' ':
        e.preventDefault();
        choose(active);
        return;
      case 'Escape':
        e.preventDefault();
        close(true);
        return;
    }
    // Type-ahead: letters typed in quick succession jump to the first bot whose name starts with them.
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const now = Date.now();
      typed.current = { text: (now - typed.current.at < 700 ? typed.current.text : '') + e.key.toLowerCase(), at: now };
      const hit = team.findIndex((b) => b.name.toLowerCase().startsWith(typed.current.text));
      if (hit >= 0) setActive(hit);
    }
  };

  const onPopKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close(true);
    }
  };

  // Focus leaving the picker (Tab past its last link) closes it.
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (open && wrap.current && !(e.relatedTarget instanceof Node && wrap.current.contains(e.relatedTarget))) setOpen(false);
  };

  const add = async (key: string) => {
    const ok = await onAdd(key);
    if (ok) close(true);
  };

  if (!selected) return null;
  return (
    <div className="picker" ref={wrap} onBlur={onBlur}>
      <button
        ref={button}
        type="button"
        className="picker-button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? `${id}-list` : undefined}
        onClick={() => (open ? close(false) : show())}
        onKeyDown={onButtonKey}
        disabled={disabled}
        data-bot={selected.key}
      >
        <BotFace mascot={selected.mascot} size={22} />
        <span className="picker-name" aria-hidden="true">{selected.name}</span>
        <span className="sr-only">{P.button(selected.name)}</span>
        <Icon name="chevronDown" size={14} className="picker-caret" />
      </button>
      {open && (
        <div className="picker-pop" onKeyDown={onPopKey}>
          <p className="micro-label picker-label" id={`${id}-label`}>{P.listLabel}</p>
          <ul
            ref={list}
            id={`${id}-list`}
            className="picker-list"
            role="listbox"
            aria-labelledby={`${id}-label`}
            tabIndex={0}
            aria-activedescendant={`${id}-opt-${active}`}
            onKeyDown={onListKey}
          >
            {team.map((b, i) => (
              <li
                key={b.key}
                id={`${id}-opt-${i}`}
                role="option"
                aria-selected={b.key === value}
                className={`picker-option ${i === active ? 'picker-option-active' : ''}`}
                onClick={() => choose(i)}
                onMouseMove={() => setActive(i)}
                data-bot={b.key}
              >
                <BotFace mascot={b.mascot} size={28} />
                <span className="picker-text">
                  <span className="picker-option-name">{b.name}</span>
                  <span className="picker-option-line">{b.title}{b.preview ? ` · ${copy.bots.preview}` : ''}</span>
                </span>
                {b.key === value && <Icon name="check" size={16} weight={2.4} className="picker-check" />}
              </li>
            ))}
          </ul>
          {others.length > 0 && (
            <ul className="picker-others" aria-label={P.notOnTeam}>
              {others.map((b) => (
                <li key={b.key} className="picker-other" data-bot={b.key}>
                  <span className="picker-other-face"><BotFace mascot={b.mascot} size={28} /></span>
                  <span className="picker-text">
                    <span className="picker-option-name">{b.name}</span>
                    <span className="picker-option-line">
                      {b.title}{b.preview ? ` · ${copy.bots.preview}` : ''} · {P.notOnTeamShort}
                    </span>
                  </span>
                  <button type="button" className="picker-add" onClick={() => void add(b.key)} disabled={adding !== null}>
                    <Words shown={P.add} full={P.addLabel(b.name)} />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <a className="picker-manage" href="#/bots" onClick={() => setOpen(false)}>
            {P.manage}
          </a>
        </div>
      )}
    </div>
  );
}
