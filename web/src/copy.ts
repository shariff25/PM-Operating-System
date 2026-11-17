// Every user-facing word in the app lives in this file, so it can be edited in one place.
// Tone: light and gently funny in greetings, empty states and hints. Errors, approvals and
// anything about security are plain.

import type { DraftKind, DraftStatus, HintName, RejectReason } from './api/types';

/**
 * What approving means for each kind of draft, in one place: the button, the hint, the dialog,
 * the success line. A Jira draft never says "page", and a Slack one never says "created".
 */
export interface ApproveWords {
  button: string;
  hint: string;
  hintShort: string;
  /** "This will create a Confluence page in Space PM, under your name (Ana)." */
  confirmText: (where: string, who: string) => string;
  confirmButton: string;
  confirming: string;
  /** "Nothing has been created until you press Confirm." */
  nothingYet: string;
  /** "Created in Space PM." or "Posted in #billing." */
  done: (where: string) => string;
  toast: string;
}

const APPROVE_WORDS: Record<DraftKind, ApproveWords> = {
  confluence_page: {
    button: 'Approve to Confluence',
    hint: "This creates the page in Confluence under your name. I'll wait until you're sure.",
    hintShort: 'Nothing is created until you approve.',
    confirmText: (where, who) => `This will create a Confluence page in ${where}, under your name (${who}).`,
    confirmButton: 'Confirm and create',
    confirming: 'Creating…',
    nothingYet: 'Nothing has been created until you press Confirm.',
    done: (where) => `Created in ${where}.`,
    toast: 'Approved. The page is in Confluence.',
  },
  jira_issue: {
    button: 'Approve to Jira',
    hint: "This creates the issue in Jira under your name. I'll wait until you're sure.",
    hintShort: 'Nothing is created until you approve.',
    confirmText: (where, who) => `This will create a Jira issue in ${where}, under your name (${who}).`,
    confirmButton: 'Confirm and create',
    confirming: 'Creating…',
    nothingYet: 'Nothing has been created until you press Confirm.',
    done: (where) => `Created in ${where}.`,
    toast: 'Approved. The issue is in Jira.',
  },
  slack_reply: {
    button: 'Approve to Slack',
    hint: "This posts the reply in Slack under your name. I'll wait until you're sure.",
    hintShort: 'Nothing posts until you approve.',
    confirmText: (where, who) => `This will post a Slack reply in ${where}, under your name (${who}).`,
    confirmButton: 'Confirm and post',
    confirming: 'Posting…',
    nothingYet: 'Nothing has been posted until you press Confirm.',
    done: (where) => `Posted in ${where}.`,
    toast: 'Approved. The reply is posted in Slack.',
  },
  slack_message: {
    button: 'Approve to Slack',
    hint: "This posts the message in Slack under your name. I'll wait until you're sure.",
    hintShort: 'Nothing posts until you approve.',
    confirmText: (where, who) => `This will post a Slack message in ${where}, under your name (${who}).`,
    confirmButton: 'Confirm and post',
    confirming: 'Posting…',
    nothingYet: 'Nothing has been posted until you press Confirm.',
    done: (where) => `Posted in ${where}.`,
    toast: 'Approved. The message is posted in Slack.',
  },
};

export const approveWords = (kind: DraftKind): ApproveWords => APPROVE_WORDS[kind];

