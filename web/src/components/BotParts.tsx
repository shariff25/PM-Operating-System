import { useId } from 'react';
import type { BotCard, BotNeed } from '../api';
import { identify, readsLabel, roleTone } from '../bots';
import type { BotIdentity } from '../bots';
import { copy } from '../copy';
import { useData } from '../data';
import { systemTone } from './bits';
import { Mascot, isMascotBot } from './Mascot';
import { useShowNudge } from './Nudge';

/**
 * Small parts of the bot marketplace shared by Pick your team, the Bots page, the picker and
 * attribution. Every string from a bot card is rendered as a React text node.
 */

/**
 * A control's words: `shown` on screen, `full` (which contains them, and names the bot) for screen
 * readers, as one text so no assistive technology runs the words together.
 */
export function Words({ shown, full }: { shown: string; full: string }) {
  return (
    <>
      <span className="sr-only">{full}</span>
      <span aria-hidden="true">{shown}</span>
    </>
  );
}

/**
 * A bot's face. With "Show mascots" off it is a small dot in the bot's colour instead (a neutral
 * dot for an unknown one); either way it is decorative and the name beside it carries the meaning.
 */
export function BotFace({ mascot, size = 20 }: { mascot: string; size?: number }) {
  const show = useShowNudge();
  if (!show) return <span className={`bot-dot bot-dot-${isMascotBot(mascot) ? mascot : 'unknown'}`} aria-hidden="true" />;
  return <Mascot bot={mascot} size={size} />;
}

/**
 * Which bot drafted a draft or ran a task: its face and name. `persona` null is Nudge; a bot no
 * longer offered, or one the catalog doesn't list, reads "A bot no longer offered" with the
 * neutral face. Screen readers hear "By Chief".
 */
export function BotBy({ persona, offered, size = 20, className }: { persona: string | null | undefined; offered: boolean | undefined; size?: number; className?: string }) {
  const { bots } = useData();
  const who = identify(persona, offered, bots?.bots);
  return <BotByIdentity who={who} size={size} className={className} />;
}

export function BotByIdentity({ who, size = 20, className }: { who: BotIdentity; size?: number; className?: string }) {
  return (
    <span className={`bot-by bot-by-${who.kind} ${className ?? ''}`.trim()} data-bot={who.kind === 'bot' || who.kind === 'nudge' ? who.mascot : who.kind}>
      <BotFace mascot={who.mascot} size={size} />
      <span className="bot-by-name" aria-hidden="true">{who.name}</span>
      <span className="sr-only">{copy.bots.byLabel(who.name)}</span>
    </span>
  );
}

/** "Drafted by Chief", under a draft's title (the owner's and the reviewer's view). */
export function DraftedBy({ persona, offered }: { persona: string | null | undefined; offered: boolean | undefined }) {
  const { bots } = useData();
  const who = identify(persona, offered, bots?.bots);
  return (
    <p className={`drafted-by bot-by-${who.kind}`} data-bot={who.kind === 'bot' || who.kind === 'nudge' ? who.mascot : who.kind}>
      <BotFace mascot={who.mascot} size={20} />
      <span>{copy.bots.draftedBy(who.name)}</span>
    </p>
  );
}

/** The role line ("CHIEF OF STAFF") in the darker tone of the bot's colour. */
export function RoleLine({ bot, className }: { bot: Pick<BotCard, 'title' | 'mascot'>; className?: string }) {
  return <p className={`bot-role ${roleTone(bot.mascot)} ${className ?? ''}`.trim()}>{bot.title}</p>;
}

/** The quiet "Preview · Not yet benchmarked" tag (the owner's decision of 2026-11-21). */
export function PreviewTag({ short }: { short?: boolean }) {
  return (
    <span className="tag tag-preview" title={copy.bots.previewNote}>
      {copy.bots.preview}
      {short ? <span className="sr-only">: {copy.bots.previewNote}</span> : <span className="tag-preview-note">· {copy.bots.previewNote}</span>}
    </span>
  );
}

/** "Needs an analytics source", with what the bot does without it as its description. */
export function NeedBadges({ needs }: { needs: readonly BotNeed[] }) {
  const base = useId();
  const unmet = needs.filter((n) => !n.met);
  if (unmet.length === 0) return null;
  return (
    <>
      {unmet.map((n, i) => (
        <span key={`${n.source}-${i}`} className="need">
          <span className="tag tag-warn need-badge" title={n.without} aria-describedby={`${base}-${i}`}>
            {copy.bots.needs(n.label)}
          </span>
          <span id={`${base}-${i}`} hidden>{n.without}</span>
        </span>
      ))}
    </>
  );
}

/** "Reads Jira Confluence Slack" as quiet pills. */
export function ReadsPills({ reads, label }: { reads: readonly string[]; label: string }) {
  if (reads.length === 0) return null;
  return (
    <ul className="bot-reads" aria-label={label}>
      {reads.map((r) => (
        <li key={r} className={`sys-pill sys-pill-${systemTone(r)}`}>{readsLabel(r)}</li>
      ))}
    </ul>
  );
}
