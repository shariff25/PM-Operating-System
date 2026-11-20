import { useId, useState } from 'react';
import { saveBlob } from '../../admin/download';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';

const C = copy.password.codes;

/**
 * The ten recovery codes, shown once: copy, download, and a box to tick before going on. The codes
 * live in this component only; they are never stored, logged or put in the address.
 */
export function RecoveryCodes({
  codes,
  saved,
  onSaved,
}: {
  codes: readonly string[];
  saved: boolean;
  onSaved: (v: boolean) => void;
}) {
  const id = useId();
  const [copied, setCopied] = useState<'yes' | 'failed' | null>(null);
  const text = codes.join('\n');

  const copyAll = () => {
    const done = () => setCopied('yes');
    const failed = () => setCopied('failed');
    if (!navigator.clipboard?.writeText) return failed();
    navigator.clipboard.writeText(text).then(done, failed);
  };

  return (
    <div className="codes">
      <ol className="codes-list" aria-label={C.label}>
        {codes.map((c) => (
          <li key={c}><code>{c}</code></li>
        ))}
      </ol>
      <div className="codes-actions">
        <button type="button" className="btn btn-sm" onClick={copyAll}>
          <Icon name={copied === 'yes' ? 'check' : 'docs'} size={14} />
          {copied === 'yes' ? C.copied : C.copy}
        </button>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => saveBlob(new Blob([`${C.fileHead}\n\n${text}\n`], { type: 'text/plain' }), C.fileName)}
        >
          <Icon name="download" size={14} />
          {C.download}
        </button>
        <span className="sr-only" role="status">{copied === 'yes' ? C.copied : copied === 'failed' ? copy.password.setup.copyFailed : ''}</span>
      </div>
      {copied === 'failed' && <p className="field-hint">{copy.password.setup.copyFailed}</p>}
      <label className="check codes-saved" htmlFor={`${id}-saved`}>
        <input id={`${id}-saved`} type="checkbox" checked={saved} onChange={(e) => onSaved(e.target.checked)} />
        {C.saved}
      </label>
    </div>
  );
}
