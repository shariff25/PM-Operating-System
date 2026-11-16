import { createContext, useContext, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { useData } from '../data';
import { PHONE_QUERY, useMediaQuery } from '../theme';
import type { Route } from '../url';
import { Icon, Mark } from './icons';
import type { IconName } from './icons';
import { UserMenu } from './UserMenu';

type NavKey = 'home' | 'drafts' | 'followups' | 'activity';

interface Slots {
  /** Where a page puts its breadcrumb in the top bar. */
  lead: HTMLElement | null;
  /** Where a page puts its actions in the top bar (desktop only). */
  actions: HTMLElement | null;
  isPhone: boolean;
}

const SlotContext = createContext<Slots>({ lead: null, actions: null, isPhone: false });
export const useShell = () => useContext(SlotContext);

/** Render into the top bar's breadcrumb slot. */
export function TopBarLead({ children }: { children: ReactNode }) {
  const { lead } = useShell();
  return lead ? createPortal(children, lead) : null;
}

/** Render into the top bar's action slot; null on phone, where pages show actions in place. */
export function TopBarActions({ children }: { children: ReactNode }) {
  const { actions, isPhone } = useShell();
  return actions && !isPhone ? createPortal(children, actions) : null;
}

export function navKey(route: Route): NavKey {
  switch (route.name) {
    case 'drafts':
    case 'draft':
      return 'drafts';
    case 'followups':
      return 'followups';
    case 'activity':
    case 'task':
      return 'activity';
    default:
      return 'home';
  }
}

const SIDEBAR_KEY = 'pmos-sidebar';
function readCollapsed(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_KEY) === 'collapsed';
  } catch {
    return false;
  }
}

