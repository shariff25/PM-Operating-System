import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { BotBadge } from './BotBadge';
import { MASCOT_BOTS, Mascot } from './Mascot';

/** What only a full figure draws: shoes and hands. */
const FULL_ONLY = '.mascot-shoe, circle[r="12"]';

describe('Mascot', () => {
  it.each(MASCOT_BOTS)('draws %s as a full figure, decorative only', (bot) => {
    const { container } = render(<Mascot bot={bot} size={120} />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveAttribute('aria-hidden', 'true');
    expect(svg).toHaveAttribute('focusable', 'false');
    expect(svg).toHaveAttribute('data-mascot', bot);
    expect(svg).not.toHaveClass('mascot-compact');
    expect(svg.querySelector('title')).toBeNull();
    expect(svg.querySelectorAll('.mascot-shoe')).toHaveLength(2);
    expect(svg.querySelectorAll('circle[r="12"]').length).toBeGreaterThanOrEqual(1);
  });

  it('draws each bot’s own prop', () => {
    const draw = (bot: string) => render(<Mascot bot={bot} size={120} />).container.querySelector('svg')!;
    expect(draw('nudge').querySelector('rect[width="32"][height="40"]')).not.toBeNull(); // badge
    expect(draw('chief').querySelector('rect[width="64"][height="84"]')).not.toBeNull(); // clipboard
    expect(draw('sprout').querySelector('path[d^="M0 70"]')).not.toBeNull(); // rising arrow
    expect(draw('echo').querySelector('path[d^="M0 26 l70"]')).not.toBeNull(); // megaphone
  });

  it.each(MASCOT_BOTS)('at 40 px and below, draws the %s face only: no limbs, legs or prop', (bot) => {
    const { container } = render(<Mascot bot={bot} size={40} />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveClass('mascot-compact');
    expect(svg).toHaveAttribute('width', '42');
    expect(svg.querySelectorAll(FULL_ONLY)).toHaveLength(0);
    expect(svg.querySelectorAll('g[transform]')).toHaveLength(0);
    expect(svg.querySelector('rect[width="32"][height="40"]')).toBeNull();
    expect(svg.querySelectorAll('rect')).toHaveLength(1); // the body only
  });

  it('scales by the body: size is the sticky note’s height in px', () => {
    const { container } = render(<Mascot bot="echo" size={100} />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveAttribute('width', '190');
    expect(svg).toHaveAttribute('height', '145');
  });

  it('draws a neutral placeholder tile for an unknown bot, without throwing', () => {
    for (const key of ['', 'grok', '__proto__', 'constructor', '<img src=x>']) {
      for (const size of [24, 120]) {
        const { container, unmount } = render(<Mascot bot={key} size={size} />);
        const svg = container.querySelector('svg')!;
        expect(svg).toHaveAttribute('data-mascot', 'unknown');
        expect(svg).toHaveAttribute('aria-hidden', 'true');
        expect(svg.querySelectorAll('.mascot-tile')).toHaveLength(1);
        expect(svg.querySelectorAll('ellipse, circle, path')).toHaveLength(0);
        expect(container.querySelector('img')).toBeNull();
        unmount();
      }
    }
  });

  it('is exposed to assistive tech only when given a title', () => {
    render(<Mascot bot="chief" size={32} title="Chief of Staff" />);
    const img = screen.getByRole('img', { name: 'Chief of Staff' });
    expect(img).not.toHaveAttribute('aria-hidden');
  });
});

describe('BotBadge', () => {
  it('shows the small face and the name', () => {
    const { container } = render(<BotBadge bot="chief" name="Chief" />);
    expect(screen.getByText('Chief')).toBeInTheDocument();
    expect(container.querySelector('svg')).toHaveClass('mascot-compact');
    expect(container.querySelector('svg')).toHaveAttribute('data-mascot', 'chief');
  });

  it('renders a hostile name as text, not markup', () => {
    const name = '<img src=x onerror=alert(1)><script>alert(2)</script>';
    const { container } = render(<BotBadge bot="echo" name={name} />);
    expect(screen.getByText(name)).toBeInTheDocument();
    expect(container.querySelector('img, script')).toBeNull();
    expect(container.querySelector('.bot-badge-name')!.textContent).toBe(name);
  });

  it('copes with an unknown bot', () => {
    const { container } = render(<BotBadge bot="nope" name="Mystery" />);
    expect(screen.getByText('Mystery')).toBeInTheDocument();
    expect(container.querySelector('svg')).toHaveAttribute('data-mascot', 'unknown');
  });
});
