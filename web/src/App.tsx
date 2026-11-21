import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { Api, Me } from './api';
import { AppContext, useApp } from './app-context';
import { canAsk } from './bots';
import { ErrorBoundary } from './components/ErrorBoundary';
import { Palette } from './components/Palette';
import { Shell } from './components/Shell';
import { ToastProvider } from './components/Toasts';
import { DataProvider, useData } from './data';
import { Activity } from './pages/Activity';
import { AdminLoading } from './components/AdminLoading';
import { Skeleton } from './components/bits';
import { copy } from './copy';
import { DraftPage } from './pages/DraftPage';
import { Drafts } from './pages/Drafts';
import { FollowUps } from './pages/FollowUps';
import { Home } from './pages/Home';
import { SignedOut } from './pages/SignedOut';
import { SignIn } from './pages/SignIn';
import { Thread } from './pages/Thread';
import { useTheme } from './theme';
import { withCleanTasks } from './briefBlock';
import { withCleanDrafts } from './citations';
import { isSetupPath, parseRoute, signedOutReason } from './url';
import type { Route } from './url';

// The whole Admin area (its pages, editors and diff code) is its own chunk: it is fetched the first
// time someone opens #/admin, so a PM who never does never downloads it.
const AdminPage = lazy(() => import('./pages/admin/AdminPage').then((m) => ({ default: m.AdminPage })));
// Reviews are for reviewers only, so they are a chunk of their own too.
const ReviewsPage = lazy(() => import('./pages/reviews/ReviewsPage').then((m) => ({ default: m.ReviewsPage })));
// Password setup (with its QR code drawing) and the security page are used rarely: chunks of their own.
const SetupPage = lazy(() => import('./pages/password/SetupPage').then((m) => ({ default: m.SetupPage })));
const SecurityPage = lazy(() => import('./pages/password/SecurityPage').then((m) => ({ default: m.SecurityPage })));
// The Bots page and the one-time "Pick your team" step are visited rarely: chunks of their own.
const BotsPage = lazy(() => import('./pages/Bots').then((m) => ({ default: m.Bots })));
const PickTeam = lazy(() => import('./pages/PickTeam').then((m) => ({ default: m.PickTeam })));

/** Shown while a small page's code is on its way. */
function PageLoading({ label }: { label: string }) {
  return (
    <div className="page">
      <Skeleton rows={3} label={label} />
    </div>
  );
}

/** Shown while the Reviews code is on its way. */
function ReviewsLoading() {
  return (
    <div className="page">
      <Skeleton rows={4} label={copy.reviews.loading} />
    </div>
  );
}

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
  const { me } = useApp();
  switch (route.name) {
    // Only someone who may ask has a team; for anyone else these are the home page.
    case 'bots':
      return canAsk(me) ? (
        <Suspense fallback={<PageLoading label={copy.bots.page.loading} />}>
          <BotsPage />
        </Suspense>
      ) : (
        <Home />
      );
    case 'pickTeam':
      return canAsk(me) ? (
        <Suspense fallback={<PageLoading label={copy.bots.pick.loading} />}>
          <PickTeam />
        </Suspense>
      ) : (
        <Home />
      );
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
    case 'reviews':
    case 'review':
      return (
        <Suspense fallback={<ReviewsLoading />}>
          <ReviewsPage id={route.name === 'review' ? route.id : undefined} />
        </Suspense>
      );
    case 'security':
      return (
        <Suspense fallback={<PageLoading label={copy.password.security.loading} />}>
          <SecurityPage />
        </Suspense>
      );
    case 'admin':
      return (
        <Suspense fallback={<AdminLoading />}>
          <AdminPage section={route.section} search={route.search} sub={route.sub} />
        </Suspense>
      );
    default:
      return <Home />;
  }
}

export function App({ createApi, pollMs }: AppProps) {
  const [me, setMe] = useState<Me | null>(null);
  const [ready, setReady] = useState(false);
  const signedOut = useRef<() => void>(() => {});
  const [api] = useState(() => withCleanDrafts(withCleanTasks(createApi(() => signedOut.current()))));
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
  // Setup with a setup or reset code works whether or not someone is signed in in this browser.
  if (isSetupPath(path)) {
    return (
      <Suspense fallback={null}>
        <SetupPage
          api={api}
          onDone={async () => {
            await refreshMe();
            navigate('/');
          }}
        />
      </Suspense>
    );
  }
  if (!ctx) {
    const reason = signedOutReason(path);
    if (reason !== null) return <SignedOut reason={reason} onTryAgain={() => navigate('/')} />;
    return <SignIn api={api} path={path} onSignedIn={() => void refreshMe()} />;
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
