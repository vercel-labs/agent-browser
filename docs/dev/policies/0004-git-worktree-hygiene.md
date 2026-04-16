# Policy | Git / Worktree Hygiene

## Policy

- Start branch-sensitive work by checking `git status`.
- Treat pre-existing dirty state as a real constraint.
- Keep one bounded branch or worktree scope per execution slice or roadmap lane, consistent with the repo's documented integration model.
- When parallel work is needed, prefer `git worktree` over a second full clone.
- Do not call work merge-ready while the intended changes are still uncommitted.
- If overlapping dirty work exists across branches or worktrees, open a reconciliation step rather than calling it a normal merge.
- Keep branch scope narrow and avoid mixing unrelated lanes unless the active slice requires it.
## Adoption Notes

Use this module in repos where multiple lanes, multiple worktrees, or parallel agents regularly overlap.

This module governs local git cleanliness and overlap handling. Use `branch-and-integration-strategy` to choose whether the repo prefers direct-to-`main`, short-lived feature branches, or another integration model.
