import { canSee, sectionsFor } from '../../admin/access';
import type { AdminSection } from '../../admin/access';
import { useApp } from '../../app-context';
import { AuditLog } from './AuditLog';
import { Connectors } from './Connectors';
import { AdminOverview } from './Overview';
import { NoAccess } from './parts';
import { Team } from './Team';

/**
 * The page for `#/admin` and the three below it. A part this person's role does not include is a
 * calm "no access" page, never a broken one: the server checks every request again anyway.
 */
export function AdminPage({ section, search }: { section: AdminSection; search: string }) {
  const { me } = useApp();
  // Someone who may open the Admin area but not its home (a connectors-only role) lands on their first part.
  const target = section === 'overview' && !canSee(me, 'overview') ? sectionsFor(me)[0] ?? section : section;
  if (!canSee(me, target)) return <NoAccess />;
  switch (target) {
    case 'audit':
      return <AuditLog search={search} />;
    case 'team':
      return <Team />;
    case 'connectors':
      return <Connectors />;
    default:
      return <AdminOverview />;
  }
}
