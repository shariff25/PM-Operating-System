import { useContext } from 'react';
import type { ReactNode } from 'react';
import { AppContext } from '../app-context';

/**
 * Nudge, the PM OS mascot, drawn from the approved art (`docs/design/nudge/generate.py`): the
 * same 200-unit body, eyes, mouths, arms and props, as inline SVG. The picture never carries
 * meaning on its own: it is always `aria-hidden`, and the text beside it says what matters.
 *
 * Poses: `hello` (onboarding), `nudge` (hints and reminders), `yay` (a draft was approved) and
 * `calm` (nothing needs you). At 40 px and below Nudge is drawn as the sheet's icon row draws
 * it: the face only, without limbs, legs, lanyard or props.
 */
export type NudgePose = 'hello' | 'nudge' | 'yay' | 'calm';

const G = '#1F8A5B';
const GD = '#156B45';
const GL = '#7FD3A8';
const INK = '#10231A';
const PINK = '#FF9DB0';
const CREAM = '#F6F5F0';

type EyeKind = 'open' | 'side' | 'sleepy' | 'happy';
type MouthKind = 'smile' | 'open' | 'smirk' | 'tiny';

function Eyes({ kind }: { kind: EyeKind }) {
  if (kind === 'happy') {
    return (
      <>
        <path d="M48 96 q22 -26 44 0" fill="none" stroke={INK} strokeWidth={9} strokeLinecap="round" />
        <path d="M108 96 q22 -26 44 0" fill="none" stroke={INK} strokeWidth={9} strokeLinecap="round" />
      </>
    );
  }
  const [dx, dy, lid]: [number, number, boolean] = { open: [0, 0, false], side: [9, 2, false], sleepy: [0, 6, true] }[kind] as [number, number, boolean];
  return (
    <>
      {[70, 130].map((cx) => (
        <g key={cx}>
          <ellipse cx={cx} cy={90} rx={27} ry={31} fill="#fff" />
          <circle cx={cx + dx} cy={94 + dy} r={14} fill={INK} />
          <circle cx={cx + dx + 5} cy={88 + dy} r={5} fill="#fff" />
          {lid && (
            <>
              <path d={`M${cx - 28} 90 a27 31 0 0 1 56 0 z`} fill={GD} />
              <path d={`M${cx - 28} 90 h56`} stroke={INK} strokeWidth={5} strokeLinecap="round" />
            </>
          )}
        </g>
      ))}
    </>
  );
}

function Mouth({ kind }: { kind: MouthKind }) {
  switch (kind) {
    case 'smile':
      return <path d="M84 132 q16 16 32 0" fill="none" stroke={INK} strokeWidth={7} strokeLinecap="round" />;
    case 'open':
      return (
        <>
          <path d="M80 128 q20 30 40 0 z" fill={INK} />
          <path d="M90 141 q10 8 20 0 q-10 -7 -20 0z" fill={PINK} />
        </>
      );
    case 'smirk':
      return <path d="M92 134 q16 8 28 -8" fill="none" stroke={INK} strokeWidth={7} strokeLinecap="round" />;
    case 'tiny':
      return <ellipse cx={100} cy={136} rx={7} ry={5} fill={INK} />;
  }
}

/** The sticky-note body with its folded corner, blush and face; the lanyard and badge when `lanyard`. */
function Body({ eyes, mouth, brows, lanyard }: { eyes: EyeKind; mouth: MouthKind; brows?: ReactNode; lanyard: boolean }) {
  return (
    <>
      <rect x={0} y={0} width={200} height={200} rx={58} fill={G} />
      <path d="M0 142 v0 a58 58 0 0 0 58 58 h84 a58 58 0 0 0 58 -58 v-18 q-100 46 -200 0z" fill={GD} opacity={0.35} />
      <path d="M150 0 h-6 l56 56 v-6 a50 50 0 0 0 -50 -50z" fill={CREAM} />
      <path d="M144 0 l56 56 h-34 a22 22 0 0 1 -22 -22z" fill={GL} />
      <ellipse cx={40} cy={124} rx={15} ry={9} fill={PINK} opacity={0.75} />
      <ellipse cx={160} cy={124} rx={15} ry={9} fill={PINK} opacity={0.75} />
      <Eyes kind={eyes} />
      {brows}
      <Mouth kind={mouth} />
      {lanyard && (
        <>
          <path d="M58 176 q42 26 84 0" fill="none" stroke="#fff" strokeWidth={5} strokeLinecap="round" opacity={0.9} />
          <rect x={84} y={182} width={32} height={40} rx={6} fill="#fff" stroke={INK} strokeWidth={4} />
          <rect x={91} y={190} width={18} height={5} rx={2.5} fill={G} />
          <rect x={91} y={200} width={18} height={4} rx={2} fill="#C9CEC9" />
          <rect x={91} y={208} width={12} height={4} rx={2} fill="#C9CEC9" />
        </>
      )}
    </>
  );
}

function Arm({ d }: { d: string }) {
  return <path d={d} fill="none" stroke={GD} strokeWidth={16} strokeLinecap="round" strokeLinejoin="round" />;
}

function Hand({ x, y }: { x: number; y: number }) {
  return <circle cx={x} cy={y} r={12} fill={GD} />;
}

