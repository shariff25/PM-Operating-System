import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import type { Source } from '../api';
import { copy } from '../copy';
import { Empty, ExtLink } from './bits';
import { Icon } from './icons';

// The sources a draft was written from, and the jump between a numbered marker in the text and
// its card. The owner's draft workspace and a reviewer's read of a draft share both, so the
// markers, cards and keyboard behaviour are the same in each.

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const reducedMotion = () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;

/**
 * Which source is picked, and the jump between it and its markers. `toSource` brings a card into
 * view and moves focus there; `toText` does the same for the first marker of the picked source in
 * the text. `view` is whatever decides what is on screen (a phone's tab): the jump runs once the
 * target is there.
 */
export function useSourceJump(view: unknown) {
  const [selected, setSelected] = useState<string | null>(null);
  const [jump, setJump] = useState<{ to: 'source' | 'text'; id: string; n: number } | null>(null);
  const [flash, setFlash] = useState<string | null>(null);

  useEffect(() => {
    if (!jump) return;
    if (jump.to === 'source') {
      const card = document.getElementById(`src-${jump.id}`);
      if (!card) return;
      card.querySelector<HTMLElement>('button.source')?.focus({ preventScroll: true });
      card.scrollIntoView?.({ block: 'center', behavior: reducedMotion() ? 'auto' : 'smooth' });
      setFlash(jump.id);
      const t = setTimeout(() => setFlash((f) => (f === jump.id ? null : f)), 1800);
      return () => clearTimeout(t);
    }
    const mark = document.querySelector<HTMLElement>('.sheet button.cite-on');
    mark?.focus({ preventScroll: true });
    mark?.scrollIntoView?.({ block: 'center', behavior: reducedMotion() ? 'auto' : 'smooth' });
  }, [jump, view]);

  return {
    selected,
    flash,
    /** Pressing a card: pick it, or put it down again. */
    pick: (sid: string) => setSelected((s) => (s === sid ? null : sid)),
    toSource: (sid: string) => {
      setSelected(sid);
      setJump((j) => ({ to: 'source', id: sid, n: (j?.n ?? 0) + 1 }));
    },
    toText: (sid: string) => setJump((j) => ({ to: 'text', id: sid, n: (j?.n ?? 0) + 1 })),
  };
}

/** The numbered source cards. With no sources, the plain sentence that says so, and `emptyAction` when there is one. */
export function SourceList({
  sources,
  places,
  selected,
  flash,
  onPick,
  onShowInText,
  emptyAction,
}: {
  sources: Source[];
  /** For each source id, how many places in the text cite it. */
  places: Record<string, number>;
  selected: string | null;
  flash: string | null;
  onPick: (id: string) => void;
  /** Phone only: a way back from a card to its markers in the text. */
  onShowInText?: (id: string) => void;
  emptyAction?: ReactNode;
}) {
  if (sources.length === 0) {
    return (
      <Empty icon="link" small action={emptyAction}>
        {copy.noSources}
      </Empty>
    );
  }
  // Cards tell which sources the draft relies on, once any does.
  const anyCited = sources.some((s) => s.cited);
  return (
    <ol className="sources">
      {sources.map((s, i) => (
        <li
          key={s.id}
          id={`src-${s.id}`}
          className={`source-item ${selected === s.id ? 'source-item-on' : ''} ${anyCited ? (s.cited ? 'source-item-cited' : 'source-item-uncited') : ''} ${flash === s.id ? 'source-item-flash' : ''}`}
        >
          <button type="button" className="source" aria-pressed={selected === s.id} onClick={() => onPick(s.id)}>
            <span className="source-num"><span className="sr-only">{copy.draft.sourceNumber(i + 1)}</span><span aria-hidden="true">{i + 1}</span></span>
            <span className="source-text">
              <span className="source-title">{s.title}</span>
              <span className="source-sys">
                <span className="sr-only">, </span>
                {capital(s.system)}
                {anyCited && !s.cited && (
                  <>
                    <span aria-hidden="true"> · </span>
                    <span className="sr-only">, </span>
                    <span className="source-uncited">{copy.draft.notCited}</span>
                  </>
                )}
              </span>
              {s.excerpt && <span className="source-excerpt">“{s.excerpt}”</span>}
            </span>
          </button>
          {selected === s.id && (places[s.id] ?? 0) > 0 && (
            <p className="source-cites">
              <span>{copy.draft.citedPlaces(places[s.id]!)}</span>
              {onShowInText && (
                <button type="button" className="source-show" onClick={() => onShowInText(s.id)}>
                  {copy.draft.showInDraft}
                </button>
              )}
            </p>
          )}
          {selected === s.id && (s.url ? (
            <ExtLink href={s.url} className="source-link" label={`: ${s.title}`}>
              {copy.draft.sourceOpen}
              <Icon name="external" size={13} />
            </ExtLink>
          ) : (
            <p className="source-link source-nolink">{copy.draft.sourceNoLink}</p>
          ))}
        </li>
      ))}
    </ol>
  );
}
