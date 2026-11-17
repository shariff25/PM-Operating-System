import { useEffect } from 'react';
import type { Task } from '../api';
import { useApp } from '../app-context';
import { Empty, NewRequestButton, PageHeader, Skeleton, TaskPill } from '../components/bits';
import { copy } from '../copy';
import { useData } from '../data';
import { relTime } from '../time';
import { taskHref } from '../url';

/** Requests as dense rows: what was asked, its status, and when. `compact` drops the column heads. */
export function TaskTable({ tasks, label, compact }: { tasks: Task[]; label: string; compact?: boolean }) {
  const now = Date.now();
  return (
    <div className="table">
      {!compact && (
        <div className="thead tasks-grid" aria-hidden="true">
          <span>{copy.activity.columns.request}</span>
          <span>{copy.activity.columns.status}</span>
          <span className="tcell-time">{copy.activity.columns.created}</span>
        </div>
      )}
      <ul className="tbody" aria-label={label}>
        {tasks.map((t) => (
          <li key={t.id}>
            <a className={`trow tasks-grid ${compact ? 'trow-compact' : ''}`} href={taskHref(t.id)}>
              <span className="tcell tcell-title">
                <span className="row-main">
                  <span className="row-title">{t.text}</span>
                  {t.status === 'failed' && t.error && <span className="row-meta">{t.error}</span>}
                </span>
              </span>
              <span className="tcell tcell-status"><TaskPill status={t.status} /></span>
              <span className="tcell tcell-time">{relTime(t.created_at, now)}</span>
            </a>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The PM's recent requests, newest first. Each opens as its thread. */
export function Activity() {
  const { openPalette } = useApp();
  const { tasks, reloadTasks } = useData();
  useEffect(() => {
    void reloadTasks();
  }, [reloadTasks]);

  return (
    <div className="page">
      <PageHeader title={copy.activity.title} lede={copy.activity.lede} eyebrow={tasks ? copy.activity.countLine(tasks.length) : undefined}>
        <NewRequestButton />
      </PageHeader>
      <section className="card card-table" aria-label={copy.activity.title}>
        {!tasks && <div className="card-body"><Skeleton rows={4} label={copy.activity.loading} /></div>}
        {tasks && tasks.length === 0 && (
          <Empty
            icon="activity"
            action={<button type="button" className="btn btn-sm" onClick={() => openPalette()}>{copy.emptyActions.askFirst}</button>}
          >
            {copy.activity.empty}
          </Empty>
        )}
        {tasks && tasks.length > 0 && <TaskTable tasks={tasks} label={copy.activity.title} />}
      </section>
    </div>
  );
}
