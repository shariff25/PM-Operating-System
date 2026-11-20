import qrcode from 'qrcode-generator';

/**
 * The modules of a QR code for `text`, as rows of booleans (true is dark). Error correction M,
 * byte mode, the smallest version that fits. Drawn here in the browser: the server sends no image,
 * and the content security policy allows no `data:` images, so it is an inline SVG.
 */
export function qrModules(text: string): boolean[][] {
  const qr = qrcode(0, 'M');
  qr.addData(text, 'Byte');
  qr.make();
  const n = qr.getModuleCount();
  return Array.from({ length: n }, (_, r) => Array.from({ length: n }, (_, c) => qr.isDark(r, c)));
}

/** One path for every dark module, so the drawing is a single element. */
export function qrPath(modules: boolean[][], quiet: number): string {
  let d = '';
  modules.forEach((row, r) =>
    row.forEach((dark, c) => {
      if (dark) d += `M${c + quiet} ${r + quiet}h1v1h-1z`;
    }),
  );
  return d;
}

/**
 * The QR code, dark on white whatever the theme (scanners expect it), with the four-module quiet
 * zone the standard asks for. `label` names it for screen readers; the key is shown as text beside it.
 */
export function QrCode({ text, label, size = 200 }: { text: string; label: string; size?: number }) {
  const modules = qrModules(text);
  const quiet = 4;
  const n = modules.length + quiet * 2;
  return (
    <svg
      className="qr"
      role="img"
      aria-label={label}
      width={size}
      height={size}
      viewBox={`0 0 ${n} ${n}`}
      shapeRendering="crispEdges"
      data-modules={modules.length}
    >
      <rect width={n} height={n} fill="#ffffff" />
      <path d={qrPath(modules, quiet)} fill="#000000" />
    </svg>
  );
}
