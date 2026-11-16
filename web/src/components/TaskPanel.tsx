import { useMemo } from 'react';
import type { Task } from '../api';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { renderMarkdown } from '../markdown';
import { Markdown, StrippedNote } from './bits';

export function isSettled(t: Task): boolean {
  return t.status !== 'queued' && t.status !== 'working';
}

export function TaskPanel({ task, onRetry, onClear }: { task: Task; onRetry: () => void; onClear: () => void }) {
  const { navigate } = useApp();
  const events = task.events ?? [];
  const rendered = useMemo(() => renderMarkdown(task.result ?? ''), [task.result]);
  const busy = events.some((e) => e.kind === 'retry') && !isSettled(task);
  return (
    <section className="card task" aria-label="Your request" aria-live="polite">
      <p className="muted">
        {copy.task.youAsked} {task.text}
      </p>
      <p className="task-status">
        <strong>{copy.task.status[task.status]}</strong>
      </p>
      {events.length > 0 && (
        <ol className="milestones">
          {events.map((e, i) => (
            <li key={`${e.at}-${i}`}>{e.text}</li>
          ))}
        </ol>
      )}
      {busy && <p className="muted">{copy.modelBusy}</p>}
      {task.result && (
        <div>
          <h3 className="sr-only">{copy.task.resultHeading}</h3>
          <Markdown rendered={rendered} />
          <StrippedNote parts={rendered.stripped} what={copy.stripped.answer} />
        </div>
      )}
      {task.status === 'failed' && (
        <div className="note note-error" role="alert">
          <p>
            {copy.task.failedWhat} {task.error ?? copy.errors.unknown}
          </p>
          <button type="button" className="btn btn-primary" onClick={onRetry}>
            {copy.task.tryAgain}
          </button>
        </div>
      )}
      <div className="row">
        {task.draft_ids.map((id) => (
          <button key={id} type="button" className="btn btn-primary" onClick={() => navigate(`/d/${encodeURIComponent(id)}`)}>
            {copy.task.openDraft}
          </button>
        ))}
        {isSettled(task) && (
          <button type="button" className="btn btn-quiet" onClick={onClear}>
            {copy.task.dismiss}
          </button>
        )}
      </div>
    </section>
  );
}
