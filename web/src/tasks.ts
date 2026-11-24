import type { Task, TaskEvent, TaskProgress } from './api/types';

export function isSettled(t: Task): boolean {
  return t.status !== 'queued' && t.status !== 'working';
}

/** The steps the PM sees. The backend sends milestones only; this keeps that promise explicit. */
export function milestones(t: Task): TaskEvent[] {
  return (t.events ?? []).filter((e) => e.kind === 'milestone');
}

/** The model-busy line shows while the server says the task is running again. */
export function isRetrying(t: Task): boolean {
  return !isSettled(t) && t.retrying === true;
}

/** A usable progress, or null: never `0 of 0`, never done above total. */
export function usableProgress(t: Task): TaskProgress | null {
  const p = t.progress;
  if (!p || !(p.total > 0)) return null;
  return { done: Math.min(Math.max(p.done, 0), p.total), total: p.total };
}
