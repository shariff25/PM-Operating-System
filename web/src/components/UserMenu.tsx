import { useState } from 'react';
import { useApp, messageOf } from '../app-context';
import { copy } from '../copy';

export function UserMenu({ onSignOut }: { onSignOut: () => void }) {
  const { api, me, setMe } = useApp();
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async (fn: () => Promise<void>) => {
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(messageOf(e, copy.errors.settingFailed));
    }
  };

  return (
    <div className="menu">
      <button
        type="button"
        className="btn btn-quiet"
        aria-expanded={open}
        aria-controls="user-menu"
        aria-label={copy.menu.button(me.name)}
        onClick={() => setOpen((o) => !o)}
      >
        {me.name}
      </button>
      {open && (
        <div id="user-menu" className="menu-panel" role="group" aria-label={copy.menu.button(me.name)}>
          <div className="menu-row">
            <span id="nudges-label">{copy.menu.nudges}</span>
            <button
              type="button"
              role="switch"
              aria-checked={me.nudges}
              aria-labelledby="nudges-label"
              className="switch"
              onClick={() => run(async () => setMe(await api.setNudges(!me.nudges)))}
            >
              {me.nudges ? copy.menu.nudgesOn : copy.menu.nudgesOff}
            </button>
          </div>
          <button type="button" className="btn btn-quiet menu-item" onClick={() => run(async () => setMe(await api.resetHints()))}>
            {copy.menu.showHints}
          </button>
          <button
            type="button"
            className="btn btn-quiet menu-item"
            onClick={() =>
              run(async () => {
                await api.logout();
                onSignOut();
              })
            }
          >
            {copy.menu.signOut}
          </button>
          {error && <p role="alert" className="hint-error">{error}</p>}
        </div>
      )}
    </div>
  );
}
