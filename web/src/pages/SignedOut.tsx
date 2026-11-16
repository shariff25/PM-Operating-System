import { Mark } from '../components/icons';
import { copy } from '../copy';

/** The message for a reason. The raw value is only ever a lookup key, never shown. */
export function signedOutMessage(reason: string): string {
  return Object.hasOwn(copy.signedOut.reasons, reason) ? copy.signedOut.reasons[reason]! : copy.signedOut.failed;
}

/** Where a sign-in that did not finish lands. "Try again" goes back to the sign-in screen. */
export function SignedOut({ reason, onTryAgain }: { reason: string; onTryAgain: () => void }) {
  return (
    <div className="signin-wrap">
      <main className="signin">
        <Mark size={36} />
        <h1>{copy.signedOut.title}</h1>
        <p role="status">{signedOutMessage(reason)}</p>
        <button type="button" className="btn btn-primary" onClick={onTryAgain}>
          {copy.signedOut.tryAgain}
        </button>
      </main>
    </div>
  );
}
