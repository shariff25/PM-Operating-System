import type { CSSProperties, ReactNode } from 'react';
import { COMPACT_MAX } from './Nudge';

/**
 * The bot family's mascots, ported from the approved art (`docs/design/bots/generate.py`): the
 * same shapes and props, as inline SVG. Body colours are the brand colours and come from CSS
 * custom properties (`--mascot-<bot>`, `-d` for the shade, `-l` for the highlight); the outline
 * details that sit directly on the page (prop outlines, shoes, the arrow) use `--mascot-line`,
 * which is lighter in the dark theme. Eyes and mouths always stay dark: they sit on white or on
 * the body colour.
 *
 * At 40 px and below a bot is the face only; above that, the full figure with its prop. Bot keys
 * will come from server data, so an unknown key draws a neutral tile instead of throwing.
 * Always decorative unless a `title` is given.
 */
export const MASCOT_BOTS = ['nudge', 'chief', 'sprout', 'echo'] as const;
export type MascotBot = (typeof MASCOT_BOTS)[number];

export function isMascotBot(bot: string): bot is MascotBot {
  return (MASCOT_BOTS as readonly string[]).includes(bot);
}

const FACE = 'var(--mascot-face-ink)';
const LINE = 'var(--mascot-line)';
const PINK = '#FF9DB0';

type Eye = 'open' | 'side' | 'happy';
type Mouth = 'smile' | 'open' | 'smirk';

/** The expressions each bot's full figure wears in the marketplace art. */
const FULL_FACE: Record<MascotBot, [Eye, Mouth]> = {
  nudge: ['open', 'open'],
  chief: ['open', 'smile'],
  sprout: ['happy', 'open'],
  echo: ['side', 'smirk'],
};

/** Each bot's full-figure view box (x, y, width, height); a body is 200 units tall. */
const FULL_BOX: Record<MascotBot, [number, number, number, number]> = {
  nudge: [-60, -40, 380, 290],
  chief: [-60, -40, 380, 290],
  sprout: [-60, -70, 380, 320],
  echo: [-60, -40, 380, 290],
};

function Eyes({ kind }: { kind: Eye }) {
  if (kind === 'happy') {
    return (
      <>
        <path d="M48 96 q22 -26 44 0" fill="none" stroke={FACE} strokeWidth={9} strokeLinecap="round" />
        <path d="M108 96 q22 -26 44 0" fill="none" stroke={FACE} strokeWidth={9} strokeLinecap="round" />
      </>
    );
  }
  const dx = kind === 'side' ? 9 : 0;
  return (
    <>
      {[70, 130].map((cx) => (
        <g key={cx}>
          <ellipse cx={cx} cy={90} rx={27} ry={31} fill="#fff" />
          <circle cx={cx + dx} cy={94} r={14} fill={FACE} />
          <circle cx={cx + dx + 5} cy={88} r={5} fill="#fff" />
        </g>
      ))}
    </>
  );
}

function MouthShape({ kind }: { kind: Mouth }) {
  if (kind === 'open') {
    return (
      <>
        <path d="M80 128 q20 30 40 0 z" fill={FACE} />
        <path d="M90 141 q10 8 20 0 q-10 -7 -20 0z" fill={PINK} />
      </>
    );
  }
  const d = kind === 'smile' ? 'M84 132 q16 16 32 0' : 'M92 134 q16 8 28 -8';
  return <path d={d} fill="none" stroke={FACE} strokeWidth={7} strokeLinecap="round" />;
}

/** The sticky-note body: shade, folded corner, blush and face (`face()` in generate.py). */
function Face({ bot, eye, mouth }: { bot: MascotBot; eye: Eye; mouth: Mouth }) {
  return (
    <>
      <rect width={200} height={200} rx={58} style={{ fill: `var(--mascot-${bot})` }} />
      <path d="M0 142 a58 58 0 0 0 58 58 h84 a58 58 0 0 0 58 -58 v-18 q-100 46 -200 0z" style={{ fill: `var(--mascot-${bot}-d)` }} opacity={0.35} />
      <path d="M144 0 l56 56 h-34 a22 22 0 0 1 -22 -22z" style={{ fill: `var(--mascot-${bot}-l)` }} />
      <path d="M150 0 h-6 l56 56 v-6 a50 50 0 0 0 -50 -50z" style={{ fill: 'var(--mascot-fold)' }} />
      <ellipse cx={40} cy={124} rx={15} ry={9} fill={PINK} opacity={0.75} />
      <ellipse cx={160} cy={124} rx={15} ry={9} fill={PINK} opacity={0.75} />
      <Eyes kind={eye} />
      <MouthShape kind={mouth} />
    </>
  );
}

