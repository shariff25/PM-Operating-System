import { useEffect, useState } from 'react';
import type { PasswordResetNotice } from '../api';
import { useApp } from '../app-context';
import { copy } from '../copy';
import { Icon } from './icons';

const R = copy.password.resetNotice;

/** "20 November 2026": the day of a reset, in the reader's own words for dates. */
function day(at: string): string {
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? at : d.toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric' });
}

/** Who reset it: the admin's name, their handle, or someone with host access. Plain text. */
export function resetBy(n: PasswordResetNotice): string {
  if (n.by === 'host') return R.host;
  return n.by_name?.trim() || n.by;
}

/**
 * Someone made a reset code for the signed-in person (docs/web-api.md, "Password sign-in"): a
 * plain notice at the top of every page, from their next sign-in until they dismiss it. A reset is
 * never silent. Shows nothing when there is no notice, password sign-in is off, or the status
 * can't be read.
 */
export function ResetNotice() {
  const { api } = useApp();
  const [notice, setNotice] = useState<PasswordResetNotice | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => api.passwordStatus())
      .then(
        (s) => live && setNotice(s.enabled && s.reset_notice ? s.reset_notice : null),
        () => live && setNotice(null),
      );
    return () => {
      live = false;
    };
  }, [api]);

  if (!notice) return null;
  const dismiss = async () => {
    setBusy(true);
    setFailed(false);
    try {
      await api.passwordDismissResetNotice();
      setNotice(null);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="note note-warn reset-notice" role="status" data-testid="reset-notice">
      <Icon name="shield" className="note-icon" />
      <div className="note-body">
        <p>{R.text(resetBy(notice), day(notice.at))}</p>
        {failed && <p role="alert">{R.dismissFailed}</p>}
      </div>
      <button type="button" className="btn btn-sm" onClick={() => void dismiss()} disabled={busy}>
        {busy ? R.dismissing : R.dismiss}
      </button>
    </div>
  );
}
