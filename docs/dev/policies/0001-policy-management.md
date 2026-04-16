# Policy | Policy Management

## Policy

- When a repo adopts shared policy, install the policy library before running selection or adoption workflows.
- Enumerate available profiles, modules, and catalog metadata deterministically from the installed policy library rather than relying on chat history or sibling checkout layout.
- Keep the adopted repo-local policy under `docs/dev/policies/`.
- Keep `AGENTS.md` as the entrypoint that wires the adopted repo-local policy into the repo contract.
- Treat `AGENTS.md` as a policy-loading contract, not just a static pointer.
- Treat repo-local policy as one section of `AGENTS.md`, not the whole file.
- Keep repo-specific commands, environment prerequisites, and operating constraints in `AGENTS.md` or adjacent local docs even after shared policy is installed.
- Keep `AGENTS.md` thin relative to the full durable policy body; do not turn it into the full policy dump if the repo can keep policy files under `docs/dev/policies/`.
- Re-read the relevant adopted policy files at the start of any non-trivial turn.
- Re-read the relevant adopted policy files when task scope changes mid-session.
- Treat policy installation, policy enumeration, and `AGENTS.md` wiring as deterministic setup work rather than ad hoc prose copying.
- When the repo uses an installable selector bundle, ensure the selector ships with the policy library it depends on.
## Adoption Notes

Use this module as the first adopted policy when a repo is managed through the shared policy selector workflow.
