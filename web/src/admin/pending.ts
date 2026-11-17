import { useCallback, useEffect, useState } from 'react';
import type { SetStateAction } from 'react';

/**
 * Unsaved changes, kept in memory for as long as this signed-in app lives. They belong to the API
 * client (one per sign-in), so moving to another page and back keeps them, a new sign-in starts
 * clean, and nothing is ever written to the browser's storage.
 */
const stores = new WeakMap<object, Map<string, unknown>>();

function storeOf(owner: object): Map<string, unknown> {
  let store = stores.get(owner);
  if (!store) {
    store = new Map();
    stores.set(owner, store);
  }
  return store;
}

export function usePending<T>(owner: object, key: string, initial: T): [T, (next: SetStateAction<T>) => void] {
  const [value, setValue] = useState<T>(() => {
    const store = storeOf(owner);
    return store.has(key) ? (store.get(key) as T) : initial;
  });
  const set = useCallback(
    (next: SetStateAction<T>) =>
      setValue((prev) => {
        const v = typeof next === 'function' ? (next as (p: T) => T)(prev) : next;
        storeOf(owner).set(key, v);
        return v;
      }),
    [owner, key],
  );
  return [value, set];
}

/** While there are unsaved changes, closing or reloading the tab asks first. */
export function useLeaveGuard(dirty: boolean) {
  useEffect(() => {
    if (!dirty) return;
    const onLeave = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      // Older browsers need a value here; the words are the browser's own.
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onLeave);
    return () => window.removeEventListener('beforeunload', onLeave);
  }, [dirty]);
}
