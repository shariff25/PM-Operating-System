import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { Api, Me } from './api';
import { AppContext } from './app-context';
import { UserMenu } from './components/UserMenu';
import { copy } from './copy';
import { DraftPage } from './pages/DraftPage';
import { Home } from './pages/Home';
import { SignIn } from './pages/SignIn';

function useHashPath(): [string, (p: string) => void] {
  const read = () => window.location.hash.replace(/^#/, '') || '/';
  const [path, setPath] = useState(read);
  useEffect(() => {
    const on = () => setPath(read());
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return [path, (p) => { window.location.hash = p; }];
}

export interface AppProps {
  createApi: (onSignedOut: () => void) => Api;
  pollMs?: number;
}

export function App({ createApi, pollMs = 2000 }: AppProps) {
  const [me, setMe] = useState<Me | null>(null);
  const [ready, setReady] = useState(false);
  const signedOut = useRef<() => void>(() => {});
  const [api] = useState(() => createApi(() => signedOut.current()));
  const [path, navigate] = useHashPath();

  signedOut.current = () => setMe(null);

  const refreshMe = useCallback(async () => {
    try {
      setMe(await api.me());
    } catch {
      setMe(null);
    }
  }, [api]);

  useEffect(() => {
    void refreshMe().then(() => setReady(true));
  }, [refreshMe]);

  const ctx = useMemo(
    () => (me ? { api, me, setMe, refreshMe, pollMs, navigate } : null),
    // navigate is stable enough: it only writes to location.hash
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [api, me, refreshMe, pollMs],
  );

  if (!ready) return null;
  if (!ctx) {
    return <SignIn api={api} onSignedIn={() => void refreshMe()} />;
  }

  const draftId = /^\/d\/(.+)$/.exec(path)?.[1];

  return (
    <AppContext.Provider value={ctx}>
      <div className="shell">
        <header className="topbar">
          <a href="#/" className="brand">{copy.appName}</a>
          <UserMenu onSignOut={() => setMe(null)} />
        </header>
        <main>{draftId ? <DraftPage key={draftId} id={decodeURIComponent(draftId)} /> : <Home />}</main>
      </div>
    </AppContext.Provider>
  );
}
