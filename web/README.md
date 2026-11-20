# PM OS web app

A Vite, React and TypeScript single-page app. It talks only to `pmos-web` through the contract in
`docs/web-api.md`. The product plan is `docs/web-client.md`.

## Run it with the mock (no backend needed)

```sh
pnpm install            # from the repo root
pnpm -C web dev         # http://localhost:5173
```

`dev` uses the mock client by default. Sign in with any handle except `nobody` (which shows the
"not a member" error). The mock has a pending Confluence PRD draft with five sources, a Jira draft,
an external Slack draft (asks for the extra confirmation), an expired Slack draft (approving gives
`409 expired`), a decided draft, tasks, a brief with follow-ups, and a practice draft. State lives in
memory, so a reload resets it.

Mock switches, set in `web/.env.local` or the shell:

| Variable | Effect |
| --- | --- |
| `VITE_API=real` | Use the real client in `dev`. Default in `dev`: the sample data. A build always uses the real client, and `pnpm build` fails if `VITE_API=mock` is set. |
| `VITE_MOCK_BRIEF=off` | The mock answers `501 not_built` for the brief, so the home page hides it. |
| `VITE_MOCK_HOSTILE=on` | Adds a draft ("Vendor call notes") that tries a style block, hidden text, a form and a remote image. |
| `VITE_MOCK_EMPTY=on` | A brand-new account: no drafts, no requests, an empty brief (for the empty states). |
| `VITE_MOCK_FAST=on` | No pretend network delay (the browser tests use it). |
| `VITE_AUTH_PROVIDER=dev` or `entra` | Sign-in provider. The switch is in one place, `src/pages/SignIn.tsx`. |

Words to try in the ask box: anything for a plain answer; "busy" for a longer run with a progress bar and the retry line; "draft a PRD ..." for a draft; "this will
fail" for a failed task (Try again then works); "busy" for the model-busy line. In the change box,
"shorter" removes a paragraph and "make it fail" shows a failed suggestion.

## Point it at a real backend

```sh
PMOS_WEB_URL=http://127.0.0.1:8787 VITE_API=real pnpm -C web dev
```

The dev server proxies `/api` to `PMOS_WEB_URL` (default `http://127.0.0.1:8787`), so the
`__Host-pmos` cookie stays same-origin. `pmos-web` must run with `PMOS_WEB_DEV_LOGIN=1` until Entra
sign-in exists. A production build uses the real client and expects `/api` on the same origin.

## Scripts

```sh
pnpm -C web typecheck   # tsc --noEmit (TypeScript 7)
pnpm -C web lint        # ESLint
pnpm -C web test        # Vitest and Testing Library
pnpm -C web build       # typecheck, then vite build into web/dist
pnpm -C web e2e         # browser tests (Playwright) against the sample data
```

`pnpm e2e` starts its own dev servers on ports 5191, 5192 and 5193 (the second is the empty account, the third has the brief switched off). It also runs axe (`@axe-core/playwright`, every rule on) on every page in both themes (`e2e/a11y.spec.ts`). It uses the browser at `/opt/pw-browsers/chromium`
when that exists, or the one in `PW_CHROMIUM`; otherwise run `pnpm exec playwright install chromium` once.

## Layout

- `src/copy.ts`: every user-facing word, in one file. Edit the wording there.
- `src/api/`: `types.ts` (the contract), `real.ts` (fetch, CSRF, typed errors), `mock.ts`.
- `src/pages/`: sign-in, home (launchpad), drafts, draft workspace, follow-ups, activity, thread.
- `src/components/`: the app shell (the dark rail, icons only on the draft workspace; on a phone a top bar and a bottom tab bar), command palette, user menu,
  toasts, coach-mark hints, approve dialog, icons (drawn here and inlined).
- `src/data.tsx`: the drafts, brief and task lists the navigation counts and pages share.
- Nudge, the mascot (`docs/design/nudge`): `src/components/Nudge.tsx` draws it from the approved
  art; `src/onboarding.ts` is the guided start on Today (`src/components/Welcome.tsx`), kept in
  the server's `hints_seen`. It lives only in the welcome card, beside the one-time hints, in
  calm empty states, in the one "Approved!" moment and on the sign-in screen; "Show Nudge" in the
  user menu hides it everywhere. On the dev server `?mock_new=1` signs in as a first visit, and
  the sample data keeps each person's settings in sessionStorage so a reload keeps them.
- Routes are hashes, checked in `src/url.ts`: `#/`, `#/drafts`, `#/d/<id>`, `#/followups`,
  `#/activity`, `#/a/<task id>`. Anything else is the home page.
- A running request is polled every 600 ms for six seconds, then every 2 s, then every 5 s after a minute (`src/polling.ts`); polling pauses in a hidden tab and resumes on focus.
- Ctrl or Cmd + K opens the palette. The theme (system, light, dark) is in the user menu and is
  kept in `localStorage`; `public/favicon.svg` is the icon (the content security policy blocks
  `data:` icons).

## Notes

- ESLint's TypeScript plugin does not support TypeScript 7 yet, so `typescript` here is the
  `@typescript/typescript6` API package (used only by ESLint) and `tsc` comes from `typescript-7`.
  Drop the alias when typescript-eslint supports TS 7.
- Draft text, titles, sources and task results come from a bot that reads pages other people can
  write to, so they are untrusted. `src/markdown.ts` keeps plain formatting only (headings, lists,
  emphasis, code, quotes, tables, links, rules). Raw HTML, styles, forms and images are not shown;
  images appear as their alt text and address. Links are `http` and `https` only, open in a new tab
  and show their real host. Whatever was left out is counted and shown on request ("This draft
  contains N hidden or stripped parts"), and the approve dialog repeats the warning and lists
  every link destination.
- Approve sends the draft `version` it showed, so the server can refuse a draft that changed
  (`409 stale`). Approving is blocked while there are unsaved edits or an open suggestion.
- The sample-data client is imported only when `import.meta.env.DEV` is true, so it is not in a
  production bundle (`src/build.test.ts` checks `dist`). The fonts, Geist and Geist Mono, come from
  the `@fontsource-variable/*` packages and are bundled as files on the app's own origin (never
  inlined as `data:`), so the app makes no third-party requests.
- `src/styles.css` holds the design tokens (light first, then the two identical dark blocks) and
  every component style; `src/contrast.test.ts` checks text pairs against 4.5:1 in both themes.
