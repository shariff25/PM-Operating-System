import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { Api, Me } from './api';
import { AppContext } from './app-context';
import { ErrorBoundary } from './components/ErrorBoundary';
import { Palette } from './components/Palette';
import { Shell } from './components/Shell';
import { ToastProvider } from './components/Toasts';
import { DataProvider, useData } from './data';
import { Activity } from './pages/Activity';
import { DraftPage } from './pages/DraftPage';
import { Drafts } from './pages/Drafts';
import { FollowUps } from './pages/FollowUps';
import { Home } from './pages/Home';
import { SignIn } from './pages/SignIn';
import { Thread } from './pages/Thread';
import { useTheme } from './theme';
import { parseRoute } from './url';
import type { Route } from './url';

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

/** The page for a route. Follow-ups exist only when the brief is built; otherwise it is home. */
function Page({ route }: { route: Route }) {
  const { briefState } = useData();
  switch (route.name) {
    case 'drafts':
      return <Drafts />;
    case 'draft':
      return <DraftPage key={route.id} id={route.id} />;
    case 'followups':
      return briefState === 'off' ? <Home /> : <FollowUps />;
    case 'activity':
      return <Activity />;
    case 'task':
      return <Thread key={route.id} id={route.id} />;
    default:
      return <Home />;
  }
}

export function App({ createApi, pollMs }: AppProps) {
  const [me, setMe] = useState<Me | null>(null);
  const [ready, setReady] = useState(false);
  const signedOut = useRef<() => void>(() => {});
  const [api] = useState(() => createApi(() => signedOut.current()));
  const [path, navigate] = useHashPath();
  const [theme, setTheme] = useTheme();
  const [palette, setPalette] = useState<{ text: string } | null>(null);

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

  const openPalette = useCallback((text = '') => setPalette({ text }), []);
  const closePalette = useCallback(() => setPalette(null), []);

  // Ctrl or Cmd + K opens the palette from anywhere, and closes it again.
  const signedIn = me !== null;
  useEffect(() => {
    if (!signedIn) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setPalette((p) => (p ? null : { text: '' }));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [signedIn]);

  // A route change closes the palette.
  useEffect(() => setPalette(null), [path]);

  const ctx = useMemo(
    () => (me ? { api, me, setMe, refreshMe, pollMs, navigate, theme, setTheme, openPalette } : null),
    // navigate is stable enough: it only writes to location.hash
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [api, me, refreshMe, pollMs, theme, setTheme, openPalette],
  );

  if (!ready) return null;
  if (!ctx) {
    return <SignIn api={api} onSignedIn={() => void refreshMe()} />;
  }

  // A malformed or unsafe id, or an unknown path, shows the home page rather than crashing.
  const route = parseRoute(path);

  return (
    <AppContext.Provider value={ctx}>
      <ToastProvider>
        <DataProvider>
          <Shell route={route} onSignOut={() => setMe(null)}>
            <ErrorBoundary resetKey={path}>
              <Page route={route} />
            </ErrorBoundary>
          </Shell>
          {palette && <Palette initial={palette.text} onClose={closePalette} />}
        </DataProvider>
      </ToastProvider>
    </AppContext.Provider>
  );
}
