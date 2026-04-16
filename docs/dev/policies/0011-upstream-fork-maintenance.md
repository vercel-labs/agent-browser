# Policy | Upstream Fork Maintenance

## Policy

- Use a distinct upstream remote when the repo carries private or local features on top of a non-owned active upstream.
- Keep private feature work isolated from the branch used to mirror or track upstream state.
- Rebase private branches onto fresh upstream state when the goal is to keep a small, understandable delta over an active upstream.
- Prefer force-push only on branches that are explicitly private, unshared, or documented as rebase-managed.
- Do not rewrite shared branch history casually when other collaborators, CI systems, or deployments may already depend on it.
- Keep one branch or tag that records the last known clean upstream sync point before heavy private divergence.
- Record conflict-prone patches, local carry patches, or intentionally retained divergences somewhere durable when they are likely to recur across rebases.
- Be explicit about whether downstream release tags are cut from rebased private branches, merge-based integration branches, or snapshots after upstream sync.
- If a private feature is becoming long-lived and hard to rebase, reconsider whether it should remain a fork-local patch set or become a maintained downstream branch line.
## Adoption Notes

Use this module when the repo is a fork or downstream derivative of an actively changing upstream that the maintainers do not control.

Repo-type guidance:
- `product-engineering`: useful for internal product forks of vendor or open-source systems where private deployable behavior rides on top of active upstream updates
- `library-cli`: useful for downstream maintained forks that publish their own releases while selectively ingesting upstream fixes
- `workspace-agent`: useful for private skill, prompt, or policy forks built on public upstream agent tooling
- `writing-project`: rarely needed unless the repo is effectively maintaining a downstream derivative of another canonical source tree

Developer-preference guidance:
- rebase-oriented downstreams usually want small private deltas and frequent upstream sync
- audit-heavy downstreams may prefer merge-based integration branches that preserve explicit upstream incorporation points
- force-push is reasonable on truly private maintenance branches, but not as a default on shared collaboration branches
