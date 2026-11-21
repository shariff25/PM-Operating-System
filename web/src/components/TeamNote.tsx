import { useState } from 'react';
import { messageOf, useApp } from '../app-context';
import { copy } from '../copy';
import { useData } from '../data';
import { Mascot } from './Mascot';
import { useShowNudge } from './Nudge';

const N = copy.bots.note;

/**
 * The one note for people who were here before the bot marketplace (`show_team_note`): what is
 * new, "Pick your team" to the Bots page, and a dismiss that marks `team_announced` so it never
 * comes back. Never shown with the "Pick your team" step, and never to someone who already added
 * a bot.
 */
export function TeamNote() {
  const { api, setMe } = useApp();
  const { bots } = useData();
  const showArt = useShowNudge();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const offered = (bots?.bots ?? []).filter((b) => !b.always_on);

  const dismiss = async () => {
    setBusy(true);
    setError(null);
    try {
      setMe(await api.markHintSeen('team_announced'));
    } catch (e) {
      setError(messageOf(e, copy.errors.settingFailed));
      setBusy(false);
    }
  };

  return (
    <section className="card team-note" aria-labelledby="team-note-title">
      {showArt && offered.length > 0 && (
        <div className="team-note-faces" aria-hidden="true">
          {offered.slice(0, 3).map((b) => (
            <Mascot key={b.key} bot={b.mascot} size={30} />
          ))}
        </div>
      )}
      <div className="team-note-text">
        <h2 id="team-note-title">{N.title}</h2>
        <p>{N.text(offered.map((b) => b.display_name))}</p>
        {error && <p className="coach-error" role="alert">{error}</p>}
      </div>
      <div className="team-note-actions">
        <a className="btn btn-sm btn-primary" href="#/bots">
          {N.action}
        </a>
        <button type="button" className="btn btn-sm btn-ghost" onClick={() => void dismiss()} disabled={busy}>
          {N.dismiss}
        </button>
      </div>
    </section>
  );
}
