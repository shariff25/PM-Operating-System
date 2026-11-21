import { canSee, sectionsFor } from '../../admin/access';
import type { AdminSection } from '../../admin/access';
import { useApp } from '../../app-context';
import type { PolicyPart } from '../../url';
import { AdminBots } from './AdminBots';
import { AuditLog } from './AuditLog';
import { Connectors } from './Connectors';
import { Content } from './Content';
import { AdminOverview } from './Overview';
import { NoAccess } from './parts';
import { Policy } from './Policy';
import { Team } from './Team';

/**
 * The page for `#/admin` and the parts below it. A part this person's role does not include is a
 * calm "no access" page, never a broken one: the server checks every request again anyway.
 */
export function AdminPage({ section, search, sub }: { section: AdminSection; search: string; sub?: PolicyPart }) {
  const { me } = useApp();
  // Someone who may open the Admin area but not its home (a connectors-only role) lands on their first part.
  const target = section === 'overview' && !canSee(me, 'overview') ? sectionsFor(me)[0] ?? section : section;
  if (!canSee(me, target)) return <NoAccess />;
  switch (target) {
    case 'audit':
      return <AuditLog search={search} />;
    case 'team':
      return <Team search={search} />;
    case 'bots':
      return <AdminBots />;
    case 'connectors':
      return <Connectors />;
    case 'policy':
      return <Policy part={sub} />;
    case 'content':
      return <Content search={search} />;
    default:
      return <AdminOverview />;
  }
}