function Figure({ bot }: { bot: MascotBot }) {
  const c = `var(--mascot-${bot})`;
  const d = `var(--mascot-${bot}-d)`;
  const l = `var(--mascot-${bot}-l)`;
  const arm = (path: string) => <path d={path} fill="none" style={{ stroke: d }} strokeWidth={16} strokeLinecap="round" />;
  const hand = (x: number, y: number) => <circle cx={x} cy={y} r={12} style={{ fill: d }} />;
  const line: CSSProperties = { stroke: LINE };
  const lineFill: CSSProperties = { fill: LINE };
  const legs = (
    <>
      <path d="M72 196 v26 M128 196 v26" style={{ stroke: d }} strokeWidth={16} strokeLinecap="round" />
      <ellipse className="mascot-shoe" cx={66} cy={226} rx={20} ry={10} />
      <ellipse className="mascot-shoe" cx={134} cy={226} rx={20} ry={10} />
    </>
  );
  let pre: ReactNode = null;
  let post: ReactNode = null;
  switch (bot) {
    case 'nudge':
      pre = (
        <>
          {arm('M4 120 q-30 16 -34 44')}
          {hand(-30, 166)}
          {arm('M196 104 q34 -20 38 -62')}
          {hand(234, 40)}
        </>
      );
      post = (
        <>
          <path d="M58 176 q42 26 84 0" fill="none" stroke="#fff" strokeWidth={5} strokeLinecap="round" />
          <rect x={84} y={182} width={32} height={40} rx={6} fill="#fff" style={line} strokeWidth={4} />
          <rect x={91} y={190} width={18} height={5} rx={2.5} style={{ fill: c }} />
          <rect x={91} y={200} width={18} height={4} rx={2} fill="#C9CEC9" />
        </>
      );
      break;
    case 'chief':
      pre = (
        <>
          {arm('M4 120 q-30 16 -30 40')}
          {hand(-26, 162)}
          {arm('M196 124 q22 14 14 40')}
          {hand(210, 166)}
        </>
      );
      post = (
        <>
          <path d="M100 150 l-13 12 l13 44 l13 -44z" fill="#F5B942" style={line} strokeWidth={4} strokeLinejoin="round" />
          <path d="M88 148 h24 l-5 14 h-14z" fill="#F5B942" style={line} strokeWidth={4} strokeLinejoin="round" />
          <g transform="translate(178 128) rotate(8)">
            <rect width={64} height={84} rx={8} fill="#fff" style={line} strokeWidth={5} />
            <rect x={20} y={-8} width={24} height={14} rx={5} style={lineFill} />
            <path d="M12 26 l6 6 l10 -12 M12 46 l6 6 l10 -12" fill="none" style={{ stroke: c }} strokeWidth={5} strokeLinecap="round" strokeLinejoin="round" />
            <path d="M36 28 h16 M36 48 h16 M14 66 h38" stroke="#C9CEC9" strokeWidth={5} strokeLinecap="round" />
          </g>
        </>
      );
      break;
    case 'sprout':
      pre = (
        <>
          {arm('M4 124 q-26 18 -26 44')}
          {hand(-22, 170)}
          {arm('M196 108 q30 -14 40 -50')}
          {hand(236, 56)}
          <path d="M100 4 v-40" style={{ stroke: d }} strokeWidth={9} strokeLinecap="round" />
          <path d="M100 -30 q-34 -30 -52 -4 q26 22 52 4z" style={{ fill: l, stroke: d }} strokeWidth={5} strokeLinejoin="round" />
          <path d="M100 -34 q30 -34 54 -10 q-24 26 -54 10z" style={{ fill: l, stroke: d }} strokeWidth={5} strokeLinejoin="round" />
        </>
      );
      post = (
        <g transform="translate(214 -30)">
          <path d="M0 70 l22 -28 l16 12 l26 -40" fill="none" style={line} strokeWidth={8} strokeLinecap="round" strokeLinejoin="round" />
          <path d="M48 8 l20 2 l-4 20" fill="none" style={line} strokeWidth={8} strokeLinecap="round" strokeLinejoin="round" />
        </g>
      );
      break;
    case 'echo':
      pre = (
        <>
          {arm('M4 124 q-28 18 -26 46')}
          {hand(-22, 172)}
          {arm('M196 116 q24 0 36 -10')}
        </>
      );
      post = (
        <>
          <g transform="translate(214 62) rotate(-14)">
            <path d="M0 26 l70 -30 v84 l-70 -30z" fill="#fff" style={line} strokeWidth={6} strokeLinejoin="round" />
            <rect x={-18} y={22} width={22} height={34} rx={6} style={lineFill} />
            <path d="M84 12 q14 26 0 52 M98 0 q22 38 0 76" fill="none" style={{ stroke: d }} strokeWidth={7} strokeLinecap="round" />
          </g>
          {hand(226, 108)}
        </>
      );
      break;
  }
  const [eye, mouth] = FULL_FACE[bot];
  return (
    <>
      {legs}
      {pre}
      <Face bot={bot} eye={eye} mouth={mouth} />
      {post}
    </>
  );
}

/**
 * A bot mascot. `size` is the height of the body (the sticky note itself) in px, as for `Nudge`;
 * a full figure is larger than that for its arms, legs and prop. Decorative (`aria-hidden`)
 * unless `title` is given.
 */
export function Mascot({ bot, size = 32, title, className }: { bot: string; size?: number; title?: string; className?: string }) {
  const known = isMascotBot(bot);
  const compact = size <= COMPACT_MAX;
  const [x, y, w, h] = !known || compact ? ([-4, -4, 208, 208] as const) : FULL_BOX[bot];
  const scale = size / 200;
  const a11y = title ? ({ role: 'img' } as const) : ({ 'aria-hidden': true } as const);
  return (
    <svg
      className={`mascot mascot-${known ? bot : 'unknown'} ${compact ? 'mascot-compact' : ''} ${className ?? ''}`.trim()}
      viewBox={`${x} ${y} ${w} ${h}`}
      width={Math.round(w * scale)}
      height={Math.round(h * scale)}
      focusable="false"
      data-mascot={known ? bot : 'unknown'}
      {...a11y}
    >
      {title ? <title>{title}</title> : null}
      {!known ? (
        <rect className="mascot-tile" width={200} height={200} rx={58} />
      ) : compact ? (
        <Face bot={bot} eye="open" mouth="smile" />
      ) : (
        <Figure bot={bot} />
      )}
    </svg>
  );
}
