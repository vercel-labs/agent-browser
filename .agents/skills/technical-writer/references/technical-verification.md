# Technical verification

## Inspect the repository first

Verify implementation claims and examples in the current repository by default, without waiting for the user to request a fact check. Scale the investigation to the change: a wording edit that preserves a verified claim does not require a fresh audit of unrelated behavior.

1. Identify the package and version the documentation targets. Inspect relevant package manifests, workspace configuration, and release context. Distinguish the current checkout, installed dependency version, and published release; they may differ.
2. Locate the relevant files with `rg --files` and search exact symbols, option names, or error strings with `rg -n`. Start within the likely package or directory. Exclude generated output, vendored code, and unrelated packages unless they are needed to resolve the claim.
3. Trace the public import or re-export to its definition. Read the relevant signature and implementation with enough surrounding context to understand defaults, branches, validation, and errors. A search match or type declaration alone does not establish runtime behavior.
4. Inspect focused tests and maintained examples for the behavior being documented. Check the conditions they cover. Tests support a claim within their scope; missing tests do not prove a feature is unsupported, and a mock does not establish an external service's behavior.
5. Follow helper calls or dependencies only as far as needed to resolve the claim. Broaden the search when the current evidence leaves a specific question unanswered. Stop when the claim and its material qualifications are supported.
6. Validate the example with the smallest relevant existing check when practical. Inspect package scripts before choosing a command. Prefer a targeted type check or test over running the entire monorepo suite for a documentation edit.

Batch independent searches and reuse evidence for repeated claims about the same API. Keep a compact working record of the supporting paths, symbols, version, and unresolved questions. Recheck when the code, target version, or claim changes. Do not dump whole files or repeat broad searches when a bounded read will answer the question.

If the repository contains only documentation, locate its declared upstream implementation or use official references for the target version. Report conflicts between code and documentation instead of silently choosing whichever supports the draft. Keep source-inspection details in the review or handoff unless readers need them to understand the API.

## Match evidence to the claim

| Claim | Evidence to inspect |
| --- | --- |
| Implementation behavior | Relevant code, tests, and version or commit |
| Supported public interface | Public exports and types for the target version, checked against official API documentation |
| Release status or availability | Official changelog and current availability documentation |
| Planned behavior | Engineering specification, clearly identified as a proposal |
| Performance or scale | Original measurements, methodology, workload, and conditions |
| Another vendor's capability | That vendor's maintained documentation |

Resolve disagreements before stating a definitive claim. A newer source is not automatically the right source if it describes a different version or deployment environment. If implementation and public documentation disagree, state the precise discrepancy in notes and keep the draft within what can be supported.

Do not use arbitrary numerical precision to make prose sound authoritative. "Up to" does not make an invented limit acceptable, and "approximately" does not rescue an unsupported number.

For measurements, preserve the metric, baseline, unit, workload, and whether the number is vendor-reported. Distinguish latency from throughput, average from percentile, and relative improvement from percentage-point change.

## Product names and historical terminology

Verify the name used for the product and version being documented. Check current terminology instead of relying on a remembered replacement table.

Preserve capitalization such as Vercel, Next.js, GitHub, GitLab, Bitbucket, Turborepo, Turbopack, and AI SDK where those names apply. Use npm and pnpm in lowercase. Distinguish the Yarn brand from the `yarn` command. Keep generic concepts lowercase unless their position requires capitalization.

A rename does not establish equivalent behavior. Do not automatically substitute a new product name in an old API, migration path, or runtime explanation. Historical names can be necessary for users searching for an old error or migrating from an older system. Identify the relevant period or version and explain the verified replacement when one exists.

## Qualify scope

Check plan, region, runtime, framework version, deployment type, permissions, and feature status when they affect the claim. Keep the relevant qualification beside the instruction or fact.

Distinguish supported behavior, defaults, optional configuration, and behavior observed in one test. Do not generalize from a successful local run to every production environment.

Absence claims need evidence too. An unsuccessful search does not establish that a feature does not exist. Prefer a narrowly supported statement about the documented interface, and record what would trigger a recheck. Exclude roadmap promises from descriptions of available behavior. Use them only when the user explicitly requests roadmap content and a dated source supports the status.

For integrations, verify which package provides each capability. Distinguish SDK behavior from provider, adapter, framework, and runtime behavior. Do not imply that every integration supports a capability because one implementation does.

## Code and commands

Before including a sample, check:

1. The package, API, flags, imports, and configuration keys exist in the relevant version.
2. The snippet uses the documented mechanism in the correct file or execution context.
3. Required setup, dependencies, credentials, and permissions are explained or linked.
4. Placeholders are recognizable, consistent, and valid for the format after substitution.
5. Error handling or cleanup is sufficient for the task, without hiding the central example in unrelated infrastructure.
6. The surrounding text accurately describes what the code does and what it does not establish.

Use `your_project_id_here` or another descriptive placeholder when the format permits it. Use clearly fictional values for sample data. Avoid placeholder forms that the shell interprets as redirection or substitution. Use environment variables for credentials, and distinguish server-only values from values exposed to clients when relevant.

A complete example should be usable in the stated environment. A partial example must say which file it changes and what existing context it assumes. Do not label pseudocode or an incomplete fragment as ready to run.

Show expected output when it helps readers verify success. Mark variable fields and avoid presenting illustrative output as an actual test result. For asynchronous operations, explain what indicates completion and what the reader should inspect if it fails.

## SDK behavior

Check the installed or targeted package version before using a signature from another branch or release. Trace re-exports to the public entry point, and verify overloads, generic types, optional arguments, and inferred return types when they affect the example. Do not infer runtime validation from TypeScript types alone.

For asynchronous APIs, explain whether a call returns a value, promise, stream, or subscription. Document when work starts, how completion is observed, and how errors reach the caller. Include cancellation, retries, cleanup, and partial-result behavior when relevant and supported by evidence.

For stateful APIs, identify what persists, who owns the state, and how concurrent calls affect it. For callbacks and events, verify the payload, execution context, ordering guarantees, and whether handlers may run more than once. Avoid promising ordering or delivery guarantees the implementation does not provide.

Identify server and client boundaries in full-stack examples. Keep private credentials on the server, explain serialization requirements where relevant, and show how the client receives results. Verify runtime restrictions and framework-specific setup instead of treating all JavaScript environments as interchangeable.

## Validation

Use the smallest meaningful check: syntax validation, compilation, a relevant test, link verification, or a safe execution of the documented path. Do not execute destructive actions, incur charges, or publish content merely to validate prose without authorization.

Record what ran, the environment or version that matters, and the result. If execution is unavailable, review the code against primary sources and report that it was not run. Do not invent a successful result.

For troubleshooting content, confirm that the diagnostic command distinguishes the proposed cause from other plausible causes. Explain the fix and how to verify recovery. Never invent an exact error string.

## Sources and maintenance

Link the specific page supporting the claim. For repository evidence, include a stable revision when available. Paraphrase source material; quote briefly only when the wording matters.

Report unresolved claims and their supporting evidence in the review or handoff. Keep research history out of the documentation page. Include dates or versions in reader-facing text when they explain a compatibility boundary, release transition, or other relevant condition.
