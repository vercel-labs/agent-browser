# Policy Adoption Note | 2026-04-12

## Installed Bundle

- bundle: `repo-policy-selector`
- bundle version: `v0.1.8`
- source commit: `dd3ed514ca1c71b35f044a46f12048eb8fef6e08`

## Selected Profile

- profile: `standalone-library`
- inferred repo purpose: `library-cli`
- execution bias: `max-token-efficiency`

## Adopted Modules

- `policy-management`
- `policy-upgrade-management`
- `policy-adoption-feedback-loop`
- `git-worktree-hygiene`
- `commit-history-discipline`
- `branch-and-integration-strategy`
- `commit-and-push-cadence`
- `versioning-and-release`
- `turn-closeout`
- `validation-and-handoff`

## Repo-Local Guidance Kept In AGENTS.md

- package manager requirements
- code style rules
- documentation update contract
- dashboard UI constraints
- release process details
- architecture summary
- testing commands
- Windows debugging workflow
- `opensrc/` usage notes

## What Worked Cleanly

- The repo matched a clean adoption path with no existing `docs/dev/policies/` tree to migrate.
- The selector's `standalone-library` recommendation fits the repo shape and release workflow.
- Existing `AGENTS.md` guidance was already structured enough to keep repo-specific sections local and add a policy entrypoint cleanly.

## Friction Or Ambiguity

- The generated `AGENTS.md` patch only appended the policy entry section; it did not materially thin shared guidance because the repo's existing file is mostly repo-specific.
- The planning audit reports missing `ROADMAP.md`, `RUNBOOK.md`, and `docs/dev/plans/`, but those are not blockers for this repo profile.
- The selector's section extraction over-read headings inside markdown examples, so merge suggestions around release snippets should be treated as heuristic rather than authoritative.

## Local Overrides And Decisions

- No profile modules were deferred during initial adoption.
- Release procedure remains documented in `AGENTS.md` because it is repo-specific and more concrete than the generic shared release module.
- This repo will treat `docs/dev/policies/` as the durable shared-policy surface and `docs/dev/notes/` as the durable upgrade and feedback surface.

## Follow-Up

- On the next policy review, run the selector upgrade workflow against the pinned bundle before changing module files.
- If repeated adoption friction appears, feed it back upstream to the shared policy repo instead of accumulating local drift silently.
