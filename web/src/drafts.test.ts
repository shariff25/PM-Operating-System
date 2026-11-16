import { describe, expect, it } from 'vitest';
import { draftWhere } from './drafts';

describe('draftWhere', () => {
  it('names the kind, and the channel for a Slack draft whose title carries one', () => {
    expect(draftWhere({ kind: 'slack_reply', title: 'Reply to Ben in #partner-pricing' })).toEqual({ kind: 'slack_reply', channel: '#partner-pricing' });
    expect(draftWhere({ kind: 'slack_message', title: 'Heads up for #eng_alerts today' }).channel).toBe('#eng_alerts');
  });

  it('does not invent a channel', () => {
    expect(draftWhere({ kind: 'slack_reply', title: 'Reply to Ben' }).channel).toBeNull();
    expect(draftWhere({ kind: 'confluence_page', title: 'Notes from #billing' }).channel).toBeNull();
    expect(draftWhere({ kind: 'jira_issue', title: 'Fix C#' }).channel).toBeNull();
  });
});
