import { useEffect, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import type { Api, PasswordEnrolment, PasswordSetupStarted } from '../../api';
import { ApiError } from '../../api';
import { Icon } from '../../components/icons';
import {
  CodeField,
  PasswordField,
  PlainField,
  RefusalNote,
  WaitNote,
  WaitOver,
  charCount,
  refusalText,
  retryAfterOf,
} from '../../components/password';
import type { Refusal } from '../../components/password';
import { copy } from '../../copy';
import { SignInBrand } from '../SignIn';
import { QrCode } from './QrCode';
import { RecoveryCodes } from './RecoveryCodes';
import { PasswordRules, weakText } from './rules';

const P = copy.password;
const S = P.setup;

type Stage = 'begin' | 'password' | 'authenticator' | 'confirm' | 'codes';
const STEP: Record<Stage, number> = { begin: 1, password: 2, authenticator: 3, confirm: 4, codes: 5 };

/** "Step 2 of 4", and the four steps with the current one marked. */
function Progress({ stage }: { stage: Stage }) {
  const n = STEP[stage];
  return (
    <div className="setup-progress">
      <p className="eyebrow">{stage === 'codes' ? S.done : S.step(n, 4)}</p>
      <ol className="setup-steps" aria-label={S.progressLabel}>
        {S.steps.map((label, i) => {
          const state = i + 1 < n ? 'done' : i + 1 === n ? 'now' : 'next';
          return (
            <li key={label} className={`setup-step setup-step-${state}`} aria-current={state === 'now' ? 'step' : undefined}>
              <span className="setup-bar" aria-hidden="true" />
              <span className="setup-step-label">
                {label}
                {state === 'done' && <span className="sr-only">, {S.done.toLowerCase()}</span>}
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/** The key as text for someone who can't scan, with a copy button. */
function SecretKey({ enrolment }: { enrolment: PasswordEnrolment }) {
  const [copied, setCopied] = useState<'yes' | 'failed' | null>(null);
  // Groups of four, as authenticator apps show and accept it.
  const grouped = enrolment.secret.replace(/(.{4})/g, '$1 ').trim();
  return (
    <div className="setup-key">
      <p className="field-label" id="setup-key-label">{S.keyLabel}</p>
      <div className="setup-key-row">
        <code className="setup-key-text" aria-labelledby="setup-key-label">{grouped}</code>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => {
            if (!navigator.clipboard?.writeText) return setCopied('failed');
            navigator.clipboard.writeText(enrolment.secret).then(() => setCopied('yes'), () => setCopied('failed'));
          }}
        >
          <Icon name={copied === 'yes' ? 'check' : 'docs'} size={14} />
          {copied === 'yes' ? S.copied : S.copyKey}
        </button>
      </div>
      <span className="sr-only" role="status">{copied === 'yes' ? S.copied : ''}</span>
      {copied === 'failed' && <p className="field-hint" role="alert">{S.copyFailed}</p>}
      <p className="field-hint">
        {S.keyFacts(enrolment.account, enrolment.issuer)} {S.keyDetails(enrolment.algorithm, enrolment.digits, enrolment.period)}
      </p>
    </div>
  );
}

const finishTime = (at: string) => {
  const t = new Date(at);
  return Number.isNaN(t.getTime()) ? at : t.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
};

/**
 * `#/setup`: a password and an authenticator from a setup or reset code an admin made, in the
 * contract's order (docs/web-api.md, "Password sign-in"): the code, a password, the authenticator,
 * a code from it, then the ten recovery codes. Nothing is kept outside this page's memory; leaving
 * part-way changes nothing, and the same code starts it again.
 */
export function SetupPage({ api, onDone }: { api: Api; onDone: () => Promise<void> }) {
  const [stage, setStage] = useState<Stage>('begin');
  const [name, setName] = useState('');
  const [setupCode, setSetupCode] = useState('');
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [started, setStarted] = useState<PasswordSetupStarted | null>(null);
  const [enrolment, setEnrolment] = useState<PasswordEnrolment | null>(null);
  const [fresh, setFresh] = useState(false);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [reset, setReset] = useState(false);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [waitUntil, setWaitUntil] = useState<number | null>(null);
  const [waitOver, setWaitOver] = useState(false);
  const seq = useRef(0);
  const first = useRef<HTMLInputElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  // Focus goes to the first field of each step, or to the heading where there is none.
  useEffect(() => {
    if (stage === 'authenticator' || stage === 'codes') heading.current?.focus();
    else first.current?.focus();
  }, [stage]);

  const rules = started?.password ?? { min_length: 12, max_length: 128 };
  const refuse = (text: string) => {
    seq.current += 1;
    setRefusal({ text, seq: seq.current });
  };
  /** Back to the first step: nothing changed, and the same code works again. */
  const restart = (text: string | null) => {
    setStarted(null);
    setEnrolment(null);
    setFresh(false);
    setPassword('');
    setCode('');
    setSetupCode('');
    setReset(false);
    setStage('begin');
    if (text) refuse(text);
    else setRefusal(null);
  };

  const fail = (e: unknown, at: Stage) => {
    const wait = retryAfterOf(e);
    if (wait !== null) {
      setRefusal(null);
      setWaitOver(false);
      setWaitUntil(Date.now() + wait * 1000);
      return;
    }
    const code = e instanceof ApiError ? e.code : '';
    if (code === 'too_many_attempts') return refuse(P.wait.noTime);
    if (code === 'setup_expired' || code === 'wrong_step' || code === 'setup_code_spent') return restart(S[code]);
    if (code === 'weak_password') return refuse(weakText(e instanceof ApiError ? e.details.reason : null, rules));
    if (code === 'code_failed' && e instanceof ApiError) {
      const n = Number(e.details.attempts_left);
      if (Number.isFinite(n) && n <= 0) return restart(S.setup_expired);
      setCode('');
      return refuse(S.codeFailed(Number.isFinite(n) ? n : 1));
    }
    const own = code === 'setup_failed' || code === 'not_a_member' || code === 'sign_in_unconfirmed' || code === 'host_only' || code === 'evals_account';
    // Past the first step, a refusal like these means this setup can't go on: start again, saying why.
    if (own && at !== 'begin') return restart(S[code as 'setup_failed']);
    if (own) {
      if (code === 'setup_failed') setSetupCode('');
      return refuse(S[code as 'setup_failed']);
    }
    refuse(refusalText(e));
  };

  const run = async (at: Stage, fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setRefusal(null);
    setWaitOver(false);
    try {
      await fn();
    } catch (e) {
      fail(e, at);
    } finally {
      setBusy(false);
    }
  };

  const enrol = (again: boolean) =>
    run('authenticator', async () => {
      const en = await api.passwordSetupAuthenticator();
      setEnrolment(en);
      setFresh(again);
      setStage('authenticator');
    });

  const waiting = waitUntil !== null;
  const notes = (
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

  let body: ReactNode;
  if (stage === 'begin') {
    const ready = name.trim() !== '' && setupCode.trim() !== '';
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (!ready || waiting) return;
      void run('begin', async () => {
        const s = await api.passwordSetupBegin(name.trim(), setupCode.trim());
        setStarted(s);
        setReset(s.kind === 'reset');
        setStage('password');
      });
    };
    body = (
      <form className="signin-form pw-form" onSubmit={submit} noValidate>
        <PlainField ref={first} label={P.nameLabel} type="email" value={name} onChange={setName} autoComplete="username" hint={P.nameHint} />
        <PlainField label={S.codeLabel} value={setupCode} onChange={setSetupCode} autoComplete="off" hint={S.codeHint} mono />
        {notes}
        <button type="submit" className="btn btn-primary" disabled={busy || waiting || !ready}>
          {busy ? S.beginning : S.begin}
        </button>
        <p className="pw-aside">{S.leaveNote}</p>
      </form>
    );
  } else if (stage === 'password') {
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (password === '' || waiting) return;
      const n = charCount(password);
      // The length is checked here too, so a too-short password needs no round trip. The server checks everything again.
      if (n < rules.min_length) return refuse(P.weak.too_short(rules.min_length));
      if (n > rules.max_length) return refuse(P.weak.too_long(rules.max_length));
      void run('password', async () => {
        await api.passwordSetupPassword(password);
        // Held no longer than this step: the server keeps it with the setup until the end.
        setPassword('');
        const en = await api.passwordSetupAuthenticator();
        setEnrolment(en);
        setFresh(false);
        setStage('authenticator');
      });
    };
    body = (
      <form className="signin-form pw-form" onSubmit={submit} noValidate>
        {/* For password managers: the new password belongs to this sign-in name. */}
        <input className="sr-only pw-hidden-user" type="text" name="username" autoComplete="username" value={name.trim()} readOnly tabIndex={-1} aria-hidden="true" />
        <PasswordRules min={rules.min_length} max={rules.max_length} id="setup-rules" />
        <PasswordField ref={first} label={S.newPassword} value={password} onChange={setPassword} autoComplete="new-password" />
        {notes}
        <button type="submit" className="btn btn-primary" disabled={busy || waiting || password === ''}>
          {busy ? S.savingPassword : S.savePassword}
        </button>
      </form>
    );
  } else if (stage === 'authenticator' && enrolment) {
    body = (
      <div className="pw-form setup-auth">
        <p className="muted">{S.authLede}</p>
        <div className="setup-qr">
          <QrCode text={enrolment.otpauth_uri} label={S.qrLabel(enrolment.account)} size={184} />
        </div>
        {fresh && (
          <p className="note note-info" role="status">
            <Icon name="refresh" className="note-icon" />
            <span>{S.newQrDone}</span>
          </p>
        )}
        <p className="pw-aside">{S.apps}</p>
        <p className="setup-cant">{S.cantScan}</p>
        <SecretKey enrolment={enrolment} />
        {notes}
        <button type="button" className="btn btn-primary" onClick={() => setStage('confirm')}>
          {S.added}
        </button>
        <button type="button" className="pw-link" disabled={busy} onClick={() => void enrol(true)}>
          {busy ? S.makingQr : S.newQr}
        </button>
      </div>
    );
  } else if (stage === 'confirm') {
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (code.length !== 6 || waiting) return;
      void run('confirm', async () => {
        const done = await api.passwordSetupConfirm(code);
        setCode('');
        // The secret has done its job: it is not kept past this step.
        setEnrolment(null);
        setReset(done.kind === 'reset');
        setCodes(done.recovery_codes);
        setStage('codes');
      });
    };
    body = (
      <form className="signin-form pw-form" onSubmit={submit} noValidate>
        <p className="muted">{S.confirmLede}</p>
        <CodeField
          ref={first}
          label={P.codeLabel}
          value={code}
          onChange={setCode}
          hint={code.length > 0 && code.length < 6 ? P.codeIncomplete(code.length) : S.confirmHint}
        />
        {notes}
        <button type="submit" className="btn btn-primary" disabled={busy || waiting || code.length !== 6}>
          {busy ? S.confirming : S.confirm}
        </button>
        <button type="button" className="pw-link" onClick={() => setStage('authenticator')}>
          {S.backToQr}
        </button>
      </form>
    );
  } else if (stage === 'codes' && codes) {
    body = (
      <div className="pw-form">
        {reset && (
          <div className="note note-info" role="status">
            <Icon name="info" className="note-icon" />
            <div className="note-body"><p>{S.resetDone}</p></div>
          </div>
        )}
        <p className="muted">{S.codesLede}</p>
        <RecoveryCodes codes={codes} saved={saved} onSaved={setSaved} />
        <button
          type="button"
          className="btn btn-primary"
          disabled={!saved || busy}
          aria-describedby={saved ? undefined : 'setup-saved-first'}
          onClick={() => {
            setBusy(true);
            void onDone().finally(() => setBusy(false));
          }}
        >
          {S.continue}
        </button>
        {!saved && <p id="setup-saved-first" className="field-hint">{P.codes.savedFirst}</p>}
      </div>
    );
  }

  const title =
    stage === 'begin' ? (reset ? S.titleReset : S.title)
    : stage === 'password' ? S.passwordTitle
    : stage === 'authenticator' ? S.authTitle
    : stage === 'confirm' ? S.confirmTitle
    : S.codesTitle;

  return (
    <div className="signin-wrap">
      <SignInBrand />
      <main className="signin setup">
        <Progress stage={stage} />
        <div className="signin-head">
          <h1 ref={heading} tabIndex={-1}>{title}</h1>
          {stage === 'begin' && <p className="muted">{S.lede}</p>}
        </div>
        {started && stage !== 'begin' && stage !== 'codes' && (
          <div className="setup-meta">
            <p className="meta-line">
              <Icon name="lock" size={14} />
              <span>
                <strong className="setup-as">{S.as(started.handle)}</strong> {S.finishBy(finishTime(started.expires_at))}
              </span>
            </p>
            {started.kind === 'reset' && (
              <div className="note note-warn">
                <Icon name="alert" className="note-icon" />
                <div className="note-body"><p>{S.resetNote}</p></div>
              </div>
            )}
          </div>
        )}
        {body}
        {stage !== 'codes' && (
          <div className="pw-links setup-foot">
            {stage === 'begin' ? (
              <a className="pw-link" href="#/">
                <Icon name="arrowLeft" size={14} />
                {S.backToSignIn}
              </a>
            ) : (
              <button type="button" className="pw-link" onClick={() => restart(null)}>
                {S.startOver}
              </button>
            )}
          </div>
        )}
      </main>
    </div>
  );
}
