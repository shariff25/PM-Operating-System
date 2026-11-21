import type { BotCard, Bots, Me } from './api';
import { ApiError, isServerErrorCode } from './api/types';
import { copy } from './copy';
import { errorText } from './errors';

/**
 * The bot marketplace in the app (docs/web-api.md, "Bots"; docs/bot-marketplace-spec.md §8).
 * Every string a bot card carries is content: it is rendered as plain React text, never as HTML
 * or Markdown, and a mascot is chosen from the app's own fixed set.
 */

/** Whether the person may ask, and so has a team: the `ask-bot` permission. */
export function canAsk(me: Pick<Me, 'permissions'>): boolean {
  return (me.permissions ?? []).includes('ask-bot');
}

/** The team as `Me` says it, with an older server's missing fields read as the contract says. */
export function teamOf(me: Me): string[] {
  if (!canAsk(me)) return [];
  return me.team ?? ['nudge'];
}

/** The picker's default: `ask_persona` while it is on the team, else Nudge. */
export function defaultPersona(me: Me): string {
  const team = teamOf(me);
  const want = me.ask_persona ?? 'nudge';
  return team.includes(want) ? want : 'nudge';
}

/** Who a bot is, for attribution and the picker. */
export interface BotIdentity {
  /** `nudge`; `bot`, an offered bot from the catalog; `gone`, a bot no longer offered; `unchecked`, the catalog can't be read. */
  kind: 'nudge' | 'bot' | 'gone' | 'unchecked';
  name: string;
  /** Which drawing: an unknown value draws the neutral face. */
  mascot: string;
  /**
   * Shown with the Preview tag ("not yet benchmarked"): an offered bot whose card says so. Never
   * Nudge, nor a bot no longer offered; a bot the app can't check (`unchecked`) is shown as a
   * Preview, since nothing says it was benchmarked.
   */
  preview: boolean;
}

const NUDGE: BotIdentity = { kind: 'nudge', name: copy.bots.nudgeName, mascot: 'nudge', preview: false };

/**
 * Who drafted a draft or ran a task: `persona` null is Nudge; a key the server says is offered is
 * looked up in the catalog; a withdrawn key, or one the catalog doesn't list, is "A bot no longer
 * offered" with the neutral face. Never a name or a mascot guessed from the key. While the catalog
 * can't be read (or the person may not read it), an offered key is named plainly as "Another bot".
 */
export function identify(persona: string | null | undefined, offered: boolean | undefined, catalog: readonly BotCard[] | null | undefined): BotIdentity {
  if (persona === null || persona === undefined || persona === 'nudge') return NUDGE;
  if (offered === false) return { kind: 'gone', name: copy.bots.gone, mascot: 'unknown', preview: false };
  if (!catalog) return { kind: 'unchecked', name: copy.bots.unchecked, mascot: 'unknown', preview: true };
  const card = catalog.find((b) => b.key === persona);
  if (!card) return { kind: 'gone', name: copy.bots.gone, mascot: 'unknown', preview: false };
  return { kind: 'bot', name: card.display_name, mascot: card.mascot, preview: card.preview === true };
}

/** The card for a key in the catalog, if it is offered. */
export function cardOf(bots: Bots | null, key: string): BotCard | null {
  return bots?.bots.find((b) => b.key === key) ?? null;
}

/** Whether a routine is the daily brief: its prompt starts with `[daily-brief]`, as pmos-web finds it. */
export function isBriefRoutine(r: { prompt: string | null }): boolean {
  return (r.prompt ?? '').trimStart().startsWith('[daily-brief]');
}

/** Whether a bot may be chosen to write the daily brief (slice R2): not Nudge, and it lists the skill. */
export function writesBriefs(bot: Pick<BotCard, 'always_on' | 'can_do'>): boolean {
  return !bot.always_on && bot.can_do.includes('daily-brief');
}

/** A skill key's label, from the app's own fixed map; a key it has no label for is shown as the key. */
export function skillLabel(key: string): string {
  const labels: Record<string, string> = copy.bots.skills;
  return labels[key] ?? key;
}

/** A system key's label ("jira" reads "Jira"); an unknown one is shown as the key. */
export function readsLabel(key: string): string {
  const labels: Record<string, string> = copy.bots.reads;
  return labels[key] ?? key;
}

/** The role line's text colour: the darker tone of the bot's colour, or plain muted for an unknown mascot. */
export function roleTone(mascot: string): string {
  return ['nudge', 'chief', 'sprout', 'echo'].includes(mascot) ? `bot-role-${mascot}` : 'bot-role-plain';
}

/**
 * The plain sentence for a failed team change or read. The codes whose general wording is about
 * drafts or proposals (`rate_limited`, `unknown_outcome`, `upstream_unavailable`,
 * `assertion_required`, `unavailable`) say what they mean for a team here; the rest are the app's
 * general sentences. `unknown_outcome` never says nothing changed.
 */
export function teamErrorText(e: unknown, fallback: string = copy.bots.errors.fallback): string {
  if (e instanceof ApiError && isServerErrorCode(e.code)) {
    const own: Partial<Record<string, string>> = copy.bots.errors.codes;
    const line = own[e.code];
    if (line) return line;
  }
  return errorText(e, fallback);
}

/** A team change's answer may have been lost: the page must be read again. */
export function isUnknownOutcome(e: unknown): boolean {
  return e instanceof ApiError && (e.code === 'unknown_outcome' || e.code === 'network' || e.status === 408 || (e.status >= 500 && !isServerErrorCode(e.code)));
}
