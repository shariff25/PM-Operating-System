import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { renderApp } from '../test/util';

const WITH_BLOCK = 'Here is your brief.\n\n```pmos-brief\n{"secret_machine_field": 1}\n```\n';

describe('the brief machine block never reaches the screen', () => {
  it('is gone from a task thread', async () => {
    renderApp({
      hash: '/a/tsk_seed_week',
      wrap: (api) => {
        const get = api.getTask.bind(api);
        api.getTask = async (id) => ({ ...(await get(id)), result: WITH_BLOCK });
        return api;
      },
    });
    expect(await screen.findByText('Here is your brief.')).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('secret_machine_field');
    expect(document.body.textContent).not.toContain('pmos-brief');
  });

  it('is gone from an unclosed block and from the failure message in the activity list', async () => {
    renderApp({
      hash: '/activity',
      wrap: (api) => {
        const list = api.listTasks.bind(api);
        api.listTasks = async () =>
          (await list()).map((t) =>
            t.status === 'failed' ? { ...t, error: 'It broke.\n```pmos-brief\n{"secret_machine_field": 1}' } : t,
          );
        return api;
      },
    });
    expect(await screen.findByText('It broke.')).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('secret_machine_field');
  });
});