export function isMac(): boolean {
  return typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

interface NavItem {
  key: NavKey;
  href: string;
  label: string;
  icon: IconName;
  count: number;
  countLabel: (n: number) => string;
}

export function Shell({ route, children, onSignOut }: { route: Route; children: ReactNode; onSignOut: () => void }) {
  const { openPalette } = useApp();
  const { pendingCount, brief, briefState, runningCount } = useData();
  const isPhone = useMediaQuery(PHONE_QUERY);
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [lead, setLead] = useState<HTMLElement | null>(null);
  const [actions, setActions] = useState<HTMLElement | null>(null);
  const current = navKey(route);

  const toggle = () => {
    const next = !collapsed;
    setCollapsed(next);
    try {
      window.localStorage.setItem(SIDEBAR_KEY, next ? 'collapsed' : 'open');
    } catch {
      /* it still collapses for this visit */
    }
  };

  const items: NavItem[] = [
    { key: 'home', href: '#/', label: copy.nav.home, icon: 'home', count: 0, countLabel: () => '' },
    { key: 'drafts', href: '#/drafts', label: copy.nav.drafts, icon: 'drafts', count: pendingCount, countLabel: copy.nav.draftsCount },
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
  ];

  const pageTitle = { home: copy.nav.home, drafts: copy.nav.drafts, followups: copy.nav.followups, activity: copy.nav.activity }[current];
  const detail = route.name === 'draft' || route.name === 'task';
  const shortcut = isMac() ? copy.nav.shortcutMac : copy.nav.shortcutOther;

  const link = (it: NavItem, variant: 'side' | 'tabs') => {
    const on = it.key === current;
    return (
      <li key={it.key}>
        <a
          href={it.href}
          className={variant === 'side' ? 'nav-item' : 'tabbar-item'}
          aria-current={on ? 'page' : undefined}
          title={variant === 'side' && collapsed ? it.label : undefined}
        >
          <span className="nav-icon">
            <Icon name={it.icon} size={variant === 'side' ? 16 : 22} />
            {it.count > 0 && (variant === 'tabs' || collapsed) && <span className="nav-dot" aria-hidden="true" />}
          </span>
          <span className={variant === 'side' && collapsed ? 'sr-only' : 'nav-label'}>{it.label}</span>
          {it.count > 0 && (
            <span className="nav-count">
              {variant === 'side' && !collapsed && <span aria-hidden="true">{it.count}</span>}
              <span className="sr-only">, {it.countLabel(it.count)}</span>
            </span>
          )}
        </a>
      </li>
    );
  };

  // The sidebar groups its items like a console: Home, then Inbox, then History.
  const groups: { label: string | null; keys: NavKey[] }[] = [
    { label: null, keys: ['home'] },
    { label: copy.nav.groupInbox, keys: ['drafts', 'followups'] },
    { label: copy.nav.groupHistory, keys: ['activity'] },
  ];
  const sideNav = (
    <div className="nav-groups">
      {groups.map((g) => {
        const list = items.filter((it) => g.keys.includes(it.key));
        if (list.length === 0) return null;
        return (
          <div key={g.keys[0]} className="nav-group">
            {g.label && !collapsed && <span className="nav-group-label" aria-hidden="true">{g.label}</span>}
            <ul className="nav-list">{list.map((it) => link(it, 'side'))}</ul>
          </div>
        );
      })}
    </div>
  );
  const tabNav = <ul className="tabbar-list">{items.map((it) => link(it, 'tabs'))}</ul>;

  return (
    <SlotContext.Provider value={{ lead, actions, isPhone }}>
      <a href="#main" className="skip-link" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
        {copy.nav.skip}
      </a>
      <div className={`app ${collapsed && !isPhone ? 'app-collapsed' : ''} ${isPhone ? 'app-phone' : ''}`}>
        {!isPhone && (
          <aside className="sidebar">
            <div className="sidebar-head">
              <a href="#/" className="brand" aria-label={`${copy.appName}, ${copy.nav.home}`}>
                <Mark size={24} />
                {!collapsed && <span className="brand-name">{copy.appName}</span>}
              </a>
              <button
                type="button"
                className="icon-btn sidebar-toggle"
                aria-label={collapsed ? copy.nav.expand : copy.nav.collapse}
                aria-expanded={!collapsed}
                onClick={toggle}
              >
                <Icon name="sidebar" size={18} />
              </button>
            </div>
            <nav aria-label={copy.nav.label} className="sidebar-nav">
              {sideNav}
            </nav>
            <div className="sidebar-foot">
              <UserMenu variant="sidebar" collapsed={collapsed} onSignOut={onSignOut} />
            </div>
          </aside>
        )}

        <div className="content">
          <header className="topbar">
            {isPhone && (detail ? (
              <a href={route.name === 'draft' ? '#/drafts' : '#/activity'} className="icon-btn" aria-label={copy.nav.back}>
                <Icon name="chevronLeft" size={20} />
              </a>
            ) : (
              <a href="#/" className="brand brand-phone" aria-label={`${copy.appName}, ${copy.nav.home}`}>
                <Mark size={26} />
              </a>
            ))}
            <div className="topbar-lead" ref={setLead}>
              {!detail && <span className="topbar-title">{pageTitle}</span>}
            </div>
            <div className="topbar-right">
              <button
                type="button"
                className={isPhone ? 'icon-btn' : 'ask-trigger'}
                onClick={() => openPalette()}
                aria-label={isPhone ? copy.nav.askButton : undefined}
                aria-keyshortcuts={isMac() ? 'Meta+K' : 'Control+K'}
              >
                <Icon name={isPhone ? 'search' : 'sparkle'} size={isPhone ? 20 : 16} />
                {!isPhone && (
                  <>
                    <span>{copy.nav.askButton}</span>
                    <kbd className="kbd">{shortcut}</kbd>
                  </>
                )}
              </button>
              <div className="topbar-actions" ref={setActions} />
              {isPhone && <UserMenu variant="compact" onSignOut={onSignOut} />}
            </div>
          </header>
          <main id="main" tabIndex={-1} className={`main main-${route.name}`}>
            {children}
          </main>
        </div>

        {isPhone && (
          <nav aria-label={copy.nav.label} className="tabbar">
            {tabNav}
          </nav>
        )}
      </div>
    </SlotContext.Provider>
  );
}
