import { createContext, useContext, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { ADMIN_SECTIONS, canAdminister, canSee, sectionHref } from '../admin/access';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { useData } from '../data';
import { PHONE_QUERY, useMediaQuery } from '../theme';
import type { Route } from '../url';
import { Icon, Mark } from './icons';
import type { IconName } from './icons';
import { ResetNotice } from './ResetNotice';
import { UserMenu } from './UserMenu';

type NavKey = 'home' | 'drafts' | 'reviews' | 'followups' | 'activity' | 'admin' | 'security';

interface Slots {
  /** Where a page puts its title in the phone top bar. Null on desktop, where pages carry their own header. */
  lead: HTMLElement | null;
  isPhone: boolean;
}

const SlotContext = createContext<Slots>({ lead: null, isPhone: false });
export const useShell = () => useContext(SlotContext);

/** Render into the phone top bar's title slot. On desktop the page shows its own header instead. */
export function TopBarLead({ children }: { children: ReactNode }) {
  const { lead } = useShell();
  return lead ? createPortal(children, lead) : null;
}

/**
 * The white bar at the top of a detail page: the breadcrumb, then whatever the page adds
 * (a status, actions). On a phone the breadcrumb goes into the top bar instead.
 */
export function PageBar({ crumb, children }: { crumb: ReactNode; children?: ReactNode }) {
  const { isPhone } = useShell();
  if (isPhone) return <TopBarLead>{crumb}</TopBarLead>;
  return (
    <div className="pagebar">
      {crumb}
      {children}
    </div>
  );
}

export function navKey(route: Route): NavKey {
  switch (route.name) {
    case 'drafts':
    case 'draft':
      return 'drafts';
    case 'reviews':
    case 'review':
      return 'reviews';
    case 'followups':
      return 'followups';
    case 'activity':
    case 'task':
      return 'activity';
    case 'admin':
      return 'admin';
    case 'security':
      return 'security';
    default:
      return 'home';
  }
}

export function isMac(): boolean {
  return typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/** A connected system and how it is doing, for the rail's Connected block. */
export interface ConnectedSystem {
  name: string;
  ok: boolean;
  detail: string;
}

/**
 * The rail's "Connected" block. The API has no connector status for a PM yet, so today this
 * is given nothing and renders nothing. It never shows made-up statuses.
 */
export function ConnectedSystems({ systems }: { systems?: ConnectedSystem[] | null }) {
  if (!systems || systems.length === 0) return null;
  return (
    <section className="rail-connected" aria-label={copy.nav.connected}>
      <h2 className="rail-label">{copy.nav.connected}</h2>
      <ul>
        {systems.map((s) => (
          <li key={s.name}>
            <span className={`rail-dot ${s.ok ? 'rail-dot-ok' : 'rail-dot-warn'}`} aria-hidden="true" />
            <span className="rail-connected-name">{s.name}</span>
            <span className={`rail-connected-detail ${s.ok ? '' : 'rail-connected-warn'}`}>{s.detail}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

interface NavItem {
  key: NavKey;
  href: string;
  label: string;
  icon: IconName;
  count: number;
  countLabel: (n: number) => string;
  /** The accent badge is for drafts only: the one count that asks for a decision. */
  accent?: boolean;
}


export function Shell({ route, children, onSignOut }: { route: Route; children: ReactNode; onSignOut: () => void }) {
  const { openPalette, me } = useApp();
  const { pendingCount, brief, briefState, runningCount, reviewCount } = useData();
  const isPhone = useMediaQuery(PHONE_QUERY);
  const [lead, setLead] = useState<HTMLElement | null>(null);
  const current = navKey(route);
  // The draft workspace (and a reviewer's read of a draft) needs the width: there the rail is icons only.
  const compact = !isPhone && (route.name === 'draft' || route.name === 'review');

  const items: NavItem[] = [
    { key: 'home', href: '#/', label: copy.nav.home, icon: 'home', count: 0, countLabel: () => '' },
    { key: 'drafts', href: '#/drafts', label: copy.nav.drafts, icon: 'drafts', count: pendingCount, countLabel: copy.nav.draftsCount, accent: true },
    // Only for someone holding the reviewer role (not for an admin who merely may recommend).
    ...(me.reviewer === true
      ? [{ key: 'reviews' as const, href: '#/reviews', label: copy.nav.reviews, icon: 'review' as const, count: reviewCount, countLabel: copy.nav.reviewsCount }]
      : []),
    ...(briefState === 'ready'
      ? [{
          key: 'followups' as const,
          href: '#/followups',
          label: copy.nav.followups,
          icon: 'followups' as const,
          count: brief?.waiting_on_you.length ?? 0,
          countLabel: copy.nav.followupsCount,
        }]
      : []),
    { key: 'activity', href: '#/activity', label: copy.nav.activity, icon: 'activity', count: runningCount, countLabel: copy.nav.activityCount },
    ...(canAdminister(me)
      ? [{ key: 'admin' as const, href: '#/admin', label: copy.nav.admin, icon: 'shield' as const, count: 0, countLabel: () => '' }]
      : []),
  ];

  const detail = route.name === 'draft' || route.name === 'task' || route.name === 'review';
  const shortcut = isMac() ? copy.nav.shortcutMac : copy.nav.shortcutOther;
  const pageTitle = { home: copy.nav.home, drafts: copy.nav.drafts, reviews: copy.nav.reviews, followups: copy.nav.followups, activity: copy.nav.activity, admin: copy.nav.admin, security: copy.password.security.title }[current];

  /** The second level under Admin: its parts, for the ones this person may open. */
  const adminSub = () => {
    const here = route.name === 'admin' ? (route.section === 'overview' && !canSee(me, 'overview') ? ADMIN_SECTIONS.find((s) => canSee(me, s)) : route.section) : null;
    return (
      <ul className="rail-sub" aria-label={copy.admin.subnavLabel}>
        {ADMIN_SECTIONS.filter((s) => canSee(me, s)).map((s) => (
          <li key={s}>
            <a href={sectionHref(s)} className="rail-subitem" aria-current={s === here ? 'page' : undefined}>
              {copy.admin.sections[s]}
            </a>
          </li>
        ))}
      </ul>
    );
  };

  const link = (it: NavItem, variant: 'rail' | 'tabs') => {
    const on = it.key === current;
    const iconOnly = variant === 'rail' && compact;
    return (
      <li key={it.key}>
        <a
          href={it.href}
          className={variant === 'rail' ? 'rail-item' : 'tabbar-item'}
          aria-current={on && !(it.key === 'admin' && variant === 'rail' && !compact) ? 'page' : undefined}
          data-active={on ? 'true' : undefined}
          title={iconOnly ? it.label : undefined}
        >
          <span className="nav-icon">
            <Icon name={it.icon} size={variant === 'rail' ? (iconOnly ? 18 : 16) : 22} />
            {it.count > 0 && (variant === 'tabs' || iconOnly) && (
              <span className={`nav-dot ${it.accent ? 'nav-dot-accent' : ''}`} aria-hidden="true" />
            )}
          </span>
          <span className={iconOnly ? 'sr-only' : 'nav-label'}>{it.label}</span>
          {it.count > 0 && (
            <span className={`nav-count ${it.accent && variant === 'rail' && !iconOnly ? 'nav-count-accent' : ''}`}>
              {variant === 'rail' && !iconOnly && <span aria-hidden="true">{it.count}</span>}
              <span className="sr-only">, {it.countLabel(it.count)}</span>
            </span>
          )}
        </a>
        {variant === 'rail' && !compact && it.key === 'admin' && current === 'admin' && adminSub()}
      </li>
    );
  };

  return (
    <SlotContext.Provider value={{ lead, isPhone }}>
      <a href="#main" className="skip-link" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
        {copy.nav.skip}
      </a>
      <div className={`app ${compact ? 'app-compact' : ''} ${isPhone ? 'app-phone' : ''}`}>
        {!isPhone && (
          <header className={`rail ${compact ? 'rail-compact' : ''}`}>
            <a href="#/" className="rail-brand" aria-label={`${copy.appName}, ${copy.nav.home}`}>
              <Mark size={compact ? 28 : 24} />
              {!compact && <span className="rail-brand-name">{copy.appName}</span>}
            </a>
            <button
              type="button"
              className="rail-ask"
              onClick={() => openPalette()}
              aria-label={compact ? copy.nav.askButton : undefined}
              title={compact ? `${copy.nav.askButton} (${shortcut})` : undefined}
              aria-keyshortcuts={isMac() ? 'Meta+K' : 'Control+K'}
            >
              <Icon name="search" size={compact ? 18 : 15} />
              {!compact && (
                <>
                  <span className="rail-ask-text">{copy.nav.askButton}</span>
                  <kbd className="kbd kbd-rail">{shortcut}</kbd>
                </>
              )}
            </button>
            <nav aria-label={copy.nav.label} className="rail-nav">
              <ul className="rail-list">{items.map((it) => link(it, 'rail'))}</ul>
            </nav>
            <ConnectedSystems />
            <div className="rail-foot">
              <UserMenu variant="rail" collapsed={compact} onSignOut={onSignOut} />
            </div>
          </header>
        )}

        <div className="content">
          {isPhone && (
            <header className="topbar">
              {detail ? (
                <a href={route.name === 'draft' ? '#/drafts' : route.name === 'review' ? '#/reviews' : '#/activity'} className="icon-btn topbar-btn" aria-label={copy.nav.back}>
                  <Icon name="chevronLeft" size={20} />
                </a>
              ) : (
                <a href="#/" className="topbar-brand" aria-label={`${copy.appName}, ${copy.nav.home}`}>
                  <Mark size={28} />
                </a>
              )}
              <div className="topbar-lead" ref={setLead}>
                {!detail && <span className="topbar-title">{pageTitle}</span>}
              </div>
              <button
                type="button"
                className="icon-btn topbar-btn"
                onClick={() => openPalette()}
                aria-label={copy.nav.askButton}
                aria-keyshortcuts={isMac() ? 'Meta+K' : 'Control+K'}
              >
                <Icon name="search" size={20} />
              </button>
              <UserMenu variant="compact" onSignOut={onSignOut} />
            </header>
          )}
          <main id="main" tabIndex={-1} className={`main main-${route.name}`}>
            <ResetNotice />
            {children}
          </main>
        </div>

        {isPhone && (
          <nav aria-label={copy.nav.label} className="tabbar">
            <ul className="tabbar-list">{items.map((it) => link(it, 'tabs'))}</ul>
          </nav>
        )}
      </div>
    </SlotContext.Provider>
  );
}