export const copy = {
  appName: 'PM OS',

  // ---- Greetings, empty states and hints (the funny voice lives here) ----
  greeting: (name: string, hour: number) => {
    const part = hour < 5 ? 'Still up' : hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
    return `${part}, ${name}.`;
  },
  welcomeWithDrafts: "Welcome in. Your drafts beat you here.",
  welcomeNoDrafts:
    "No drafts yet, so here's one about me. Edit it, argue with it, press every button. Nothing leaves this page.",
  emptyDrafts: 'No drafts here. Ask me for one, or start one from Slack.',
  nothingWaiting: 'Nothing is waiting for you. Suspiciously calm.',
  nothingWaitedOn: 'You are not waiting on anyone. Enjoy it while it lasts.',
  modelBusy: "The model is catching its breath. I'm trying again, so you don't need to do anything.",
  doneFirstApproval: "Done. It's in Confluence, and it has your name on it, not mine.",
  /** One action per empty state; the line itself is above or in the page's own copy. */
  emptyActions: {
    askDraft: 'Ask for a draft',
    askDraftText: 'Draft a ',
    showAll: 'Show all drafts',
    askFirst: 'Ask your first question',
    askOpen: 'Ask what is open',
    askOpenText: 'What is waiting on me?',
  },
  noSources: 'I could not tell which sources this draft came from, so I am not going to pretend.',

  hints: {
    sources: 'Everything I claimed, and where I got it. Click a source to open it.',
    ask: 'Tell me what to fix, the way you\'d tell a colleague. "Shorter" works.',
    approve: (kind: DraftKind) => APPROVE_WORDS[kind].hint,
  } satisfies Record<HintName, string | ((kind: DraftKind) => string)>,
  /** One short line each, for the banner on a phone. */
  hintsShort: {
    sources: 'Sources shows where I got each claim.',
    ask: 'Ask for changes in plain words.',
    approve: (kind: DraftKind) => APPROVE_WORDS[kind].hintShort,
  } satisfies Record<HintName, string | ((kind: DraftKind) => string)>,
  hintLabel: 'Hint',
  hintDismiss: 'Got it',
  hintStep: (i: number, n: number) => `Tip ${i} of ${n}`,
  /** Hints come one at a time, in this order. */
  hintOrder: ['sources', 'ask', 'approve'] as const satisfies readonly HintName[],

  // ---- Text that was left out of the formatted view ----
  stripped: {
    warning: (n: number, what: string) =>
      `This ${what} contains ${n} hidden or stripped ${n === 1 ? 'part' : 'parts'}.`,
    why: 'These are not shown in the formatted text. Whatever is in the text is published under your name, so read them too.',
    show: 'Show them as text',
    hide: 'Hide them',
    listLabel: 'Hidden or stripped parts, as plain text',
    draft: 'draft',
    answer: 'answer',
  },

  // ---- Sign-in ----
  signIn: {
    title: 'Sign in to PM OS',
    lede: 'Use your work account. You only see your own drafts and requests.',
    tagline: 'Your product work, drafted and waiting for your yes.',
    points: [
      'Reads Jira, Confluence and Slack the way you would',
      'Drafts pages, issues and replies with their sources',
      'Nothing is sent anywhere until you approve it',
    ],
    handleLabel: 'Your handle',
    handleHint: 'Development sign-in. Use the handle from team.yaml, such as ana.',
    submit: 'Sign in',
    working: 'Signing in…',
    microsoft: 'Sign in with Microsoft',
    notAMember: (message: string) => message,
    retry: 'Check the handle and try again.',
    loading: 'Checking how you can sign in…',
    notSetUp: "Sign-in isn't set up yet.",
    devHeading: 'For development',
    devNote: 'Only on development setups. Use the handle from team.yaml.',
  },

  // ---- Signed out (where Microsoft sends people back to) ----
  signedOut: {
    title: 'You are signed out',
    tryAgain: 'Try again',
    reasons: {
      not_a_member: "You're not on the team list yet. Ask a platform owner to add you.",
      cancelled: 'Sign-in was cancelled.',
      expired: 'That sign-in expired. Try again.',
      unavailable: "Microsoft sign-in isn't answering. Try again in a minute.",
    } as Record<string, string>,
    failed: "Sign-in didn't work. Try again.",
  },

  // ---- Menu ----
  menu: {
    button: (name: string) => `Menu for ${name}`,
    nudges: 'Hints and nudges',
    nudgesOn: 'On',
    nudgesOff: 'Off',
    showHints: 'Show hints again',
    signOut: 'Sign out',
  },

  // ---- Home ----
  home: {
    askLabel: 'What do you need?',
    askPlaceholder: 'For example: draft a PRD for the grace period setting',
    askButton: 'Send',
    asking: 'Sending…',
    waitingOnYou: 'Waiting on you',
    youWaitOn: "You're waiting on",
    changed: 'Changed since yesterday',
    draftReply: 'Draft a reply',
    draftNudge: 'Draft a nudge',
    notMine: 'Not mine',
    ageDays: (n: number) => (n === 0 ? 'today' : n === 1 ? '1 day' : `${n} days`),
    openSource: 'Open source',
    draftsHeading: 'Your drafts',
    pendingBadge: 'Needs you',
    practiceBadge: 'Practice',
    expiresIn: (hours: number, minutes: number) =>
      hours >= 1 ? `Expires in ${hours} h` : `Expires in ${minutes} min`,
    expired: 'Expired',
    expiredRow: 'Needs a fresh draft',
    openDraft: 'Open draft',
    statusLabel: (s: DraftStatus) =>
      ({ pending: 'Needs you', approved: 'Approved', rejected: 'Rejected', expired: 'Expired', failed: 'Failed' })[s],
    kindLabel: (k: DraftKind) =>
      ({ confluence_page: 'Confluence page', jira_issue: 'Jira issue', slack_reply: 'Slack reply', slack_message: 'Slack message' })[k],
    loadingDrafts: 'Loading your drafts…',
  },

  // ---- Task progress ----
  task: {
    status: {
      queued: 'Waiting to start',
      working: 'Working on it',
      needs_approval: 'I made a draft for you. Nothing is sent until you approve it.',
      done: 'Done',
      failed: 'That did not work',
      cancelled: 'Cancelled',
    },
    progress: (done: number, total: number) => `${done} of ${total}`,
    progressLabel: 'Progress',
    noResult: 'I finished, but there is nothing to show for this one. Ask again and I will take another run at it.',
    resultHeading: 'Result',
    failedWhat: 'What happened:',
    tryAgain: 'Try again',
    openDraft: 'Open the draft',
    youAsked: 'You asked:',
    dismiss: 'Clear',
  },

  // ---- Draft workspace ----
  draft: {
    back: 'Back to home',
    loading: 'Opening the draft…',
    titleLabel: 'Title',
    bodyLabel: 'Draft text',
    edit: 'Edit',
    discard: 'Discard changes',
    save: 'Save',
    formattedLabel: 'Formatted draft',
    editingNote: 'Editing. Save to see the formatted text again.',
    saving: 'Saving…',
    saved: 'Saved',
    unsaved: 'Unsaved changes',
    saveFailed: 'Could not save. Your text is still here, try Save again.',
    readOnly: (status: DraftStatus) =>
      status === 'approved'
        ? 'This draft was approved, so it can no longer be edited.'
        : `This draft is ${status}, so it can no longer be edited.`,
    practiceLabel: 'Practice draft',
    practiceNote: 'Nothing here is sent anywhere. Approving it creates nothing.',
    practiceDelete: 'Delete practice draft',
    sourcesHeading: 'What the bot read',
    sourcesCount: (n: number) => `${n} ${n === 1 ? 'source' : 'sources'}`,
    sourceNumber: (n: number) => `Source ${n}: `,
    citeLabel: (n: number, title: string) => `Source ${n}: ${title}`,
    citedPlaces: (n: number) => (n === 1 ? 'Cited in 1 place in the text' : `Cited in ${n} places in the text`),
    notCited: 'Read, not cited',
    showInDraft: 'Show in the draft',
    publishedAs: (name: string) => `under your name, ${name}`,
    changeShort: 'Change',
    suggestionBar: (instruction: string) => `Suggested change from your request: "${instruction}"`,
    checklistHeading: 'Before you approve',
    checklist: {
      sources: (n: number) => `Reads from ${n} ${n === 1 ? 'source' : 'sources'}, listed on the right`,
      sourcesPhone: (n: number) => `Reads from ${n} ${n === 1 ? 'source' : 'sources'}`,
      noSources: 'No sources recorded',
      cited: (n: number, m: number) => `${n} of ${m} ${m === 1 ? 'source is' : 'sources are'} cited in the text`,
      noneCited: 'No sentence is linked to a source',
      editedNoLinks: 'You edited this draft, so source links were removed',
      internal: 'Not shared outside the company',
      external: 'Goes to a channel shared outside the company',
      suggestion: 'A suggested change is still open',
      unsaved: 'You have changes that are not saved',
      edited: 'You edited this draft',
      expires: (left: string) => `Expires soon: ${left}`,
      noExpiry: 'Does not expire',
      stripped: (n: number) => `${n} hidden or stripped ${n === 1 ? 'part' : 'parts'} to read`,
      practice: 'Practice draft: nothing is sent',
      ok: 'Looks fine',
      check: 'Check this',
      info: 'Note',
    },
    sourceOpen: 'Open in source',
    sourceNoLink: 'No link was recorded for this one.',
    sourceUnsafe: 'This address is not a web link, so it is shown as text only:',
    sourceSelected: 'Selected',
    askHeading: 'Ask for a change',
    askLabel: 'What should change?',
    askPlaceholder: 'Shorter, or: add a risk about legal review',
    askButton: 'Suggest a change',
    askSources: 'Ask me to add sources',
    askSourcesPrefill: 'Add the sources you used, with a link for each.',
    suggestionWorking: 'Working on your change…',
    suggestionHeading: 'Suggested change',
    suggestionFor: (instruction: string) => `You asked: "${instruction}"`,
    beforeAfter: 'Before and after',
    diffAdded: 'Added',
    diffRemoved: 'Removed',
    diffSame: 'Unchanged',
    diffChanged: 'Reworded',
    diffAddedCue: ' [added: ',
    diffRemovedCue: ' [removed: ',
    diffEndCue: '] ',
    diffListStyle: 'List style changed',
    diffShowRemoved: 'Show what was removed',
    diffHideRemoved: 'Hide what was removed',
    diffNoChange: 'This suggestion changes nothing.',
    accept: 'Accept change',
    rejectSuggestion: 'Reject',
    suggestionFailed: (error: string) => `I could not make that change. ${error}`,
    suggestionFailedNext: 'Try asking in other words.',
    saveFirst: 'Save or discard your edit before you approve.',
    resolveFirst: 'Accept or reject the suggested change before you approve.',
    expired: 'This draft expired, so it cannot be approved. Ask me for a fresh draft.',
    checkedNoChange: 'I checked: that request did not reach me, so you can ask again.',
    checkFirst: 'Check where this draft stands before you approve it again.',
    changeInProgress: 'I am still working on your last change. Wait for it to finish, then ask again.',
  },

  // ---- Approval ----
  approve: {
    button: (kind: DraftKind) => APPROVE_WORDS[kind].button,
    confirmHeading: 'Check before you approve',
    confirmTitle: 'Title',
    confirmWhere: 'Goes to',
    confirmWho: 'Published under the name',
    confirmLinks: 'Links in this draft',
    confirmNoLinks: 'This draft has no links.',
    confirmLinkLine: (text: string) => `Link text: ${text}`,
    confirmText: (kind: DraftKind, where: string, who: string) => APPROVE_WORDS[kind].confirmText(where, who),
    confirmNothing: (kind: DraftKind) => APPROVE_WORDS[kind].nothingYet,
    confirmPractice: 'This is a practice draft. Confirming creates nothing anywhere.',
    externalWarning: 'This channel is shared outside the company. People at other companies will read this.',
    externalCheckbox: 'I understand, and I want to post it there.',
    confirm: (kind: DraftKind) => APPROVE_WORDS[kind].confirmButton,
    confirmExternalButton: 'Confirm and post outside the company',
    confirming: (kind: DraftKind) => APPROVE_WORDS[kind].confirming,
    cancel: 'Not yet',
    errors: {
      needs_external_confirm: {
        text: 'This channel is shared outside the company, so I need your explicit yes before posting.',
        next: 'Tick the box, then confirm again.',
      },
      audience_blocked: {
        text: 'The company rules do not allow posting this to that audience.',
        next: 'Reject this draft, or ask me for a version meant for a different place.',
      },
      stale: {
        text: 'This draft changed after you opened it, so nothing was sent. I reloaded it.',
        next: 'Read it again, then approve if it is still right.',
      },
      expired: {
        text: 'This draft expired, so it was not sent.',
        next: 'Go home and ask me for a fresh one.',
      },
      not_pending: {
        text: 'This draft has already been decided, so nothing more was sent.',
        next: 'Reload to see the current state.',
      },
      // Only for a definite refusal (a 4xx the server meant): then it is known nothing was sent.
      other: {
        text: 'The server refused the approval, so nothing was sent.',
        next: 'Try again.',
      },
      // A dropped connection, a timeout, a 5xx or unknown_outcome: whether it was sent is not known.
      unknown_outcome: {
        text: "I can't tell whether this was sent. The connection dropped or the server did not answer in time.",
        next: 'Check where the draft stands before you try again, so it is not sent twice.',
      },
    },
    checkAgain: 'Check again',
    checkFailed: "I still can't tell whether this was sent. I could not reach the server to check.",
    checkedPending: 'I checked: the draft is still waiting for you, so it was not sent. You can approve it again.',
    goHome: 'Go home',
    reload: 'Reload the draft',
    tryAgain: 'Try again',
    approvedHeading: 'Approved',
    approvedWhere: (kind: DraftKind, label: string) => APPROVE_WORDS[kind].done(label),
    practiceApproved: 'Practice approved. Nothing was created, because this was practice.',
    resultLink: 'Open it',
    approvedNoLink: 'Approved.',
    rejectedNote: 'You rejected this draft. Nothing was sent.',
    expiredNote: 'This draft expired, so nothing was sent. Ask me for a fresh draft.',
    failedNote: 'This draft failed. Nothing was sent.',
  },

  reject: {
    button: 'Reject draft',
    heading: 'Why are you rejecting it?',
    confirm: 'Confirm rejection',
    cancel: 'Keep it',
    reasons: {
      wrong_facts: 'The facts are wrong',
      wrong_target: 'Wrong place to put it',
      tone: 'The tone is off',
      incomplete: 'It is incomplete',
      should_not_draft: 'I did not want a draft at all',
      other: 'Something else',
    } satisfies Record<RejectReason, string>,
  },

  // ---- App shell and navigation ----
  nav: {
    label: 'Main',
    home: 'Today',
    drafts: 'Drafts',
    followups: 'Follow-ups',
    activity: 'Activity',
    draftsCount: (n: number) => `${n} ${n === 1 ? 'draft needs' : 'drafts need'} a decision`,
    followupsCount: (n: number) => `${n} waiting on you`,
    activityCount: (n: number) => `${n} still working`,
    collapse: 'Collapse sidebar',
    expand: 'Expand sidebar',
    skip: 'Skip to content',
    askButton: 'Ask or jump to…',
    admin: 'Admin',
    connected: 'Connected',
    railLabel: 'App',
    shortcutMac: '⌘K',
    shortcutOther: 'Ctrl K',
    back: 'Back',
    groupInbox: 'Inbox',
    groupHistory: 'History',
    newRequest: 'New request',
  },

  theme: {
    label: 'Theme',
    system: 'System',
    light: 'Light',
    dark: 'Dark',
  },

  // ---- Command palette ----
  palette: {
    label: 'Ask PM OS or jump to a page',
    inputLabel: 'Ask, or type a page or draft name',
    placeholder: 'Ask anything, or jump to a page or draft…',
    ask: (q: string) => `Ask: ${q}`,
    askEmpty: 'Type a question and press Enter to ask',
    pages: 'Go to',
    drafts: 'Drafts',
    nothing: 'Nothing matches. Press Enter to ask me instead.',
    hintMove: 'to move',
    hintPick: 'to choose',
    hintClose: 'to close',
  },

  // ---- Toasts ----
  toast: {
    region: 'Notifications',
    saved: 'Draft saved.',
    approved: (kind: DraftKind) => APPROVE_WORDS[kind].toast,
    practiceApproved: 'Practice approved. Nothing was sent.',
    rejected: 'Draft rejected. Nothing was sent.',
    accepted: 'Change accepted.',
    dismissed: 'Removed from your follow-ups.',
    close: 'Dismiss notification',
  },

  // ---- Relative time ----
  time: {
    justNow: 'just now',
    minutes: (n: number) => `${n} min ago`,
    hours: (n: number) => `${n} h ago`,
    yesterday: 'yesterday',
    days: (n: number) => `${n} days ago`,
    left: (hours: number, minutes: number) => (hours >= 1 ? `${hours} h left` : `${minutes} min left`),
  },

  // ---- Home launchpad ----
  launch: {
    composerLabel: 'What do you need?',
    send: 'Ask',
    newLine: 'Shift + Enter for a new line',
    suggestions: ['Summarize my week', 'Draft a status update', 'What is blocked?'],
    suggestionsLabel: 'Suggestions',
    draftsCard: 'Drafts need you',
    waitingCard: 'Waiting on you',
    waitedOnCard: 'You are waiting on',
    viewDrafts: 'All drafts',
    viewFollowups: 'All follow-ups',
    noPendingDrafts: 'Nothing needs a decision. Inbox zero, draft edition.',
    recent: 'Recent activity',
    viewActivity: 'All activity',
    noActivity: 'Nothing asked yet. The box above is very patient.',
    summaryLabel: 'At a glance',
  },

  // ---- Today (the home page) ----
  today: {
    /** "Three things need you." Numbers are words up to ten. */
    needs: (n: number) => {
      const words = ['Nothing', 'One', 'Two', 'Three', 'Four', 'Five', 'Six', 'Seven', 'Eight', 'Nine', 'Ten'];
      if (n <= 0) return 'Nothing needs you right now.';
      if (n === 1) return 'One thing needs you.';
      return `${n <= 10 ? words[n] : n} things need you.`;
    },
    briefReady: 'Your daily brief is in',
    /** No brief has ever run. Briefs come on weekday mornings, at the time the PM sets in Slack. */
    briefNever: "Your first brief hasn't arrived yet. It comes on weekday mornings, at the time you set in Slack.",
    briefStale: (when: string) => `Your last brief was ${when}.`,
    briefNeverCard: 'Nothing here yet. This fills in when your first brief arrives.',
    briefNeverSub: 'No brief yet',
    briefStaleCard: 'Nothing in your last brief.',
    composerLabel: 'Ask PM OS',
    readsFrom: 'Reads from',
    systems: ['Jira', 'Confluence', 'Slack'],
    nothingSent: 'Nothing is sent anywhere until you approve it.',
    statsLabel: 'Today in numbers',
    stats: {
      drafts: 'Drafts to decide',
      waiting: 'Waiting on you',
      waitedOn: "You're waiting on",
      changed: 'Changed overnight',
      running: 'Running now',
      done: 'Done today',
    },
    statSub: {
      nextExpiry: (left: string) => `Next one: ${left}`,
      noExpiry: 'None of them expire',
      allDecided: 'All decided',
      oldest: (age: string) => `Oldest: ${age}`,
      nobody: 'Nobody',
      quiet: 'A quiet night',
      bySystem: (parts: string[]) => parts.join(', '),
      inSystem: (n: number, system: string) => `${n} in ${system}`,
      running: 'Requests still working',
      nothingRunning: 'Nothing running',
      done: 'Answers and drafts',
      noneDone: 'Nothing finished yet',
    },
    decideHeading: 'Needs your decision',
    allDrafts: 'All drafts',
    review: 'Review',
    working: (text: string) => `Working: ${text}`,
    waitingHeading: 'Waiting on you',
    changedHeading: 'What changed',
    recentHeading: 'Recent activity',
    allActivity: 'All activity',
    allFollowups: 'All follow-ups',
  },

  // ---- Drafts page ----
  draftsPage: {
    title: 'Drafts',
    lede: 'Everything I wrote for you. Nothing is sent until you approve it.',
    countLine: (n: number) => `${n} to decide`,
    tabsLabel: 'Filter drafts',
    tabs: { pending: 'Needs you', approved: 'Approved', expired: 'Expired', all: 'All' },
    emptyPending: 'Nothing needs a decision. Suspiciously tidy.',
    emptyApproved: 'Nothing approved yet. Your first one gets a small fanfare.',
    emptyExpired: 'Nothing expired. Every draft got the attention it deserved.',
    created: (when: string) => `Created ${when}`,
    columns: { title: 'Draft', kind: 'Goes to', status: 'Status', expires: 'Time left', created: 'Created' },
  },

  // ---- Follow-ups page ----
  followups: {
    title: 'Follow-ups',
    emptyAll: 'No follow-ups. Nobody is waiting on you, and you are not waiting on anyone. Enjoy it.',
    lede: 'Who is waiting on you, and who you are waiting on, from the daily brief.',
    countLine: 'From today\'s brief',
    countLineNever: 'No brief yet',
    emptyStale: 'Nothing in your last brief, so there is nothing to follow up on.',
    countLineStale: (when: string) => `From the brief of ${when}`,
    emptyNever:
      'No follow-ups yet. They come from your daily brief, and your first one has not arrived. Say "brief me at 8" to the bot in Slack to start.',
    from: (who: string) => `From ${who}`,
    age: (n: number) => (n === 0 ? 'Today' : n === 1 ? '1 day' : `${n} days`),
    columns: { who: 'Who', what: 'What', source: 'Source', age: 'Age' },
  },

  // ---- Activity and threads ----
  activity: {
    title: 'Activity',
    lede: 'Your recent requests to PM OS.',
    countLine: (n: number) => `${n} ${n === 1 ? 'request' : 'requests'}`,
    askedAt: (when: string) => `Asked ${when}`,
    empty: 'Nothing asked yet. I am ready when you are.',
    loading: 'Loading your requests…',
    status: {
      queued: 'Queued',
      working: 'Working',
      needs_approval: 'Draft ready',
      done: 'Done',
      failed: 'Failed',
      cancelled: 'Cancelled',
    },
    you: 'You',
    bot: 'PM OS',
    showSteps: 'Show steps',
    reference: 'Reference for support',
    copyReference: 'Copy',
    copiedReference: 'Copied',
    hideSteps: 'Hide steps',
    stepsLabel: 'Steps',
    draftCard: 'Draft',
    openDraft: 'Open draft',
    notFound: 'I could not find that request.',
    threadLabel: 'Conversation',
    columns: { request: 'Request', status: 'Status', created: 'Asked' },
  },

  // ---- Draft workspace panel ----
  panel: {
    label: 'About this draft',
    tabsLabel: 'Draft panel',
    doc: 'Draft',
    unchanged: (n: number) => `${n} unchanged ${n === 1 ? 'paragraph' : 'paragraphs'}`,
    edited: 'Edited by you',
    external: 'Shared outside the company',
    sources: 'Sources',
    ask: 'Ask for a change',
    details: 'Details',
    before: 'Before',
    after: 'After',
    bot: 'Written by',
    created: 'Created',
    expires: 'Expires',
    never: 'Does not expire',
    task: 'Request',
    id: 'Draft id',
    openTask: 'Open the request',
    noTask: 'Not linked to a request',
    target: 'Goes to',
    kind: 'Kind',
    more: 'More actions',
    moreShort: 'More',
    breadcrumb: 'Breadcrumb',
  },

  // ---- Admin area (admins and auditors) ----
  admin: {
    area: 'Admin',
    subnavLabel: 'Admin sections',
    sections: { overview: 'Overview', audit: 'Audit log', team: 'Team', connectors: 'Connectors' },
    noAccess: {
      title: "You don't have access to this",
      body: 'Your role does not include this part of the app. If you think it should, ask an admin.',
      back: 'Back to Today',
    },
    notRecorded: 'Not recorded',
    loading: 'Loading…',

    overview: {
      title: 'Overview',
      lede: 'How the team is doing over the last week.',
      sinceLine: (date: string) => `Last 7 days, since ${date}`,
      statsLabel: 'Team counts',
      stats: { pending: 'Pending drafts', approved: 'Approved', rejected: 'Rejected', denied: 'Denied actions' },
      subs: {
        pending: 'Across the whole team',
        approved: 'Writes that went out',
        rejected: 'Drafts turned down',
        denied: (n: number) => (n > 0 ? 'Refused. Worth a look' : 'None. A calm week'),
        unavailable: 'Not available right now',
      },
      partial: "The tool server didn't answer, so some counts are missing. Try again in a moment.",
      linksLabel: 'Where to next',
      links: {
        audit: { title: 'Audit log', body: 'Who did what, when and where. Filter it, bookmark it, export it.' },
        team: { title: 'Team', body: 'Members, their roles and which accounts they have linked.' },
        connectors: { title: 'Connectors', body: 'Whether the systems the bots read and write are reachable.' },
      },
      error: 'I could not load the overview.',
      recent: {
        denied: 'Recent denied actions',
        approved: 'Recent approvals',
        seeAll: 'See all',
        noDenied: 'No denied actions. A calm stretch.',
        noApproved: 'No approvals recorded yet.',
        failed: 'I could not load these.',
      },
    },

    audit: {
      title: 'Audit log',
      lede: 'Every action the bots and this app took, newest first.',
      metadataOnly:
        'This view shows metadata only: who did what, when, and to which system. It never shows draft text or Slack messages.',
      filtersLabel: 'Filter the audit log',
      fields: {
        actor: 'Who',
        actorHint: 'A handle, such as ana',
        action: 'Action',
        actionHint: 'For example jira.create_issue',
        outcome: 'Outcome',
        anyOutcome: 'Any outcome',
        from: 'From',
        to: 'To',
        daysHint: 'Days are in UTC',
      },
      apply: 'Apply',
      clear: 'Clear filters',
      backwards: 'The first day is after the last day, so nothing can match.',
      eventsHeading: 'Events',
      count: (n: number, more: boolean) => (more ? `${n} shown, more available` : `${n} ${n === 1 ? 'event' : 'events'}`),
      tableLabel: 'Audit events',
      columns: {
        time: 'Time (UTC)',
        who: 'Who',
        client: 'Client',
        action: 'Action',
        system: 'System',
        target: 'Target',
        outcome: 'Outcome',
        details: 'Details',
      },
      showDetails: (action: string, time: string) => `Show details for ${action} at ${time}`,
      hideDetails: (action: string, time: string) => `Hide details for ${action} at ${time}`,
      detailsLabel: 'Every field of this event',
      detailFields: {
        id: 'Event id',
        at: 'Time',
        actor: 'Who',
        client: 'Client',
        action: 'Action',
        system: 'System',
        target: 'Target',
        outcome: 'Outcome',
        detail: 'Note',
        sources: 'Items read',
        titledSources: 'Items with a title',
        skill: 'Skill',
        bot: 'Bot',
        task: 'Run',
      },
      titlesNote: 'Only the count is kept. Titles are never shown here.',
      loadMore: 'Load more',
      loadingMore: 'Loading…',
      endLine: 'That is every event that matches.',
      empty: 'No audit events yet. Once the bots do something, it shows up here.',
      emptyFiltered: 'Nothing matches these filters.',
      loading: 'Loading the audit log…',
      loadFailed: 'I could not load the audit log.',
      loadMoreFailed: 'I could not load more events. The ones above are still good.',
      export: 'Export CSV',
      exporting: 'Preparing…',
      exportedLine: (n: number, file: string) => `Saved ${n.toLocaleString('en-US')} ${n === 1 ? 'event' : 'events'} to ${file}.`,
      exportTruncated: (n: number) =>
        `The file holds the newest ${n.toLocaleString('en-US')} events, and more matched. Narrow the dates and export again to get the rest.`,
      exportFailed: 'I could not export the audit log, and no file was saved.',
      clients: {
        mcp: 'A bot or skill run',
        orchestrator: 'The bot runtime',
        bot: 'The Slack app',
        web: 'This web app',
        console: 'The operator console',
      } as Record<string, string>,
      outcomes: {
        ok: 'Ran',
        denied: 'Denied',
        pending: 'Pending',
        approved: 'Approved',
        rejected: 'Rejected',
        granted: 'Granted',
        started: 'Started',
        error: 'Error',
        failed: 'Failed',
        expired: 'Expired',
      } as Record<string, string>,
    },

    team: {
      title: 'Team',
      lede: 'Everyone on the team list. People join through Slack onboarding or a pull request to team.yaml.',
      count: (n: number) => `${n} ${n === 1 ? 'member' : 'members'}`,
      tableLabel: 'Team members',
      columns: { member: 'Member', roles: 'Roles', bot: 'Bot', email: 'Email', atlassian: 'Atlassian', slack: 'Slack', github: 'GitHub' },
      emailYes: 'On file',
      emailNo: 'Missing',
      linked: 'Linked',
      notLinked: 'Not linked',
      unknown: 'Unknown',
      noRoles: 'No role',
      accountsDown: "I couldn't ask the tool server about linked accounts, so those columns are blank for now.",
      linkingOff: 'Account linking is off while connectors run on fake data, so nothing can be linked yet.',
      empty: 'There are no members on the team list.',
      loading: 'Loading the team…',
      loadFailed: 'I could not load the team.',
      roleLabels: {
        admin: 'Admin',
        pm: 'Product Manager',
        reviewer: 'Reviewer',
        auditor: 'Auditor',
        'skill-author': 'Skill author',
      } as Record<string, string>,
    },

    connectors: {
      title: 'Connectors',
      lede: 'Each system the bots read and write, checked just now.',
      tableLabel: 'Connectors',
      columns: { name: 'Connector', mode: 'Mode', health: 'Health', checked: 'Last checked' },
      modes: { live: 'Live', fake: 'Fake' } as Record<string, string>,
      health: { ok: 'Healthy', down: 'Down' } as Record<string, string>,
      errorClasses: {
        timeout: 'Timed out',
        auth: 'Sign-in refused',
        unreachable: 'Cannot be reached',
        error: 'Failed',
      } as Record<string, string>,
      recheck: 'Check again',
      rechecking: 'Checking…',
      note: "Jira, Confluence and Slack act with each person's own account, so they read healthy here. A broken link shows in that person's own setup.",
      empty: 'No connectors are configured.',
      loading: 'Checking the connectors…',
      loadFailed: 'I could not check the connectors.',
    },
  },

  // ---- Errors: plain, what happened, one next step ----
  errors: {
    network: 'I could not reach the server.',
    unknown: 'Something went wrong on my side.',
    /** For an action whose outcome is not known: it never says the thing did or did not happen. */
    unsure: (what: string) => `I can't tell whether ${what}.`,
    unsureWhat: {
      rejected: 'the rejection went through',
      accepted: 'the change was accepted',
      declined: 'the change was set aside',
      asked: 'that request reached me',
    },
    unsureNext: 'Check where things stand before you try again.',
    unsureStarted: "I can't tell whether that was started. Check Activity before you ask again, so it does not run twice.",
    checkAgain: 'Check again',
    retry: 'Try again',
    signedOut: 'You were signed out. Sign in again to continue.',
    loadDraftFailed: 'I could not open that draft.',
    notFound: 'That draft does not exist, or it is not yours.',
    loadHomeFailed: 'I could not load your drafts.',
    askFailed: 'I could not send that to the bot.',
    followupFailed: 'I could not start that draft.',
    dismissFailed: 'I could not remove that item.',
    settingFailed: 'I could not save that setting.',
    screenBroke: 'Something on this page broke. Go back home, or reload the page.',
  },
} as const;
