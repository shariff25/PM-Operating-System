import { useCallback, useEffect, useRef, useState } from 'react';
import { ApiError } from '../api';
import type { Api, AuditEvent } from '../api';
import { messageOf } from '../app-context';
import { copy } from '../copy';
import { filtersToSearch, rangeBackwards, toQuery } from './filters';
import type { AuditFilters } from './filters';

export const AUDIT_PAGE_SIZE = 50;

export interface AuditEvents {
  /** Null until the first page arrives. */
  events: AuditEvent[] | null;
  /** The first page failed: a plain sentence. */
  error: string | null;
  /** The next page failed. The events already shown are kept. */
  moreError: string | null;
  forbidden: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  /** The filters can match nothing (first day after the last), so nothing was asked. */
  backwards: boolean;
  loadMore: () => void;
  retry: () => void;
}

/**
 * The audit log for a set of filters, a page at a time. Changing the filters starts over from the
 * newest event. "Load more" sends the cursor back with the same filters, as the server asks.
 */
export function useAuditEvents(api: Api, filters: AuditFilters): AuditEvents {
  const key = filtersToSearch(filters);
  const backwards = rangeBackwards(filters);
  const [events, setEvents] = useState<AuditEvent[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const generation = useRef(0);
  const invalidate = useCallback(() => {
    generation.current += 1;
  }, []);
  // The filters of the pages on screen, so a cursor is never sent with different ones.
  const shown = useRef<AuditFilters>(filters);

  useEffect(() => {
    const mine = ++generation.current;
    shown.current = filters;
    setEvents(null);
    setCursor(null);
    setError(null);
    setMoreError(null);
    setForbidden(false);
    setLoadingMore(false);
    if (backwards) return;
    api
      .adminAudit({ ...toQuery(filters), limit: AUDIT_PAGE_SIZE })
      .then((page) => {
        if (mine !== generation.current) return;
        setEvents(page.events);
        setCursor(page.next_cursor);
      })
      .catch((e: unknown) => {
        if (mine !== generation.current) return;
        if (e instanceof ApiError && e.status === 403) setForbidden(true);
        else setError(messageOf(e, copy.admin.audit.loadFailed));
      });
    return invalidate;
    // `key` stands for the filters: the object changes identity on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, key, attempt]);

  const loadMore = useCallback(() => {
    if (!cursor || loadingMore) return;
    const mine = generation.current;
    setLoadingMore(true);
    setMoreError(null);
    api
      .adminAudit({ ...toQuery(shown.current), limit: AUDIT_PAGE_SIZE, before: cursor })
      .then((page) => {
        if (mine !== generation.current) return;
        setEvents((list) => [...(list ?? []), ...page.events]);
        setCursor(page.next_cursor);
      })
      .catch((e: unknown) => {
        if (mine !== generation.current) return;
        if (e instanceof ApiError && e.status === 403) setForbidden(true);
        else setMoreError(messageOf(e, copy.admin.audit.loadMoreFailed));
      })
      .finally(() => {
        if (mine === generation.current) setLoadingMore(false);
      });
  }, [api, cursor, loadingMore]);

  return {
    events,
    error,
    moreError,
    forbidden,
    hasMore: cursor !== null,
    loadingMore,
    backwards,
    loadMore,
    retry: () => setAttempt((n) => n + 1),
  };
}
