import { renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { pollDelay, usePoll } from './polling';

describe('pollDelay', () => {
  it('is 600 ms for the first 6 seconds, then 2 seconds, then 5 seconds after a minute', () => {
    expect(pollDelay(0)).toBe(600);
    expect(pollDelay(5_999)).toBe(600);
    expect(pollDelay(6_000)).toBe(2_000);
    expect(pollDelay(59_999)).toBe(2_000);
    expect(pollDelay(60_000)).toBe(5_000);
    expect(pollDelay(10 * 60_000)).toBe(5_000);
  });
});

describe('usePoll', () => {
  let hidden = false;
  beforeEach(() => {
    vi.useFakeTimers();
    hidden = false;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
  });
  afterEach(() => {
    vi.useRealTimers();
    delete (document as { hidden?: unknown }).hidden;
  });

  it('follows the schedule while active', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    renderHook(() => usePoll(true, tick));
    await vi.advanceTimersByTimeAsync(599);
    expect(tick).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(tick).toHaveBeenCalledTimes(1);
    // 600 ms apart for the first six seconds
    await vi.advanceTimersByTimeAsync(5_400);
    expect(tick.mock.calls.length).toBe(10);
    // then 2 seconds apart
    tick.mockClear();
    await vi.advanceTimersByTimeAsync(4_000);
    expect(tick).toHaveBeenCalledTimes(2);
  });

  it('slows to 5 seconds after a minute', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    renderHook(() => usePoll(true, tick));
    await vi.advanceTimersByTimeAsync(61_000);
    tick.mockClear();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(tick).toHaveBeenCalledTimes(2);
  });

  it('does not poll when not active', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    renderHook(() => usePoll(false, tick));
    await vi.advanceTimersByTimeAsync(10_000);
    expect(tick).not.toHaveBeenCalled();
  });

  it('stops while the tab is hidden and checks at once when it is visible again', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    renderHook(() => usePoll(true, tick));
    await vi.advanceTimersByTimeAsync(700);
    expect(tick).toHaveBeenCalledTimes(1);
    hidden = true;
    await vi.advanceTimersByTimeAsync(700); // a call already scheduled may finish; then nothing more
    tick.mockClear();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(tick).not.toHaveBeenCalled();
    hidden = false;
    document.dispatchEvent(new Event('visibilitychange'));
    await vi.advanceTimersByTimeAsync(0);
    expect(tick).toHaveBeenCalledTimes(1);
  });

  it('checks again on window focus', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    renderHook(() => usePoll(true, tick));
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);
    expect(tick).toHaveBeenCalledTimes(1);
  });

  it('stops when it becomes inactive', async () => {
    const tick = vi.fn().mockResolvedValue(undefined);
    const { rerender } = renderHook(({ on }) => usePoll(on, tick), { initialProps: { on: true } });
    await vi.advanceTimersByTimeAsync(700);
    rerender({ on: false });
    tick.mockClear();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(tick).not.toHaveBeenCalled();
  });
});
