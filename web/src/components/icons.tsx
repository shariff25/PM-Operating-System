// A small set of line icons, drawn for this app on a 24px grid and inlined (no icon font, no
// remote file). Every icon is decorative: the control that holds it carries the words.

const PATHS = {
  home: 'M4 10.5 12 4l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5.5h-5V20H5a1 1 0 0 1-1-1z',
  drafts: 'M14 3.5H7a1.5 1.5 0 0 0-1.5 1.5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8zM14 3.5V8h4.5M9 12.5h6M9 16h4',
  followups: 'M4 7.5h11M11 3.5l4 4-4 4M20 16.5H9M13 12.5l-4 4 4 4',
  activity: 'M3.5 12h4l2.5-6.5 4 13 2.5-6.5h4',
  search: 'M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13zM15.2 15.2 20 20',
  sparkle: 'M12 3.5c.6 3.9 1.9 5.5 5.5 6.5-3.6 1-4.9 2.6-5.5 6.5-.6-3.9-1.9-5.5-5.5-6.5 3.6-1 4.9-2.6 5.5-6.5zM18.5 15.5c.3 1.6.9 2.2 2.5 2.5-1.6.3-2.2.9-2.5 2.5-.3-1.6-.9-2.2-2.5-2.5 1.6-.3 2.2-.9 2.5-2.5z',
  sidebar: 'M5 4.5h14a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5zM9.5 4.5v15',
  chevronRight: 'M9.5 6l6 6-6 6',
  chevronLeft: 'M14.5 6l-6 6 6 6',
  chevronDown: 'M6 9.5l6 6 6-6',
  chevronUpDown: 'M8 9.5l4-4 4 4M8 14.5l4 4 4-4',
  arrowUp: 'M12 19V5M6 11l6-6 6 6',
  arrowLeft: 'M19 12H5M11 6l-6 6 6 6',
  arrowRight: 'M5 12h14M13 6l6 6-6 6',
  shield: 'M12 3.5 19 6v5.5c0 4.4-3 7.7-7 9-4-1.3-7-4.6-7-9V6zM9 12l2 2 4-4',
  menu: 'M4 7h16M4 12h16M4 17h16',
  sun: 'M12 16.5a4.5 4.5 0 1 0 0-9 4.5 4.5 0 0 0 0 9zM12 2.5v2M12 19.5v2M4.6 4.6l1.4 1.4M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4',
  moon: 'M19.5 14.5A7.5 7.5 0 0 1 9.5 4.5a7.5 7.5 0 1 0 10 10z',
  monitor: 'M4.5 5h15a1 1 0 0 1 1 1v9.5a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM9 20h6M12 16.5V20',
  bell: 'M6 16.5V11a6 6 0 1 1 12 0v5.5l1.5 2H4.5zM10 21h4',
  lightbulb: 'M9.5 18h5M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3z',
  logout: 'M14.5 4.5H18a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5h-3.5M10 8l-4 4 4 4M6 12h9.5',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  x: 'M6 6l12 12M18 6 6 18',
  more: 'M6 12h.01M12 12h.01M18 12h.01',
  external: 'M14 4.5h5.5V10M19.5 4.5 11 13M18 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1H10',
  edit: 'M4.5 19.5h4L19 9a2.1 2.1 0 0 0-3-3L5.5 16.5zM14.5 7.5l3 3',
  clock: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM12 7.5V12l3 2',
  alert: 'M12 3.5 21 19.5H3zM12 10v4M12 17h.01',
  info: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM12 11v5M12 8h.01',
  inbox: 'M3.5 13.5 6 5.5h12l2.5 8V18a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18zM3.5 13.5h5l1 2h5l1-2h5',
  link: 'M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1',
  // Systems
  confluence: 'M6.5 3.5h7l4 4v12a1 1 0 0 1-1 1h-10a1 1 0 0 1-1-1v-15a1 1 0 0 1 1-1zM13.5 3.5v4h4M8.5 12h7M8.5 15h7M8.5 18h4',
  jira: 'M12 3 21 12l-9 9-9-9zM12 8.5 15.5 12 12 15.5 8.5 12z',
  slack: 'M9.5 3.5v17M14.5 3.5v17M3.5 9.5h17M3.5 14.5h17',
  message: 'M5 5h14a1 1 0 0 1 1 1v9.5a1 1 0 0 1-1 1h-7.5L7 20v-3.5H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z',
  drive: 'M8.5 4h7l5 8.5-3.5 6h-10L3.5 12.5zM8.5 4l5 8.5h7M3.5 12.5h10l-3.5 6',
  docs: 'M5.5 4.5h13v15h-13zM8.5 8.5h7M8.5 12h7M8.5 15.5h4',
  globe: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM3.5 12h17M12 3.5c2.3 2.4 3.5 5.2 3.5 8.5s-1.2 6.1-3.5 8.5c-2.3-2.4-3.5-5.2-3.5-8.5s1.2-6.1 3.5-8.5z',
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 18,
  className,
  weight = 1.75,
}: {
  name: IconName;
  size?: number;
  className?: string;
  /** Stroke width; the checklist and approve marks use a heavier line. */
  weight?: number;
}) {
  return (
    <svg
      className={`icon ${className ?? ''}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={weight}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

/** The product mark: a monogram tile in the accent colour. */
export function Mark({ size = 28 }: { size?: 24 | 28 | 36 }) {
  return (
    <span className={`mark mark-${size}`} aria-hidden="true">
      P
    </span>
  );
}

/** The icon for a system name a source or follow-up comes from. Unknown systems get a globe. */
export function systemIcon(system: string): IconName {
  const s = system.toLowerCase();
  if (s.includes('confluence') || s === 'wiki') return 'confluence';
  if (s.includes('jira')) return 'jira';
  if (s.includes('slack')) return 'slack';
  if (s.includes('drive') || s.includes('sheet')) return 'drive';
  if (s.includes('doc')) return 'docs';
  return 'globe';
}

export function kindIcon(kind: string): IconName {
  if (kind === 'confluence_page') return 'confluence';
  if (kind === 'jira_issue') return 'jira';
  return 'slack';
}
