import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { copy } from '../copy';
import { Icon } from './icons';

type Tone = 'good' | 'bad' | 'plain';
interface ToastItem {
  id: number;
  text: string;
  tone: Tone;
}

const ToastContext = createContext<(text: string, tone?: Tone) => void>(() => {});

export function useToast() {
  return useContext(ToastContext);
}

/**
 * Short confirmations (saved, approved) and errors that have no other place to show. They live in
 * one polite live region; the page itself still shows anything the PM must act on.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const seq = useRef(0);
  const timers = useRef(new Map<number, ReturnType<typeof setTimeout>>());

  const close = useCallback((id: number) => {
    setItems((list) => list.filter((t) => t.id !== id));
    const timer = timers.current.get(id);
    if (timer) clearTimeout(timer);
    timers.current.delete(id);
  }, []);

  const push = useCallback(
    (text: string, tone: Tone = 'plain') => {
      const id = ++seq.current;
      setItems((list) => [...list.slice(-2), { id, text, tone }]);
      timers.current.set(id, setTimeout(() => close(id), 4000));
    },
    [close],
  );

  useEffect(() => {
    const all = timers.current;
    return () => all.forEach((t) => clearTimeout(t));
  }, []);

  const value = useMemo(() => push, [push]);

  return (
    <ToastContext.Provider value={value}>
      {children}
      <div className="toasts" aria-live="polite">
        {items.map((t) => (
          <div key={t.id} className={`toast toast-${t.tone}`}>
            <Icon name={t.tone === 'bad' ? 'alert' : 'check'} size={16} className="toast-icon" />
            <span>{t.text}</span>
            <button type="button" className="toast-close" aria-label={copy.toast.close} onClick={() => close(t.id)}>
              <Icon name="x" size={14} />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
