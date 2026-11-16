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
| `VITE_API=mock` or `real` | Which client to use. Default: mock in `dev`, real in a build. |
| `VITE_MOCK_BRIEF=off` | The mock answers `501 not_built` for the brief, so the home page hides it. |
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
```

## Layout

- `src/copy.ts`: every user-facing word, in one file. Edit the wording there.
- `src/api/`: `types.ts` (the contract), `real.ts` (fetch, CSRF, typed errors), `mock.ts`.
- `src/pages/`: sign-in, home, draft workspace.
- `src/components/`: task progress, hints, user menu.

## Notes

- ESLint's TypeScript plugin does not support TypeScript 7 yet, so `typescript` here is the
  `@typescript/typescript6` API package (used only by ESLint) and `tsc` comes from `typescript-7`.
  Drop the alias when typescript-eslint supports TS 7.
- Markdown from drafts and task results is sanitised with DOMPurify before it is rendered.
