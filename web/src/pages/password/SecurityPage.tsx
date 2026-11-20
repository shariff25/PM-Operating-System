import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import type { PasswordStatus } from '../../api';
import { ApiError } from '../../api';
import { useApp } from '../../app-context';
import { ErrorNote, PageHeader, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import { CodeField, PasswordField, RefusalNote, WaitNote, WaitOver, charCount, refusalText, retryAfterOf } from '../../components/password';
import type { Refusal } from '../../components/password';
import { copy } from '../../copy';
import { RecoveryCodes } from './RecoveryCodes';
import { PasswordRules, weakText } from './rules';

const P = copy.password;
const S = P.security;
const RULES = { min_length: 12, max_length: 128 };

/** One form's request: busy, the last refusal (focused), and a wait after too many tries. */
function useAttempt() {
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [waitUntil, setWaitUntil] = useState<number | null>(null);
  const [waitOver, setWaitOver] = useState(false);
  const seq = useRef(0);

  const refuse = (text: string) => {
    seq.current += 1;
    setRefusal({ text, seq: seq.current });
  };

  const run = async (fn: () => Promise<void>, onFail: (e: unknown) => string | null) => {
    if (busy) return;
    setBusy(true);
    setRefusal(null);
    setWaitOver(false);
    try {
      await fn();
    } catch (e) {
      const wait = retryAfterOf(e);
      if (wait !== null) setWaitUntil(Date.now() + wait * 1000);
      else {
        const text = onFail(e);
        if (text) refuse(text);
      }
    } finally {
      setBusy(false);
    }
  };

  const notes: ReactNode = (
    <>
      {waitUntil !== null && (
        <WaitNote
          until={waitUntil}
          onOver={() => {
            setWaitUntil(null);
            setWaitOver(true);
          }}
        />
      )}
      {waitOver && <WaitOver />}
      {refusal && <RefusalNote refusal={refusal} />}
    </>
  );
  return { busy, run, refuse, notes, waiting: waitUntil !== null, clear: () => setRefusal(null) };
}

/** What a refusal on this page means. `unknown_outcome` may have happened: it never says either way. */
function securityText(e: unknown): string {
  const code = e instanceof ApiError ? e.code : '';
  if (code === 'reauth_failed') return S.reauth_failed;
  if (code === 'no_password') return S.no_password;
  if (code === 'unknown_outcome') return S.unknown;
  if (code === 'too_many_attempts') return P.wait.noTime;
  if (code === 'weak_password') return weakText(e instanceof ApiError ? e.details.reason : null, RULES);
  return refusalText(e);
}

function ChangePassword({ onChanged }: { onChanged: () => Promise<void> }) {
  const { api } = useApp();
  const [current, setCurrent] = useState('');
  const [code, setCode] = useState('');
  const [next, setNext] = useState('');
  const [done, setDone] = useState(false);
  const a = useAttempt();
  const ready = current !== '' && code.length === 6 && next !== '';

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!ready || a.waiting) return;
    setDone(false);
    const n = charCount(next);
    if (n < RULES.min_length) return a.refuse(P.weak.too_short(RULES.min_length));
    if (n > RULES.max_length) return a.refuse(P.weak.too_long(RULES.max_length));
    void a.run(
      async () => {
        await api.passwordChange({ current_password: current, code, new_password: next });
        setCurrent('');
        setCode('');
        setNext('');
        setDone(true);
        // The session was replaced: read Me again for its new CSRF token.
        await onChanged();
      },
      (err) => {
        // A code works once, and a wrong current password is not shown again.
        setCode('');
        if (err instanceof ApiError && err.code === 'reauth_failed') setCurrent('');
        return securityText(err);
      },
    );
  };

  return (
    <section className="card security-card" aria-labelledby="change-h">
      <div className="card-head">
        <h2 id="change-h"><Icon name="lock" size={16} />{S.changeTitle}</h2>
      </div>
      <form className="card-body security-form" onSubmit={submit} noValidate>
        <p className="muted">{S.changeLede}</p>
        <div className="security-grid">
          <div className="security-fields">
            <PasswordField label={S.current} value={current} onChange={setCurrent} autoComplete="current-password" />
            <CodeField label={S.code} value={code} onChange={setCode} hint={code.length > 0 && code.length < 6 ? P.codeIncomplete(code.length) : undefined} />
            <PasswordField label={S.next} value={next} onChange={setNext} autoComplete="new-password" />
          </div>
          <PasswordRules min={RULES.min_length} max={RULES.max_length} />
        </div>
        <p className="quiet-line security-ends">
          <Icon name="logout" size={14} />
          <span>{S.changeEnds}</span>
        </p>
        {a.notes}
        {done && (
          <div className="note note-good" role="status">
            <Icon name="check" className="note-icon" />
            <div className="note-body"><p>{S.changed}</p></div>
          </div>
        )}
        <div className="security-actions">
          <button type="submit" className="btn btn-primary" disabled={a.busy || a.waiting || !ready}>
            {a.busy ? S.changing : S.change}
          </button>
        </div>
      </form>
    </section>
  );
}

