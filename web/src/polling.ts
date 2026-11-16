import { useEffect, useRef } from 'react';

/**
 * How long to wait before the next check of a running request: quick at first, because most
 * answers arrive within seconds, then easing off. `elapsedMs` counts from when polling began.
 */
export function pollDelay(elapsedMs: number): number {
  if (elapsedMs < 6_000) return 600;
  if (elapsedMs < 60_000) return 2_000;
  return 5_000;
}

/**
 * Call `tick` repeatedly while `active`. It pauses while the tab is hidden and checks again
 * at once when the tab becomes visible or the window gets focus. `fixedMs` (used by tests)
 * replaces the schedule. `tick` handles its own errors.
 */
export function usePoll(active: boolean, tick: () => Promise<void>, fixedMs?: number): void {
  const latest = useRef(tick);
  useEffect(() => {
    latest.current = tick;
  });
  useEffect(() => {
    if (!active) return;
    const start = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let stopped = false;
    let running = false;

    const schedule = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      if (stopped || document.hidden) return;
      timer = setTimeout(() => void run(), fixedMs ?? pollDelay(Date.now() - start));
    };
    const run = async () => {
      if (running || stopped) return;
      running = true;
      try {
        await latest.current();
      } finally {
        running = false;
      }
      schedule();
    };
    const wake = () => {
      if (!document.hidden) void run();
    };

    schedule();
    document.addEventListener('visibilitychange', wake);
    window.addEventListener('focus', wake);
    return () => {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
      document.removeEventListener('visibilitychange', wake);
      window.removeEventListener('focus', wake);
    };
  }, [active, fixedMs]);
}
