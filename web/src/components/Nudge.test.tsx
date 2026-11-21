import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { Nudge } from './Nudge';
import type { NudgePose } from './Nudge';

const POSES: NudgePose[] = ['hello', 'nudge', 'yay', 'calm'];

describe('Nudge', () => {
  it.each(POSES)('draws the %s pose, decorative only', (pose) => {
    const { container } = render(<Nudge pose={pose} size={64} />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveAttribute('aria-hidden', 'true');
    expect(svg).toHaveAttribute('focusable', 'false');
    expect(svg).toHaveAttribute('data-nudge', pose);
    expect(svg.querySelector('title')).toBeNull();
    // the full pose has the lanyard's badge
    expect(svg.querySelector('rect[width="32"][height="40"]')).not.toBeNull();
  });

  it('scales by the body: size is the sticky note’s height in px', () => {
    const { container } = render(<Nudge pose="hello" size={100} />);
    const svg = container.querySelector('svg')!;
    const [, , w, h] = svg.getAttribute('viewBox')!.split(' ').map(Number);
    expect(Number(svg.getAttribute('width'))).toBe(Math.round(w! / 2));
    expect(Number(svg.getAttribute('height'))).toBe(Math.round(h! / 2));
  });

  it.each(POSES)('at 40 px and below, draws the %s face only: no limbs, legs, lanyard or props', (pose) => {
    const { container } = render(<Nudge pose={pose} size={24} />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveClass('nudge-compact');
    expect(svg).toHaveAttribute('width', '25');
    expect(svg.querySelector('rect[width="32"][height="40"]')).toBeNull();
    expect(svg.querySelectorAll('.nudge-shoe, .nudge-shoe-line')).toHaveLength(0);
    expect(svg.querySelector('circle[r="12"]')).toBeNull();
  });
});
