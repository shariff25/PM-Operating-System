/** Only http and https addresses are ever turned into links. Anything else is shown as text. */
export function safeUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const u = new URL(value.trim());
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.href : null;
  } catch {
    return null;
  }
}

/** A draft or task id is safe to put in a path or a hash only when it is a plain token. */
export function isSafeId(id: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(id);
}

/** Read the id out of a `/d/<id>` hash path. Returns null when it is malformed or unsafe. */
export function draftIdFromPath(path: string): string | null {
  const raw = /^\/d\/([^/]+)$/.exec(path)?.[1];
  if (!raw) return null;
  try {
    const id = decodeURIComponent(raw);
    return isSafeId(id) ? id : null;
  } catch {
    return null;
  }
}
