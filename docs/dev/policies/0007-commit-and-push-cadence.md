# Policy | Commit And Push Cadence

## Policy

- Commit at meaningful slice boundaries rather than waiting until a large body of work becomes hard to reason about or recover.
- Make an explicit local checkpoint before risky refactors, rebases, or cleanup that could discard work.
- Push when remote backup, collaboration, review, CI, or cross-machine continuity materially matters.
- Do not delay pushing important shared work so long that teammates or automation reason from stale branch state.
- Do not push half-understood or misleading commits to shared branches just to create activity.
- If the repo allows work-in-progress commits, keep them on private or clearly scoped branches unless the shared workflow says otherwise.
- Match push cadence to branch type:
  - private branch: checkpoint for backup and continuity as needed
  - shared feature branch: push whenever collaborators or CI need the current state
  - protected branch: push only through the repo's documented integration path
- Be explicit about whether end-of-day or end-of-slice pushing is expected for backup and handoff.
- In multi-track repos, do not let unpublished local `main` become a hidden holding area for architectural work once other maintainers depend on `main` for routine maintenance or operational continuity.
- Require handoff clarity about branch intent when different work classes coexist, for example whether a branch is maintenance-safe, migration-only, or still experimental.
## Adoption Notes

Use this module when repos need a durable answer to "when should I commit?" and "when should I push?" across more than one maintainer or environment.

Repo-type guidance:
- `product-engineering`: usually wants frequent local commits, timely shared-branch pushes, and explicit rules for when CI-ready state is required
- `library-cli`: often wants commits at coherent feature/fix boundaries and pushes aligned with review or release preparation
- `workspace-agent`: usually benefits from frequent private checkpoints because local experimentation and repo-local automation can move quickly
- `writing-project`: may prefer fewer but still meaningful commits around review checkpoints, major draft edits, and submission-affecting changes

Developer-preference guidance:
- solo maintainers can tolerate lighter push cadence if local recovery is strong, but should still push before machine risk or context switching
- teams with active CI or review automation should push early enough for those systems to stay relevant
- repos that value clean shared history may allow messy local checkpoints but require cleanup before integration

Multi-track repo guidance:
- when conservative maintenance and deeper platform work happen in parallel, maintenance-oriented branches should be pushed soon enough that operators are not forced to reason from stale assumptions
- architectural branches can tolerate more local iteration, but should be published once their state is coherent enough for another operator to interpret without private chat context
