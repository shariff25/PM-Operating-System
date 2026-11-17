import { copy } from '../copy';
import { Skeleton } from './bits';

/** Shown while the Admin area's code is on its way: the page's own shape, never a blank flash. */
export function AdminLoading() {
  return (
    <div className="page page-wide admin-loading">
      <div className="admin-loading-head" aria-hidden="true">
        <span className="skeleton-bar skeleton-bar-a" />
        <span className="skeleton-bar skeleton-bar-b" />
      </div>
      <p className="muted small admin-loading-note">{copy.admin.openingNote}</p>
      <Skeleton rows={4} label={copy.admin.opening} variant="cards" />
    </div>
  );
}