function NewRecoveryCodes({ status, onMade }: { status: PasswordStatus; onMade: () => Promise<void> }) {
  const { api } = useApp();
  const [code, setCode] = useState('');
  const [codes, setCodes] = useState<string[] | null>(null);
  const [saved, setSaved] = useState(false);
  const a = useAttempt();
  const done = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    if (codes) done.current?.focus();
  }, [codes]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (code.length !== 6 || a.waiting) return;
    void a.run(
      async () => {
        const r = await api.passwordRecoveryCodes(code);
        setCode('');
        setSaved(false);
        setCodes(r.recovery_codes);
      },
      (err) => {
        setCode('');
        return securityText(err);
      },
    );
  };

  const left = status.recovery_codes_left;
  return (
    <section className="card security-card" aria-labelledby="codes-h">
      <div className="card-head">
        <h2 id="codes-h"><Icon name="shield" size={16} />{S.codesTitle}</h2>
      </div>
      {codes ? (
        <div className="card-body security-form">
          <h3 ref={done} tabIndex={-1} className="security-new">{S.newTitle}</h3>
          <p className="muted">{P.setup.codesLede}</p>
          <RecoveryCodes codes={codes} saved={saved} onSaved={setSaved} />
          <div className="security-actions">
            <button
              type="button"
              className="btn btn-primary"
              disabled={!saved}
              onClick={() => {
                setCodes(null);
                void onMade();
              }}
            >
              {S.done}
            </button>
            {!saved && <span className="field-hint">{P.codes.savedFirst}</span>}
          </div>
        </div>
      ) : (
        <form className="card-body security-form" onSubmit={submit} noValidate>
          <p className="muted">{S.codesLede}</p>
          {left <= 2 && (
            <div className={`note ${left === 0 ? 'note-error' : 'note-warn'}`} role="status">
              <Icon name="alert" className="note-icon" />
              <div className="note-body"><p>{left === 0 ? S.noneWarn : S.lowWarn(left)}</p></div>
            </div>
          )}
          <p className="quiet-line security-ends">
            <Icon name="refresh" size={14} />
            <span>{S.codesReplace}</span>
          </p>
          <div className="security-fields security-narrow">
            <CodeField label={S.code} value={code} onChange={setCode} hint={code.length > 0 && code.length < 6 ? P.codeIncomplete(code.length) : undefined} />
          </div>
          {a.notes}
          <div className="security-actions">
            <button type="submit" className="btn" disabled={a.busy || a.waiting || code.length !== 6}>
              {a.busy ? S.making : S.make}
            </button>
          </div>
        </form>
      )}
    </section>
  );
}

/** `#/security`: change the password and make new recovery codes, for someone who signs in with a password. */
export function SecurityPage() {
  const { api, refreshMe } = useApp();
  const [state, setState] = useState<{ status: PasswordStatus | null; error: string | null; off: boolean }>({ status: null, error: null, off: false });

  const load = useCallback(async () => {
    try {
      const status = await api.passwordStatus();
      setState({ status, error: null, off: !status.enabled });
    } catch (e) {
      const off = e instanceof ApiError && e.status === 404;
      setState({ status: null, error: off ? null : S.loadFailed, off });
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  const { status, error, off } = state;
  return (
    <div className="page page-narrow security">
      <PageHeader title={S.title} lede={S.lede} />
      {!status && !error && !off && <Skeleton rows={3} label={S.loading} />}
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={() => void load()}>
          {error}
        </ErrorNote>
      )}
      {(off || (status && !status.password_set)) && (
        <div className="note note-info" role="status">
          <Icon name="info" className="note-icon" />
          <div className="note-body"><p>{off ? S.off : S.noPassword}</p></div>
        </div>
      )}
      {status && status.enabled && status.password_set && (
        <>
          <section className="card security-card" aria-labelledby="status-h">
            <div className="card-head">
              <h2 id="status-h"><Icon name="shield" size={16} />{S.statusTitle}</h2>
            </div>
            <dl className="security-status">
              <div>
                <dt>{P.passwordLabel}</dt>
                <dd>
                  <span className="sign-state sign-good">
                    <Icon name="check" size={13} weight={2.25} />
                    {S.passwordOn}
                  </span>
                </dd>
              </div>
              <div>
                <dt>{P.codes.label}</dt>
                <dd className={status.recovery_codes_left <= 2 ? 'sign-warn' : undefined}>{S.recoveryLeft(status.recovery_codes_left)}</dd>
              </div>
            </dl>
          </section>
          <ChangePassword onChanged={refreshMe} />
          <NewRecoveryCodes status={status} onMade={load} />
        </>
      )}
    </div>
  );
}
