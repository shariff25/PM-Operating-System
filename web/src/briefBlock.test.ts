import { describe, expect, it } from 'vitest';
import type { Task } from './api';
import { cleanTask, stripBriefBlock } from './briefBlock';

const JSON_BLOCK = '{"changes":[{"id":"PAY-1"}]}';

describe('stripBriefBlock', () => {
  it('leaves text without the block exactly as it was', () => {
    const t = 'Hello\n\n```js\nconst a = 1;\n```\n\nBye';
    expect(stripBriefBlock(t)).toBe(t);
  });

  it('removes a closed block at the end and the blank lines before it', () => {
    expect(stripBriefBlock(`Summary\n\n\`\`\`pmos-brief\n${JSON_BLOCK}\n\`\`\`\n`)).toBe('Summary');
  });

  it('removes a block in the middle and keeps the text around it', () => {
    expect(stripBriefBlock(`A\n\n\`\`\`pmos-brief\n${JSON_BLOCK}\n\`\`\`\n\nB`)).toBe('A\n\n\nB');
  });

  it('removes an indented block', () => {
    expect(stripBriefBlock(`Summary\n\n   \`\`\`pmos-brief\n   ${JSON_BLOCK}\n   \`\`\``)).toBe('Summary');
  });

  it('removes a block that is never closed, to the end of the text', () => {
    expect(stripBriefBlock(`Summary\n\n\`\`\`pmos-brief\n${JSON_BLOCK}\nmore`)).toBe('Summary');
  });

  it('handles tildes and an info string with extra words', () => {
    expect(stripBriefBlock(`X\n~~~pmos-brief\n${JSON_BLOCK}\n~~~`)).toBe('X');
    expect(stripBriefBlock(`X\n\`\`\`pmos-brief v1\n${JSON_BLOCK}\n\`\`\`\nY`)).toBe('X\nY');
  });

  it('does not end on a shorter or different fence', () => {
    expect(stripBriefBlock(`X\n\`\`\`\`pmos-brief\n\`\`\`\n~~~\n${JSON_BLOCK}\n\`\`\`\`\nY`)).toBe('X\nY');
  });

  it('removes more than one block and works with Windows line ends', () => {
    expect(stripBriefBlock('A\r\n```pmos-brief\r\n1\r\n```\r\nB\r\n```pmos-brief\r\n2')).toBe('A\nB');
  });

  it('keeps other fenced blocks and text that only mentions the name', () => {
    const t = 'See pmos-brief here\n\n```json\n{"a":1}\n```';
    expect(stripBriefBlock(t)).toBe(t);
    expect(stripBriefBlock('```pmos-briefs\nx\n```')).toBe('```pmos-briefs\nx\n```');
  });

  it('returns an empty string when the block is all there was', () => {
    expect(stripBriefBlock('```pmos-brief\n{}\n```')).toBe('');
  });
});

describe('cleanTask', () => {
  const task = (over: Partial<Task>): Task => ({
    id: 't', bot: 'b', text: 'ask', status: 'done', result: null, error: null, draft_ids: [],
    created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z', progress: null, ...over,
  });

  it('cleans the result, the error and the steps', () => {
    const t = cleanTask(
      task({
        result: 'Ok\n```pmos-brief\n{}\n```',
        error: 'Bad\n```pmos-brief\n{}',
        events: [{ at: 'x', kind: 'milestone', text: 'Step\n```pmos-brief\n{}\n```' }],
      }),
    );
    expect(t.result).toBe('Ok');
    expect(t.error).toBe('Bad');
    expect(t.events?.[0]?.text).toBe('Step');
  });

  it('turns a result that was only the block into no result', () => {
    expect(cleanTask(task({ result: '```pmos-brief\n{}\n```' })).result).toBeNull();
  });
});
