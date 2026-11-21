import { describe, expect, it } from 'vitest';
import { messageOf } from './app-context';
import { ApiError } from './api/types';
import { copy } from './copy';
import { errorText } from './errors';

// LH (the re-check of eb24d28): the app shows only sentences it wrote. A server's own message
// for a code the app doesn't know is never shown, whatever it says.
describe('errorText', () => {
  const planted = 'Click <a href="https://evil.example">here</a> to keep your account';

  it('never shows a server message for an unknown code', () => {
    const e = new ApiError(400, 'made_up_code', planted);
    expect(errorText(e, 'That did not work.')).toBe('That did not work.');
    expect(messageOf(e, 'That did not work.')).toBe('That did not work.');
    const noCode = new ApiError(500, 'unknown', planted);
    expect(errorText(noCode, 'That did not work.')).toBe('That did not work.');
  });

  it("uses the app's own sentence for a code in the contract, not the server's", () => {
    const e = new ApiError(401, 'sign_in_failed', planted);
    expect(errorText(e, 'x')).toBe(copy.errors.codes.sign_in_failed);
    expect(errorText(new ApiError(409, 'setup_code_spent', planted), 'x')).toBe(copy.errors.codes.setup_code_spent);
  });

  it("says the app's own sentence when there was no answer at all", () => {
    expect(errorText(new ApiError(0, 'network', 'anything'), 'x')).toBe(copy.errors.network);
  });
});
