# Policy | Policy Adoption Feedback Loop

## Policy

- After first policy adoption, major policy upgrade, or meaningful policy friction, record a dated feedback artifact in the adopting repo.
- The feedback artifact should identify at least:
  - installed policy bundle version or ref, or the policy source reviewed
  - selected profile
  - modules adopted
  - modules deferred, retired, or overridden locally
  - what worked cleanly
  - what created friction or ambiguity
  - what should remain repo-local
  - what may warrant an upstream module, profile, or selector change
- Prefer storing dated adoption feedback in the repo's normal durable continuity surface, such as:
  - `docs/dev/notes/`
  - `docs/dev/memories/`
  - bounded plans plus matching runbook entries
  - another documented local equivalent
- Do not leave important adoption lessons only in chat history, commit messages, or oral maintainer knowledge.
- When feedback appears reusable across repos, route it into the shared policy repo through a deterministic harvest path rather than treating it as one repo's private observation.
- If the repo uses a pinned installed selector bundle, tie feedback to that pinned version so later maintainers can interpret it correctly.
- When a repo adopts local overrides instead of the exact starter profile, record why; those reasons are often the best signal for future shared policy refinement.
- When a repo upgrades policy, compare the new experience to prior adoption notes so repeated friction becomes visible over time.
- A single dated artifact may satisfy this module, `policy-upgrade-management`, and `notes-and-memories` when it captures both the upgrade or adoption decision and the resulting feedback clearly.
## Adoption Notes

Use this module when repos adopt shared policy from an external source library and want a durable loop between downstream adoption experience and upstream policy improvement.

This module complements `notes-and-memories` and `policy-harvest-loop`:
- `notes-and-memories` defines where continuity artifacts live
- `policy-harvest-loop` governs how a policy repo normalizes reusable rules
- `policy-adoption-feedback-loop` governs how adopting repos capture feedback that can later be harvested
