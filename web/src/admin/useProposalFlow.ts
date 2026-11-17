import { useCallback, useRef, useState } from 'react';
import type { ProposalResult, Validation } from '../api';
import { copy } from '../copy';
import { canPropose, checkIsStale, problemOf } from './proposals';
import type { Checked, Problem } from './proposals';

export interface ProposalFlow {
  checked: Checked | null;
  checking: boolean;
  /** A refusal of the check itself (the change was not looked at). */
  checkProblem: Problem | null;
  /** Only after a clean check of exactly the change on screen. */
  ready: boolean;
  /** The result on screen is for an older version of the change. */
  stale: boolean;
  runCheck: () => Promise<void>;
  dialogOpen: boolean;
  openDialog: () => void;
  closeDialog: () => void;
  submitting: boolean;
  submitProblem: Problem | null;
  submit: (title: string, reason: string) => Promise<void>;
  done: ProposalResult | null;
  /** Clear the result and the check, for the next change. */
  reset: () => void;
}

/**
 * Check, then propose: the same steps on the policy and the content page. `key` names the change on
 * screen; `validate` and `propose` send it. A proposal is allowed only for the key that last
 * passed a check.
 */
export function useProposalFlow({
  key,
  empty,
  validate,
  propose,
  onOpened,
}: {
  key: string;
  empty: boolean;
  validate: () => Promise<Validation>;
  propose: (title: string, reason: string) => Promise<ProposalResult>;
  onOpened: () => void;
}): ProposalFlow {
  const [checked, setChecked] = useState<Checked | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkProblem, setCheckProblem] = useState<Problem | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [submitProblem, setSubmitProblem] = useState<Problem | null>(null);
  const [done, setDone] = useState<ProposalResult | null>(null);
  const run = useRef(0);

  const ready = canPropose(checked, key, empty);

  const runCheck = useCallback(async () => {
    const mine = ++run.current;
    setChecking(true);
    setCheckProblem(null);
    setDone(null);
    try {
      const result = await validate();
      if (mine === run.current) setChecked({ key, result });
    } catch (e) {
      if (mine === run.current) {
        setChecked(null);
        setCheckProblem(problemOf(e, copy.admin.propose.checkFailed));
      }
    } finally {
      if (mine === run.current) setChecking(false);
    }
  }, [validate, key]);

  const submit = useCallback(
    async (title: string, reason: string) => {
      if (!ready || !checked) return;
      setSubmitting(true);
      setSubmitProblem(null);
      try {
        const result = await propose(title, reason);
        setDone(result);
        setDialogOpen(false);
        setChecked(null);
        onOpened();
      } catch (e) {
        const p = problemOf(e, copy.errors.unknown);
        setSubmitProblem(p);
        // The server's re-check failed: the result on screen is no longer clean, so proposing stops here.
        if (p.code === 'invalid') {
          const errors = p.issues.filter((i) => i.severity === 'error');
          const warnings = p.issues.filter((i) => i.severity === 'warning');
          setChecked({ key: checked.key, result: { ...checked.result, valid: false, errors, warnings } });
        }
      } finally {
        setSubmitting(false);
      }
    },
    [ready, checked, propose, onOpened],
  );

  return {
    checked,
    checking,
    checkProblem,
    ready,
    stale: checkIsStale(checked, key),
    runCheck,
    dialogOpen,
    openDialog: () => {
      if (!ready) return;
      setSubmitProblem(null);
      setDialogOpen(true);
    },
    closeDialog: () => setDialogOpen(false),
    submitting,
    submitProblem,
    submit,
    done,
    reset: () => {
      setDone(null);
      setChecked(null);
      setSubmitProblem(null);
      setCheckProblem(null);
    },
  };
}
