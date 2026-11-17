import { useEffect, useState } from 'react';

/** Anything that sits over the page and takes the focus: a dialog, a menu, the command palette. */
export const MODAL_LAYER = '[aria-modal="true"], [role="dialog"], [role="alertdialog"], [role="menu"]';

const anyOpen = () => typeof document !== 'undefined' && document.querySelector(MODAL_LAYER) !== null;

/** True while any modal layer is open anywhere on the page. Follows the page as layers come and go. */
export function useModalOpen(): boolean {
  const [open, setOpen] = useState(anyOpen);
  useEffect(() => {
    const check = () => setOpen(anyOpen());
    check();
    const watch = new MutationObserver(check);
    watch.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['role', 'aria-modal'] });
    return () => watch.disconnect();
  }, []);
  return open;
}
