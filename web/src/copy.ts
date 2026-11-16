// Every user-facing word in the app lives in this file, so it can be edited in one place.
// Tone: light and gently funny in greetings, empty states and hints. Errors, approvals and
// anything about security are plain.

import type { DraftKind, DraftStatus, HintName, RejectReason } from './api/types';

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
    approve: "This creates the page in Confluence under your name. I'll wait until you're sure.",
  } satisfies Record<HintName, string>,
  /** One short line each, for the banner on a phone. */
  hintsShort: {
    sources: 'Sources shows where I got each claim.',
    ask: 'Ask for changes in plain words.',
    approve: 'Nothing posts until you approve.',
  } satisfies Record<HintName, string>,
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
    askButton: 'Ask',
    asking: 'Asking…',
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
    sourcesHeading: 'Sources',
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
    accept: 'Accept',
    rejectSuggestion: 'Reject',
    suggestionFailed: (error: string) => `I could not make that change. ${error}`,
    suggestionFailedNext: 'Try asking in other words.',
    saveFirst: 'Save or discard your edit before you approve.',
    resolveFirst: 'Accept or reject the suggested change before you approve.',
    expired: 'This draft expired, so it cannot be approved. Ask me for a fresh draft.',
    changeInProgress: 'I am still working on your last change. Wait for it to finish, then ask again.',
  },

  // ---- Approval ----
  approve: {
    button: (kind: DraftKind) =>
      ({
        confluence_page: 'Approve to Confluence',
        jira_issue: 'Approve to Jira',
        slack_reply: 'Approve to Slack',
        slack_message: 'Approve to Slack',
      })[kind],
    confirmHeading: 'Check before you approve',
    confirmTitle: 'Title',
    confirmWhere: 'Goes to',
    confirmWho: 'Published under the name',
    confirmLinks: 'Links in this draft',
    confirmNoLinks: 'This draft has no links.',
    confirmLinkLine: (text: string) => `Link text: ${text}`,
    confirmText: (kind: DraftKind, where: string, who: string) => {
      const what = {
        confluence_page: 'create a Confluence page',
        jira_issue: 'create a Jira issue',
        slack_reply: 'post a Slack reply',
        slack_message: 'post a Slack message',
      }[kind];
      return `This will ${what} in ${where}, under your name (${who}).`;
    },
    confirmNothing: 'Nothing has been written until you press Confirm.',
    confirmPractice: 'This is a practice draft. Confirming creates nothing anywhere.',
    externalWarning: 'This channel is shared outside the company. People at other companies will read this.',
    externalCheckbox: 'I understand, and I want to post it there.',
    confirm: 'Confirm and create',
    confirmSlack: 'Confirm and post',
    confirmExternalButton: 'Confirm and post outside the company',
    confirming: 'Sending…',
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
      other: {
        text: 'The approval did not go through, and nothing was created.',
        next: 'Try again.',
      },
    },
    goHome: 'Go home',
    reload: 'Reload the draft',
    tryAgain: 'Try again',
    approvedHeading: 'Approved',
    approvedWhere: (label: string) => `Created in ${label}.`,
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
    home: 'Home',
    drafts: 'Drafts',
    followups: 'Follow-ups',
    activity: 'Activity',
    draftsCount: (n: number) => `${n} ${n === 1 ? 'draft needs' : 'drafts need'} a decision`,
    followupsCount: (n: number) => `${n} waiting on you`,
    activityCount: (n: number) => `${n} still working`,
    collapse: 'Collapse sidebar',
    expand: 'Expand sidebar',
    skip: 'Skip to content',
    askButton: 'Ask PM OS',
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
    approved: 'Approved and sent.',
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

  // ---- Drafts page ----
  draftsPage: {
    title: 'Drafts',
    lede: 'Everything I wrote for you. Nothing is sent until you approve it.',
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
    from: (who: string) => `From ${who}`,
    age: (n: number) => (n === 0 ? 'Today' : n === 1 ? '1 day' : `${n} days`),
    columns: { who: 'Who', what: 'What', source: 'Source', age: 'Age' },
  },

  // ---- Activity and threads ----
  activity: {
    title: 'Activity',
    lede: 'Your recent requests to PM OS.',
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

  // ---- Errors: plain, what happened, one next step ----
  errors: {
    network: 'I could not reach the server, so nothing was changed.',
    unknown: 'Something went wrong on my side, and nothing was changed.',
    retry: 'Try again',
    signedOut: 'You were signed out. Sign in again to continue.',
    loadDraftFailed: 'I could not open that draft.',
    notFound: 'That draft does not exist, or it is not yours.',
    loadHomeFailed: 'I could not load your drafts.',
    askFailed: 'I could not send that to the bot, and nothing was started.',
    followupFailed: 'I could not start that draft, and nothing was sent.',
    dismissFailed: 'I could not remove that item.',
    settingFailed: 'I could not save that setting.',
    screenBroke: 'Something on this page broke, and nothing was changed.',
  },
} as const;
