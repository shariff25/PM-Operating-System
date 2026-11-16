import type { Task, TaskEvent } from './api/types';

export function isSettled(t: Task): boolean {
  return t.status !== 'queued' && t.status !== 'working';
}

/**
 * The steps the PM sees. Only `milestone` events are meant for people; everything else
 * (queued, claimed, started, "2 of 3 done", retries) is bookkeeping and stays hidden.
 */
export function milestones(t: Task): TaskEvent[] {
  return (t.events ?? []).filter((e) => e.kind === 'milestone');
}

/** The model-busy line shows while a retry is happening and the task is not settled. */
export function isRetrying(t: Task): boolean {
  return !isSettled(t) && (t.events ?? []).some((e) => e.kind === 'retry');
}
