import type { Api, Task } from './api';

/**
 * A daily brief answer may end with a fenced block whose info string is `pmos-brief`: JSON for
 * another service, not for the person. It is removed before anything is rendered.
 */
const OPEN = /^[ \t]*(`{3,}|~{3,})[ \t]*pmos-brief(?:[ \t].*)?$/;

/** Remove every `pmos-brief` fenced block: indented, tilde-fenced or never closed. */
export function stripBriefBlock(text: string): string {
  if (!/pmos-brief/.test(text)) return text;
  const out: string[] = [];
  let close: RegExp | null = null;
  for (const line of text.split(/\r\n|\r|\n/)) {
    if (close) {
      if (close.test(line)) close = null;
      continue;
    }
    const open = OPEN.exec(line);
    if (open) {
      const fence = open[1]!;
      // A closing fence is the same character, at least as long, with nothing after it.
      close = new RegExp(`^[ \\t]*\\${fence[0]}{${fence.length},}[ \\t]*$`);
      continue;
    }
    out.push(line);
  }
  return out.join('\n').replace(/\s+$/, '');
}

/** The task with the machine block gone from every text the bot wrote. */
export function cleanTask(task: Task): Task {
  return {
    ...task,
    result: task.result === null ? null : stripBriefBlock(task.result) || null,
    error: task.error === null ? null : stripBriefBlock(task.error) || null,
    events: task.events?.map((e) => ({ ...e, text: stripBriefBlock(e.text) })),
  };
}

/**
 * The one place bot text enters the app: every task the client returns is cleaned first.
 * The client is changed in place, so anything holding it keeps seeing the same object.
 */
export function withCleanTasks(api: Api): Api {
  const { createTask, getTask, listTasks, draftFollowup } = api;
  api.createTask = async (text) => cleanTask(await createTask.call(api, text));
  api.getTask = async (id) => cleanTask(await getTask.call(api, id));
  api.listTasks = async () => (await listTasks.call(api)).map(cleanTask);
  api.draftFollowup = async (id) => cleanTask(await draftFollowup.call(api, id));
  return api;
}