/** Standing legs. The shoes take a theme colour, so they stay visible on a dark page. */
function Legs() {
  return (
    <>
      <path d="M72 196 v26" stroke={GD} strokeWidth={16} strokeLinecap="round" />
      <path d="M128 196 v26" stroke={GD} strokeWidth={16} strokeLinecap="round" />
      <ellipse className="nudge-shoe" cx={66} cy={226} rx={20} ry={10} fill={INK} />
      <ellipse className="nudge-shoe" cx={134} cy={226} rx={20} ry={10} fill={INK} />
    </>
  );
}

const BROWS = (
  <>
    <path d="M46 52 q22 -14 46 -4" fill="none" stroke={INK} strokeWidth={7} strokeLinecap="round" />
    <path d="M110 44 q22 -10 44 6" fill="none" stroke={INK} strokeWidth={7} strokeLinecap="round" />
  </>
);

/** Each full pose and the box it fits in (x, y, width, height in body units). */
const POSES: Record<NudgePose, { box: [number, number, number, number]; art: ReactNode }> = {
  hello: {
    box: [-46, -16, 330, 256],
    art: (
      <>
        <Legs />
        <Arm d="M4 120 q-30 16 -34 44" />
        <Hand x={-30} y={166} />
        <Arm d="M196 104 q34 -20 38 -62" />
        <Hand x={234} y={40} />
        <Body eyes="open" mouth="open" lanyard />
        <path d="M252 18 l10 -12 M262 40 l16 -4 M240 6 l2 -16" stroke={G} strokeWidth={6} strokeLinecap="round" />
      </>
    ),
  },
  nudge: {
    box: [-30, -16, 342, 262],
    art: (
      <g transform="rotate(-7 100 110)">
        <Legs />
        <Arm d="M4 124 q-28 20 -24 50" />
        <Hand x={-20} y={176} />
        <Arm d="M196 118 q40 4 66 -6" />
        <Hand x={264} y={111} />
        <path d="M264 111 h34" stroke={GD} strokeWidth={11} strokeLinecap="round" />
        <Body eyes="side" mouth="smirk" brows={BROWS} lanyard />
      </g>
    ),
  },
  yay: {
    box: [-52, 12, 304, 228],
    art: (
      <>
        <Legs />
        <Arm d="M6 96 q-34 -24 -40 -66" />
        <Hand x={-36} y={28} />
        <Arm d="M194 96 q34 -24 40 -66" />
        <Hand x={236} y={28} />
        <Body eyes="happy" mouth="open" lanyard />
      </>
    ),
  },
  calm: {
    box: [-22, -6, 286, 236],
    art: (
      <>
        <Arm d="M4 128 q-22 26 6 44" />
        <Hand x={12} y={172} />
        <Arm d="M196 128 q26 22 0 40" />
        <Hand x={196} y={170} />
        <Body eyes="sleepy" mouth="tiny" lanyard />
        <g transform="translate(176 138)">
          <path d="M54 14 q26 4 22 26 q-4 16 -24 14" fill="none" stroke={INK} strokeWidth={8} />
          <rect x={0} y={0} width={58} height={62} rx={12} fill="#fff" stroke={INK} strokeWidth={5} />
          <path d="M12 -10 q-8 -12 2 -22 q8 -10 0 -22 M34 -10 q-8 -12 2 -22 q8 -10 0 -22" className="nudge-steam" fill="none" stroke="#B9C2BC" strokeWidth={5} strokeLinecap="round" />
          <path d="M20 22 l8 8 l14 -16" fill="none" stroke={G} strokeWidth={6} strokeLinecap="round" strokeLinejoin="round" />
        </g>
        <path className="nudge-shoe-line" d="M34 196 q-26 10 -40 0 M166 196 q26 10 40 0" fill="none" stroke={INK} strokeWidth={14} strokeLinecap="round" />
      </>
    ),
  },
};

/** The face-only icon of each pose, as the sheet's "In the app" row draws it. */
const FACES: Record<NudgePose, { eyes: EyeKind; mouth: MouthKind }> = {
  hello: { eyes: 'open', mouth: 'smile' },
  nudge: { eyes: 'side', mouth: 'smirk' },
  yay: { eyes: 'happy', mouth: 'smile' },
  calm: { eyes: 'sleepy', mouth: 'tiny' },
};

/** At this size and below, Nudge is the face only. */
export const COMPACT_MAX = 40;

/**
 * Nudge. `size` is the height of the body (the sticky note itself) in px; a full pose is a little
 * larger than that, for its arms, legs and props. Always decorative.
 */
export function Nudge({ pose, size = 32, className }: { pose: NudgePose; size?: number; className?: string }) {
  const compact = size <= COMPACT_MAX;
  const [x, y, w, h] = compact ? [-4, -4, 208, 208] : POSES[pose].box;
  const scale = size / 200;
  return (
    <svg
      className={`nudge nudge-${pose} ${compact ? 'nudge-compact' : ''} ${className ?? ''}`}
      viewBox={`${x} ${y} ${w} ${h}`}
      width={Math.round(w * scale)}
      height={Math.round(h * scale)}
      aria-hidden="true"
      focusable="false"
      data-nudge={pose}
    >
      {compact ? <Body eyes={FACES[pose].eyes} mouth={FACES[pose].mouth} lanyard={false} /> : POSES[pose].art}
    </svg>
  );
}

/**
 * Whether to draw Nudge: the person's "Show Nudge" setting (on by default; an older server
 * leaves it out, which means on). Outside the signed-in app (sign-in) it is on.
 */
export function useShowNudge(): boolean {
  const ctx = useContext(AppContext);
  return ctx ? ctx.me.mascot !== false : true;
}
