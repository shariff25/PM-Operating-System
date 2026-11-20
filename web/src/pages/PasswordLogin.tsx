import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import type { Api } from '../api';
import { ApiError } from '../api';
import { Icon } from '../components/icons';
import { CodeField, PasswordField, PlainField, RefusalNote, WaitNote, WaitOver, refusalText, retryAfterOf } from '../components/password';
import type { Refusal } from '../components/password';
import { copy } from '../copy';

const P = copy.password;

type Step = 'password' | 'code' | 'recovery' | 'recovered';

/**
 * Password sign-in (docs/web-api.md, "Password sign-in"): the sign-in name and password, then
 * always a second step, an authenticator code or a recovery code. Only the second step makes a
 * session. The password is held in this form only until the first step answers, and nothing typed
 * is ever logged, stored or put in the address.
 */
export function PasswordLogin({
  api,
  onSignedIn,
  primary,
  onSecondStep,
}: {
  api: Api;
  onSignedIn: () => void;
  /** The main way to sign in here: its button is the primary one. */
  primary: boolean;
  /** Told when the card moves to the second step and back, so the other ways to sign in step aside. */
  onSecondStep: (on: boolean) => void;
}) {
  const [step, setStep] = useState<Step>('password');
  const [name, setName] = useState('');
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [recovery, setRecovery] = useState('');
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [waitUntil, setWaitUntil] = useState<number | null>(null);
  const [waitOver, setWaitOver] = useState(false);
  const [left, setLeft] = useState(0);
  const seq = useRef(0);
  const first = useRef<HTMLInputElement>(null);
  const moved = useRef(false);

  // Focus goes to the first field of each step (not on first load: the page doesn't steal focus).
  useEffect(() => {
    if (!moved.current) return;
    if (step !== 'recovered') first.current?.focus();
    else document.getElementById('pw-recovered-title')?.focus();
  }, [step]);

  const go = (next: Step) => {
    moved.current = true;
    setStep(next);
    onSecondStep(next !== 'password');
  };
  const refuse = (text: string) => {
    seq.current += 1;
    setRefusal({ text, seq: seq.current });
  };
  /** Back to the password, with the name kept and everything secret cleared. */
  const restart = (text: string | null) => {
    setPassword('');
    setCode('');
    setRecovery('');
    go('password');
    if (text) refuse(text);
    else setRefusal(null);
  };

  const fail = (e: unknown) => {
    const wait = retryAfterOf(e);
    if (wait !== null) {
      setRefusal(null);
      setWaitOver(false);
      setWaitUntil(Date.now() + wait * 1000);
      return;
    }
    if (e instanceof ApiError && e.code === 'too_many_attempts') return refuse(P.wait.noTime);
    if (step === 'password') {
      // One sentence for every reason: it never says which part was wrong, or whether the account exists.
      if (e instanceof ApiError && e.code === 'sign_in_failed') setPassword('');
      return refuse(refusalText(e));
    }
    if (e instanceof ApiError && e.code === 'code_failed') {
      const n = Number(e.details.attempts_left);
      if (Number.isFinite(n) && n <= 0) return restart(P.refusals.lastTry);
      setCode('');
      setRecovery('');
      const tries = Number.isFinite(n) ? n : 1;
      return refuse(step === 'recovery' ? P.refusals.recoveryFailed(tries) : P.refusals.codeFailed(tries));
    }
    if (e instanceof ApiError && (e.code === 'sign_in_expired' || e.code === 'not_a_member')) return restart(refusalText(e));
    refuse(refusalText(e));
  };

  const run = async (fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setRefusal(null);
    setWaitOver(false);
    try {
      await fn();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const waiting = waitUntil !== null;
  const notice = (
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

  if (step === 'password') {
    const ready = name.trim() !== '' && password !== '';
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (!ready || waiting) return;
      void run(async () => {
        await api.passwordSignIn(name.trim(), password);
        // The password has done its job: it is not kept for the second step.
        setPassword('');
        go('code');
      });
    };
    return (
      <form onSubmit={submit} className="signin-form pw-form" noValidate data-testid="password-form">
        <PlainField ref={first} label={P.nameLabel} type="email" value={name} onChange={setName} autoComplete="username" hint={P.nameHint} />
        <PasswordField label={P.passwordLabel} value={password} onChange={setPassword} autoComplete="current-password" />
        {notice}
        <button type="submit" className={`btn ${primary ? 'btn-primary' : ''}`} disabled={busy || waiting || !ready}>
          {busy ? P.working : P.submit}
        </button>
        <a className="pw-setup-link" href="#/setup">
          <Icon name="lock" size={14} />
          {P.setupLink}
        </a>
      </form>
    );
  }

  if (step === 'recovered') {
    return (
      <div className="pw-step" data-testid="recovered">
        <div className="signin-head">
          <h1 id="pw-recovered-title" tabIndex={-1}>{P.recoveredTitle}</h1>
        </div>
        <div className={`note ${left === 0 ? 'note-error' : left <= 2 ? 'note-warn' : 'note-info'}`} role="status">
          <Icon name={left <= 2 ? 'alert' : 'info'} className="note-icon" />
          <div className="note-body">
            <p>{left === 0 ? P.recoveredNone : P.recoveredLeft(left)}</p>
            <p>{P.recoveredWhere}</p>
          </div>
        </div>
        <div className="pw-actions">
          <button type="button" className="btn btn-primary" onClick={onSignedIn}>{P.continue}</button>
          <button
            type="button"
            className="btn"
            onClick={() => {
              window.location.hash = '/security';
              onSignedIn();
            }}
          >
            {P.makeNew}
          </button>
        </div>
      </div>
    );
  }

  const usingRecovery = step === 'recovery';
  const ready = usingRecovery ? recovery.trim() !== '' : code.length === 6;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!ready || waiting) return;
    void run(async () => {
      const r = await api.passwordSecondStep(usingRecovery ? { recovery_code: recovery.trim() } : { code });
      setCode('');
      setRecovery('');
      if (r.recovery_code_used) {
        setLeft(r.recovery_codes_left);
        go('recovered');
      } else {
        onSignedIn();
      }
    });
  };
  return (
    <form onSubmit={submit} className="signin-form pw-step" noValidate data-testid="second-step">
      <div className="signin-head">
        <span className="pw-step-icon" aria-hidden="true"><Icon name={usingRecovery ? 'lock' : 'shield'} size={18} /></span>
        <h1>{usingRecovery ? P.recoveryTitle : P.codeTitle}</h1>
        <p className="muted">{usingRecovery ? P.recoveryLede : P.codeLede}</p>
      </div>
      {usingRecovery ? (
        <PlainField ref={first} label={P.recoveryLabel} value={recovery} onChange={setRecovery} autoComplete="one-time-code" hint={P.recoveryHint} mono />
      ) : (
        <CodeField
          ref={first}
          label={P.codeLabel}
          value={code}
          onChange={setCode}
          hint={code.length > 0 && code.length < 6 ? P.codeIncomplete(code.length) : refusal ? P.codeHintShort : P.codeHint}
        />
      )}
      {notice}
      <button type="submit" className="btn btn-primary" disabled={busy || waiting || !ready}>
        {busy ? P.verifying : P.verify}
      </button>
      <div className="pw-links">
        <button
          type="button"
          className="pw-link"
          onClick={() => {
            setRefusal(null);
            setCode('');
            setRecovery('');
            go(usingRecovery ? 'code' : 'recovery');
          }}
        >
          {usingRecovery ? P.useCode : P.useRecovery}
        </button>
        <button type="button" className="pw-link" onClick={() => restart(null)}>
          {P.startOver}
        </button>
      </div>
    </form>
  );
}
