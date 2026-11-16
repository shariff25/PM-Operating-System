import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import type { Api, AuthProvider } from '../api';
import { messageOf } from '../app-context';
import { ErrorNote } from '../components/bits';
import { Icon, Mark } from '../components/icons';
import { copy } from '../copy';
import { loginHref } from '../url';

interface ProviderProps {
  api: Api;
  onSignedIn: () => void;
}

/** Development sign-in: a handle from team.yaml. Needs PMOS_WEB_DEV_LOGIN=1 on the server. */
function DevLogin({ api, onSignedIn, secondary }: ProviderProps & { secondary: boolean }) {
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
    <form onSubmit={submit} className={`signin-form ${secondary ? 'signin-dev' : ''}`}>
      {secondary && <p className="signin-dev-title">{copy.signIn.devHeading}</p>}
      <label htmlFor="handle">{copy.signIn.handleLabel}</label>
      <p id="handle-hint" className="muted">{secondary ? copy.signIn.devNote : copy.signIn.handleHint}</p>
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
      <button type="submit" className={`btn ${secondary ? '' : 'btn-primary'}`} disabled={busy || !handle.trim()}>
        {busy ? copy.signIn.working : copy.signIn.submit}
      </button>
    </form>
  );
}

/** Microsoft sign-in is a full-page trip, so it is a link and not a fetch. */
function MicrosoftLogin({ path }: { path: string }) {
  return (
    <a className="btn btn-primary signin-ms" href={loginHref(path)}>
      {copy.signIn.microsoft}
    </a>
  );
}

type Providers = { state: 'loading' } | { state: 'ready'; list: AuthProvider[] };

/** What the server offers. A failed call is the same as nothing offered. */
function useProviders(api: Api): Providers {
  const [providers, setProviders] = useState<Providers>({ state: 'loading' });
  useEffect(() => {
    let live = true;
    api.providers().then(
      (list) => live && setProviders({ state: 'ready', list }),
      () => live && setProviders({ state: 'ready', list: [] }),
    );
    return () => {
      live = false;
    };
  }, [api]);
  return providers;
}

/** `path` is the page the PM was trying to reach, so Microsoft can send them back to it. */
export function SignIn(props: ProviderProps & { path?: string }) {
  const providers = useProviders(props.api);
  const entra = providers.state === 'ready' && providers.list.includes('entra');
  const dev = providers.state === 'ready' && providers.list.includes('dev');
  return (
    <div className="signin-wrap">
      <main className="signin">
        <Mark size={36} />
        <h1>{copy.signIn.title}</h1>
        {providers.state === 'loading' && <p className="muted" role="status">{copy.signIn.loading}</p>}
        {providers.state === 'ready' && !entra && !dev && (
          <p className="signin-none">
            <Icon name="alert" size={16} />
            <span>{copy.signIn.notSetUp}</span>
          </p>
        )}
        {entra && <MicrosoftLogin path={props.path ?? '/'} />}
        {dev && <DevLogin api={props.api} onSignedIn={props.onSignedIn} secondary={entra} />}
      </main>
    </div>
  );
}
