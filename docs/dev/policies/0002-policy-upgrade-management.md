# Policy | Policy Upgrade Management

## Policy

- Treat shared policy upgrades as intentional maintenance work, not accidental drift from copying files ad hoc.
- Check for policy-library updates through a deterministic source of truth, such as:
  - tagged releases
  - upstream commits
  - a pinned selector bundle version
  - a known GitHub repository and branch or release channel
- Record what version, tag, or commit of the shared policy library the repo last reviewed or adopted when that information materially affects reproducibility.
- When upstream policy changes appear, decide explicitly whether to:
  - adopt a new module
  - upgrade an already adopted module
  - retire a no-longer-useful local policy
  - defer the change for a documented reason
- Review profile changes separately from module changes; a profile upgrade should not silently force a repo into every newly suggested module.
- When a local repo has customized policy, prefer merge review over blind overwrite.
- Retire superseded local policy files explicitly when a shared replacement makes them unnecessary.
- When the policy library publishes release notes, changelog entries, or comparable upgrade summaries, use them to scope the upgrade review before patching local policy.
- If the repo follows upstream commits directly instead of releases, define how often to check and what level of change justifies adoption.
- Keep policy upgrade decisions durable in repo docs or notes when the rationale would otherwise be lost.
- One dated policy adoption or upgrade note may serve as the canonical durable artifact for:
  - the upgrade decision
  - adoption feedback
  - reusable continuity notes
  when it records the version reviewed, decision taken, rationale, and notable fit or friction.
## Adoption Notes

Use this module when the repo depends on an external or shared policy library and needs a durable contract for staying current without adopting every upstream change blindly.

Repo-type guidance:
- `product-engineering`: usually wants deliberate upgrade review because planning, release, and operator policies can have cross-cutting effects
- `library-cli`: often benefits from checking policy upgrades near release or dependency-maintenance cycles
- `workspace-agent`: often benefits from reviewing upstream policy or selector updates regularly because skill and orchestration behavior can drift quickly
- `writing-project`: usually wants lighter upgrade cadence tied to major workflow or deliverable shifts rather than constant policy churn
