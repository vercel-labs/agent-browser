# Policy | Versioning And Release

## Policy

- Document one primary versioning scheme for the repo instead of mixing incompatible schemes opportunistically.
- Choose a versioning scheme that matches the consumer contract:
  - use semantic versioning when downstream users depend on compatibility signals between released artifacts
  - use date-based or milestone-based versioning when the repo primarily ships dated deliverables, internal deployment cuts, or review snapshots rather than reusable APIs
- Treat a version or release tag as an immutable cut that points to a reviewable repo state.
- Version and release consumer-visible changes, not every internal commit by default.
- Record what changed, who it affects, and any required migration, rollout, or operator action for each release.
- Keep the release process deterministic enough that two maintainers would cut the same release from the same validated state.
- Be explicit about release gating:
  - what validation is required before a release cut
  - whether release notes are required
  - whether tags, packages, deploys, or deliverable bundles are the canonical release artifact
- Do not imply stronger compatibility guarantees than the repo can actually honor.
- If the repo supports multiple artifact types, document which artifact is authoritative for versioning and which are derived outputs.
## Adoption Notes

Use this module when the repo ships named versions, release tags, package builds, deployable cuts, or formal deliverable revisions.

Repo-type guidance:
- `product-engineering`: usually version consumer-facing API, app, or deployable changes; release notes should emphasize user-visible behavior, operator steps, and migration risk
- `library-cli`: usually prefer semantic versioning because external consumers often depend on compatibility signals from tags or packages
- `workspace-agent`: version installable skills, plugins, or policy bundles when downstream repos consume them as artifacts; if the repo is mostly internal, lighter tag-based releases may be enough
- `writing-project`: often prefer revision, milestone, or date-based release cuts tied to submission or review checkpoints rather than semantic versioning

Developer-preference guidance:
- manual maintainers may prefer explicit human-reviewed release notes and manual tagging
- automation-heavy teams may prefer deterministic changelog generation, release scripts, and automated tagging once validation passes
- trunk-based repos may cut releases directly from `main`, while branch-heavy repos may require a documented release branch or stabilization step
- concise teams may keep short release summaries, while externally consumed repos usually need more explicit compatibility and upgrade notes
