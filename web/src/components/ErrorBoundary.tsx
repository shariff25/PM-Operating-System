import { Component } from 'react';
import type { ReactNode } from 'react';
import { copy } from '../copy';

/** One plain message instead of a blank page when a screen throws. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidUpdate(prev: { resetKey: string }) {
    if (this.state.failed && prev.resetKey !== this.props.resetKey) this.setState({ failed: false });
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="note note-error" role="alert">
        <p>{copy.errors.screenBroke}</p>
        <a href="#/" className="btn">{copy.draft.back}</a>
      </div>
    );
  }
}
