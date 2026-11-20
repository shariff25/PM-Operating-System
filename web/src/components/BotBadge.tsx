import { Mascot } from './Mascot';

/**
 * A bot's small face and its display name, for draft rows. The name is untrusted text (it will
 * come from server data) and is rendered as a plain React text node, never as markup.
 */
export function BotBadge({ bot, name, size = 20 }: { bot: string; name: string; size?: number }) {
  return (
    <span className="bot-badge" data-bot={bot}>
      <Mascot bot={bot} size={size} />
      <span className="bot-badge-name">{name}</span>
    </span>
  );
}
