## Fork Strategy

This repository now uses the official `vercel-labs/agent-browser` codebase as the `main` branch.

Legacy state from the former `BUNotesAI/agent-browser-session` fork is preserved for reference:

- Branch: `legacy/patchright-v0.4.6`
- Tag: `legacy-v0.4.6`

Why this layout:

- The Patchright fork and the current official project have diverged at the runtime architecture level.
- A direct long-lived merge would create a high-conflict codebase that is difficult to maintain.
- The official project already includes first-party support for persistent profiles, session persistence, and auto-connect.

Maintenance rule going forward:

- Track official updates on `main`.
- Cherry-pick or re-implement only the fork features that are still valuable.
- Do not try to keep a permanent "full merge" between the legacy Patchright daemon and the official native daemon.
