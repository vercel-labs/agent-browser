# Policy | Validation And Handoff

## Policy

- Run the relevant validation for the touched surface before commit, handoff, or merge preparation.
- Prefer targeted verification that matches the changed area, and widen to broader suites when the impact is user-visible or cross-cutting.
- Include concrete pass/fail evidence in the handoff or closeout note.
- Keep handoff notes concise, explicit about remaining risk, and clear about the next recommended action.
- When live or manual smoke matters for the changed surface, record whether it was run and what it proved.
## Adoption Notes

Use this module when the repo:
- has multiple test or smoke surfaces with different scopes
- expects evidence-backed closeout notes
- needs clear verification and residual-risk communication before review or release
