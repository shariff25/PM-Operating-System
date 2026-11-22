import { useCallback, useRef, useState } from 'react';
import type { AdminBot, AdminBots as AdminBotsData, AdminHeldDraft } from '../../api';
import { canEditContent } from '../../admin/access';
import { auditTime } from '../../admin/format';
import { useLoad } from '../../admin/useLoad';
import { messageOf, useApp } from '../../app-context';
import { BotFace, PreviewTag } from '../../components/BotParts';
import { Empty, ErrorNote, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import { useToast } from '../../components/Toasts';
import { copy } from '../../copy';
import { relTime } from '../../time';
import { listWords } from '../../admin/team';
import { AdminHead, NoAccess } from './parts';
import { StatusTile } from './proposal';
import { TeamDialog } from './teamParts';
import type { Problem } from './teamParts';

const B = copy.admin.bots;

/** Where a bot's PERSONA.md opens in the content admin. */
const contentHref = (path: string) => `#/admin/content?file=${encodeURIComponent(path)}`;

/** Offered, Preview or Withdrawn, as a quiet pill with its meaning beside it. */
function StatePill({ bot }: { bot: AdminBot }) {
  const tone = bot.state === 'withdrawn' ? '' : bot.state === 'preview' ? 'pill-accent' : 'pill-good';
  return (
    <span className="bot-state">
      <span className={`pill ${tone}`}>
        <span className="pill-dot" aria-hidden="true" />
        {B.states[bot.state] ?? bot.state}
      </span>
      <span className="bot-state-note">{bot.always_on ? copy.bots.alwaysOn : B.stateNotes[bot.state]}</span>
    </span>
  );
}

/** How many people have the bot, and who: every handle as text, nothing hidden behind a hover. */
function People({ bot }: { bot: AdminBot }) {
  if (bot.users === null) return <span className="muted">{B.alwaysOn}</span>;
  if (bot.users === 0) return <span className="muted">{B.noOne}</span>;
  return (
    <span className="bot-people">
      <span className="bot-people-count">{B.people(bot.users)}</span>
      {bot.handles && bot.handles.length > 0 && (
        <span className="bot-people-handles mono-val" aria-label={B.handlesLabel(bot.display_name)}>
          {bot.handles.join(', ')}
        </span>
      )}
    </span>
  );
}

/** Name clashes, lint and unmet needs: what an admin may want to act on. */
function Checks({ bot }: { bot: AdminBot }) {
  const items: { tone: 'warn' | 'bad' | 'info' | 'ok'; text: string; note?: string }[] = [];
  if (bot.name_clashes.length > 0) {
    items.push({ tone: 'warn', text: B.clash(bot.name_clashes.length), note: B.clashNote(listWords(bot.name_clashes)) });
  }
  if (bot.lint === null) items.push({ tone: 'info', text: B.lint.unknown });
  else if (bot.lint.errors > 0) items.push({ tone: 'bad', text: B.lint.error(bot.lint.errors) });
  else if (bot.lint.warnings > 0) items.push({ tone: 'warn', text: B.lint.warning(bot.lint.warnings) });
  for (const n of bot.needs.filter((x) => !x.met)) items.push({ tone: 'info', text: B.needsUnmet(n.label) });
  if (items.length === 0 && bot.lint !== null) items.push({ tone: 'ok', text: B.lint.ok });
  return (
    <ul className="bot-checks">
      {items.map((i) => (
        <li key={i.text} className={`bot-check bot-check-${i.tone}`}>
          <Icon name={i.tone === 'ok' ? 'check' : i.tone === 'info' ? 'info' : 'alert'} size={14} />
          <span className="bot-check-text">
            {i.text}
            {i.note && <span className="bot-check-note">{i.note}</span>}
          </span>
        </li>
      ))}
    </ul>
  );
}

function CatalogTable({ bots, canEdit }: { bots: AdminBot[]; canEdit: boolean }) {
  return (
    <div className="table-scroll">
      <table className="dtable admin-bots-table" aria-label={B.tableLabel}>
        <thead>
          <tr>
            <th scope="col" className="c-bot">{B.columns.bot}</th>
            <th scope="col" className="c-state">{B.columns.state}</th>
            <th scope="col" className="c-people">{B.columns.people}</th>
            <th scope="col" className="c-drafts">{B.columns.drafts}</th>
            <th scope="col" className="c-checks">{B.columns.checks}</th>
            <th scope="col" className="c-file">{B.columns.file}</th>
          </tr>
        </thead>
        <tbody>
          {bots.map((b) => (
            <tr key={b.key} className={`admin-bot-row admin-bot-${b.state}`}>
              <th scope="row" className="c-bot">
                <span className="admin-bot-name">
                  <span className="admin-bot-face"><BotFace mascot={b.mascot ?? 'unknown'} size={28} /></span>
                  <span className="admin-bot-text">
                    <span className="admin-bot-display">
                      {b.display_name}
                      {b.preview && <> <PreviewTag short /></>}
                    </span>
                    <span className="admin-bot-key mono-val">{b.key}</span>
                  </span>
                </span>
              </th>
              <td className="c-state"><StatePill bot={b} /></td>
              <td className="c-people"><People bot={b} /></td>
              <td className="c-drafts">
                <span className="mono-val" aria-hidden="true">{b.drafts_30d}</span>
                <span className="sr-only">{B.drafts(b.drafts_30d)}</span>
              </td>
              <td className="c-checks"><Checks bot={b} /></td>
              <td className="c-file">
                <a
                  className="btn btn-sm"
                  href={contentHref(b.path)}
                  aria-label={canEdit ? B.changeFileLabel(b.display_name) : B.openFileLabel(b.display_name)}
                >
                  <Icon name={canEdit ? 'edit' : 'file'} size={14} />
                  {canEdit ? B.changeFile : B.openFile}
                </a>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

interface HeldRow {
  draft: AdminHeldDraft;
  /** The bot's name, or the deleted key with "(deleted)". */
  bot: string;
  mascot: string;
  /** Named with the Preview tag wherever it's shown (the owner's decision of 2026-11-21). */
  preview: boolean;
}

function heldRows(data: AdminBotsData): HeldRow[] {
  const rows: HeldRow[] = [];
  for (const b of data.bots) for (const d of b.held) rows.push({ draft: d, bot: b.display_name, mascot: b.mascot ?? 'unknown', preview: b.preview });
  for (const u of data.unknown) for (const d of u.held) rows.push({ draft: d, bot: B.heldGoneBot(u.key), mascot: 'unknown', preview: false });
  return rows.sort((a, b) => (b.draft.created_at ?? '').localeCompare(a.draft.created_at ?? ''));
}

function ReleaseDialog({ row, onClose, onDone }: { row: HeldRow; onClose: () => void; onDone: () => void }) {
  const { api } = useApp();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | null>(null);
  const D = B.releaseDialog;
  const owner = row.draft.user;
  return (
    <TeamDialog
      title={D.title}
      intro={D.intro(owner, row.bot)}
      onClose={onClose}
      busy={busy}
      problem={problem}
      submitLabel={D.submit}
      busyLabel={D.busy}
      testId="release-dialog"
      onSubmit={() => {
        setBusy(true);
        setProblem(null);
        api
          .adminReleaseHeld(row.draft.id)
          .then(() => {
            toast(B.released(row.draft.id), 'good');
            onDone();
          })
          .catch((e: unknown) => {
            setBusy(false);
            setProblem({ text: messageOf(e, B.releaseFailed), tone: 'error' });
          });
      }}
    >
      <dl className="release-facts">
        <div><dt>{B.heldColumns.draft}</dt><dd className="mono-val">{row.draft.id}</dd></div>
        <div><dt>{B.heldColumns.owner}</dt><dd className="mono-val">{owner}</dd></div>
        <div><dt>{B.heldColumns.bot}</dt><dd>{row.bot}</dd></div>
      </dl>
      <h3 className="release-what">{D.what}</h3>
      <ul className="release-points">
        {D.points(owner).map((p) => <li key={p}>{p}</li>)}
      </ul>
    </TeamDialog>
  );
}

function HeldTable({ rows, canRelease, onRelease }: { rows: HeldRow[]; canRelease: boolean; onRelease: (r: HeldRow, from: HTMLButtonElement) => void }) {
  const now = Date.now();
  return (
    <div className="table-scroll">
      <table className="dtable held-table" aria-label={B.heldTableLabel}>
        <thead>
          <tr>
            <th scope="col" className="c-draft">{B.heldColumns.draft}</th>
            <th scope="col" className="c-bot">{B.heldColumns.bot}</th>
            <th scope="col" className="c-owner">{B.heldColumns.owner}</th>
            <th scope="col" className="c-when">{B.heldColumns.drafted}</th>
            {canRelease && <th scope="col" className="c-action"><span className="sr-only">{B.heldColumns.action}</span></th>}
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.draft.id} className="held-row">
              <th scope="row" className="c-draft">
                <span className="held-draft">
                  <span className="held-kind">{r.draft.kind ? copy.home.kindLabel(r.draft.kind) : B.kindUnknown}</span>
                  <span className="mono-val held-id">{r.draft.id}</span>
                </span>
              </th>
              <td className="c-bot">
                <span className="bot-by">
                  <BotFace mascot={r.mascot} size={20} />
                  <span>{r.bot}</span>
                  {r.preview && <PreviewTag short />}
                </span>
              </td>
              <td className="c-owner"><span className="mono-val">{r.draft.user}</span></td>
              <td className="c-when">
                {r.draft.created_at ? (
                  <time dateTime={r.draft.created_at} title={`${auditTime(r.draft.created_at)} UTC`}>{relTime(r.draft.created_at, now)}</time>
                ) : (
                  <span className="muted">{copy.admin.notRecorded}</span>
                )}
              </td>
              {canRelease && (
                <td className="c-action">
                  <button
                    type="button"
                    className="btn btn-sm"
                    aria-label={B.releaseLabel(r.draft.id, r.draft.user)}
                    onClick={(e) => onRelease(r, e.currentTarget)}
                  >
                    {B.release}
                  </button>
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * The Admin's view of the bot marketplace: the catalog with each bot's state, who has it, its
 * checks and its file; the bots teams still name but the catalog lost; and the drafts on hold,
 * which an admin may release. Nothing here offers, withdraws or adds a bot: that is a content
 * change by pull request, which the File column opens.
 */
export function AdminBots() {
  const { api, me } = useApp();
  const load = useCallback(() => api.adminBots(), [api]);
  const { data, error, forbidden, loading, reload } = useLoad(load, B.loadFailed);
  const [releasing, setReleasing] = useState<HeldRow | null>(null);
  const opener = useRef<HTMLButtonElement | null>(null);
  if (forbidden) return <NoAccess />;

  const canEdit = data ? data.can_edit_content : canEditContent(me);
  const rows = data ? heldRows(data) : [];
  const offered = data?.bots.filter((b) => b.state !== 'withdrawn').length ?? 0;
  const previews = data?.bots.filter((b) => b.state === 'preview').length ?? 0;
  const withdrawn = data?.bots.filter((b) => b.state === 'withdrawn').length ?? 0;
  const flags = data?.bots.filter((b) => b.name_clashes.length > 0 || (b.lint !== null && b.lint.status !== 'ok')).length ?? 0;

  const close = () => {
    setReleasing(null);
    opener.current?.focus();
  };

  return (
    <div className="page page-wide">
      <AdminHead section="bots" title={copy.admin.sections.bots} lede={B.lede}>
        <button type="button" className="btn btn-keep" disabled={loading} onClick={reload}>
          <Icon name="refresh" size={15} />
          {loading && data ? B.reloading : B.reload}
        </button>
      </AdminHead>
      <p className="meta-line">
        <Icon name="pullRequest" size={15} />
        <span>
          {canEdit ? B.offerNote : B.offerNoteViewer} {B.metadataOnly}
        </span>
      </p>
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>
          {error}
        </ErrorNote>
      )}
      {!data && !error && <Skeleton rows={4} label={B.loading} variant="cards" />}
      {data && (
        <>
          <div className="stats" role="group" aria-label={B.catalogHeading}>
            <StatusTile label={B.stats.offered} value={offered} sub={B.stats.offeredSub(previews)} />
            <StatusTile label={B.stats.withdrawn} value={withdrawn} sub={B.stats.withdrawnSub} />
            <StatusTile label={B.stats.held} value={data.held_total} sub={B.stats.heldSub(data.held_total)} tone={data.held_total > 0 ? 'warn' : undefined} />
            <StatusTile label={B.stats.flags} value={flags} sub={flags > 0 ? B.stats.flagsSub : B.allClear} tone={flags > 0 ? 'warn' : undefined} />
          </div>

          {data.unknown.length > 0 && (
            <div className="note note-warn" role="note">
              <Icon name="alert" className="note-icon" />
              <div className="note-body">
                <p><strong>{B.unknownHeading}</strong></p>
                <ul className="unknown-bots">
                  {data.unknown.map((u) => (
                    <li key={u.key}>
                      {B.unknownLine(u.key, u.users, u.held.length)}
                      {u.handles.length > 0 && <> <span className="mono-val">({u.handles.join(', ')})</span></>}
                    </li>
                  ))}
                </ul>
              </div>
            </div>
          )}

          <section className="card card-table" aria-labelledby="bots-catalog-h">
            <div className="card-head card-head-sub">
              <h2 id="bots-catalog-h">
                {B.catalogHeading}
                <span className="card-count">{data.bots.length}</span>
              </h2>
              {canEdit && (
                <a className="btn btn-sm" href="#/admin/content">
                  <Icon name="pullRequest" size={15} />
                  {B.openContent}
                </a>
              )}
            </div>
            {data.bots.length === 0 ? <Empty icon="cpu">{B.loadFailed}</Empty> : <CatalogTable bots={data.bots} canEdit={canEdit} />}
          </section>

          <section className="card card-table" aria-labelledby="bots-held-h">
            <div className="card-head card-head-sub">
              <h2 id="bots-held-h" tabIndex={-1}>
                {B.heldHeading}
                <span className="card-count">{data.held_total}</span>
              </h2>
            </div>
            <p className="card-lede">{B.heldLede}</p>
            {rows.length === 0 ? (
              <Empty icon="check" small>{B.heldNone}</Empty>
            ) : (
              <>
                <HeldTable
                  rows={rows}
                  canRelease={data.can_release}
                  onRelease={(r, from) => {
                    opener.current = from;
                    setReleasing(r);
                  }}
                />
                {!data.can_release && <p className="card-foot muted small">{B.releaseAdminOnly}</p>}
                {data.held_total > rows.length && <p className="card-foot muted small">{B.heldMore(rows.length, data.held_total)}</p>}
              </>
            )}
          </section>
        </>
      )}
      {releasing && (
        <ReleaseDialog
          row={releasing}
          onClose={close}
          onDone={() => {
            setReleasing(null);
            reload();
            // The row is gone: focus goes to the section's heading instead.
            document.getElementById('bots-held-h')?.focus();
          }}
        />
      )}
    </div>
  );
}
