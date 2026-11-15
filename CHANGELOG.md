# Changelog

One "PM OS can now ___" line per capability a PM would care about (the launch-post test,
`docs/bot-decisions.md` D9). Plumbing that a PM never sees doesn't get a line.

## Unreleased (before Gate 1; nothing is live yet)
- PM OS can now learn from your edits and rejections: a rejected draft asks why in one tap, each bot's owner gets a daily list of what went wrong, and changes to a bot's skills must pass its benchmarks before they merge.
- PM OS can now be installed on the Spark with one setup script and run day to day with `pmos-admin`: status, backups and restore, adding bots, and runbooks written for PM platform owners.
- PM OS can now be tried end to end in a test Slack workspace before the Spark is ready, with a Claude model standing in for the local models (testing only).
- PM OS can now onboard you itself: DM it, tap **Request access**, and once a platform owner approves you get your own `pm-<you>` bot and a `setup` checklist to connect your accounts.
- PM OS can now keep your bot's notes in your own private repo in the Fortanix GitHub org, so you can read and edit what it remembers.
- PM OS can now be talked to as named bots: DM the Docs bot (`docs: …`) or your own `pm-<you>` bot in Slack, and watch its progress in one message.
- PM OS can now draft a Jira issue, Confluence page or Slack message and show you a card to Approve, Edit or Reject before anything is sent.
- PM OS can now run a routine you describe in plain words ("every weekday at 8am, summarize #launch"), and you can pause or delete it from Slack.
- PM OS can now show every bot, routine, pending approval and task trace on one small console.
- PM OS can now search the Fortanix support docs from Slack: `/docs rotate a key`, visible only to you.
- PM OS can now draft a Confluence page or Jira issue and wait for your approval before anything is created.
- PM OS can now read Jira, Confluence and Slack as you, with your own permissions, once you link your accounts.
