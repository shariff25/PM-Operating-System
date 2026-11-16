import { useState } from 'react';
import type { FormEvent } from 'react';
import type { Api } from '../api';
import { messageOf } from '../app-context';
import { ErrorNote } from '../components/bits';
import { copy } from '../copy';

interface ProviderProps {
  api: Api;
  onSignedIn: () => void;
}

/** Development sign-in: a handle from team.yaml. Needs PMOS_WEB_DEV_LOGIN=1 on the server. */
function DevLogin({ api, onSignedIn }: ProviderProps) {
  const [handle, setHandle] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.devLogin(handle.trim());
      onSignedIn();
    } catch (err) {
      setError(messageOf(err, copy.errors.unknown));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="stack">
      <label htmlFor="handle">{copy.signIn.handleLabel}</label>
      <p id="handle-hint" className="muted">{copy.signIn.handleHint}</p>
      <input
        id="handle"
        value={handle}
        onChange={(e) => setHandle(e.target.value)}
        aria-describedby="handle-hint"
        autoComplete="username"
        autoCapitalize="none"
        spellCheck={false}
      />
      {error && <ErrorNote next={copy.signIn.retry}>{error}</ErrorNote>}
      <button type="submit" className="btn btn-primary" disabled={busy || !handle.trim()}>
        {busy ? copy.signIn.working : copy.signIn.submit}
      </button>
    </form>
  );
}

/**
 * Entra ID sign-in, not built yet. The redirect path below is a placeholder until the
 * contract names the route.
 */
function MicrosoftLogin() {
  return (
    <button type="button" className="btn btn-primary" onClick={() => window.location.assign('/api/auth/entra/start')}>
      {copy.signIn.microsoft}
    </button>
  );
}

// The one place that picks the sign-in provider. To switch to Entra, set VITE_AUTH_PROVIDER=entra
// (or change the default here).
const PROVIDERS = { dev: DevLogin, entra: MicrosoftLogin } as const;
const provider: keyof typeof PROVIDERS = import.meta.env.VITE_AUTH_PROVIDER === 'entra' ? 'entra' : 'dev';

export function SignIn(props: ProviderProps) {
  const Provider = PROVIDERS[provider];
  return (
    <main className="signin card stack">
      <h1>{copy.signIn.title}</h1>
      <Provider {...props} />
    </main>
  );
}
