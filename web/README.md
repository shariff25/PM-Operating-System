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
| `VITE_MOCK_FAST=on` | No pretend network delay (the browser tests use it). |
| `VITE_AUTH_PROVIDER=dev` or `entra` | Sign-in provider. The switch is in one place, `src/pages/SignIn.tsx`. |

Words to try in the ask box: anything for a plain answer; "draft a PRD ..." for a draft; "this will
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

`pnpm e2e` starts its own dev server on port 5191. It uses the browser at `/opt/pw-browsers/chromium`
when that exists, or the one in `PW_CHROMIUM`; otherwise run `pnpm exec playwright install chromium` once.

## Layout

- `src/copy.ts`: every user-facing word, in one file. Edit the wording there.
- `src/api/`: `types.ts` (the contract), `real.ts` (fetch, CSRF, typed errors), `mock.ts`.
- `src/pages/`: sign-in, home, draft workspace.
- `src/components/`: task progress, hints, user menu.

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
  production bundle (`src/build.test.ts` checks `dist`). There are no web fonts: the app uses the
  system font stack and makes no third-party requests.
