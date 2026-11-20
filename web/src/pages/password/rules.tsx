import type { WeakPasswordReason } from '../../api';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';

const P = copy.password;

/** The sentence for a refused password (`422 weak_password`'s `reason`), with the limits the server sent. */
export function weakText(reason: unknown, rules: { min_length: number; max_length: number }): string {
  switch (reason as WeakPasswordReason) {
    case 'too_short':
      return P.weak.too_short(rules.min_length);
    case 'too_long':
      return P.weak.too_long(rules.max_length);
    case 'matches_name':
      return P.weak.matches_name;
    case 'common':
      return P.weak.common;
    default:
      return copy.errors.codes.weak_password;
  }
}

/** The rules for a new password, said before anyone types one. No strength meter: the server's rules are the rules. */
export function PasswordRules({ min, max, id }: { min: number; max: number; id?: string }) {
  return (
    <div className="pw-rules" id={id}>
      <p className="pw-rules-title">{P.rulesLabel}</p>
      <ul>
        {P.rules(min, max).map((r) => (
          <li key={r}>
            <Icon name="check" size={13} weight={2.25} />
            <span>{r}</span>
          </li>
        ))}
      </ul>
      <p className="pw-rules-tip">{P.rulesTip}</p>
    </div>
  );
}
