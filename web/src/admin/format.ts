const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const two = (n: number) => String(n).padStart(2, '0');

/** "16 Nov 09:05:12", in UTC, which is how the log is kept. An unreadable value is returned as it came. */
export function auditTime(at: string): string {
  const t = new Date(at);
  if (Number.isNaN(t.getTime())) return at;
  return `${t.getUTCDate()} ${MONTHS[t.getUTCMonth()]} ${two(t.getUTCHours())}:${two(t.getUTCMinutes())}:${two(t.getUTCSeconds())}`;
}

/** "9 Nov", in UTC. */
export function shortDay(at: string): string {
  const t = new Date(at);
  if (Number.isNaN(t.getTime())) return at;
  return `${t.getUTCDate()} ${MONTHS[t.getUTCMonth()]}`;
}

/** "Connectors" for "connectors", "Docs" for "docs". */
export const titleCase = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
