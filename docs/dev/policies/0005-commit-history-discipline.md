# Policy | Commit History Discipline

## Policy

- Prefer commits that represent one coherent change or one tightly related slice of work.
- Do not mix unrelated fixes, refactors, and feature work in the same commit when they can be separated cleanly.
- Keep commit messages truthful about the actual change instead of describing aspirational intent.
- Write commit subjects that make sense in history on their own without relying on chat context.
- Include enough detail in the commit body when the reason, risk, migration effect, or operator impact would be unclear from the diff alone.
- Commit before risky history operations, broad rebases, or destructive cleanup so recoverable checkpoints exist.
- Do not create fake cleanliness by squashing materially different changes into one commit if that harms reviewability or future archaeology.
- Do not create noisy checkpoint spam on shared history when the repo expects a cleaner review-oriented log.
- Treat commit history as a durable engineering artifact, not just transport for the current turn.
## Adoption Notes

Use this module in repos where git history is expected to support review, release notes, rollback, or later debugging.

Repo-type guidance:
- `product-engineering`: usually wants reviewable feature/fix commits with bodies for migration or operator impact when needed
- `library-cli`: often benefits from commit history that maps cleanly to release notes and compatibility changes
- `workspace-agent`: usually benefits from explicit commit subjects because downstream maintainers often inspect history to understand selector, skill, or policy changes
- `writing-project`: can keep lighter history, but major structure changes, evidence updates, and submission-affecting edits should still be described truthfully

Developer-preference guidance:
- squash-heavy teams may allow messy local checkpoint commits before merge, but should still require a truthful final shared history
- repos that value archaeology may prefer preserving a few well-scoped intermediate commits instead of aggressively flattening everything
