# Jev support: research notes

Date: 2026-11-08. Scope: whether and how agent-browser should call TypeSafe's Jev model. No source code was changed.

Convention check: no research-note or ADR directory exists in the repo. The glob for `research*`, `docs/**/*adr*`, and `docs/**/*design*` returned nothing, and `docs/` holds only `docs/tests/README.md` outside the Geistdocs site. This file is therefore a new `docs/research/jev-support.md`.

## Summary

- Use Jev as an opt-in fallback for one judgment code cannot make: picking an element from a snapshot when `find` locators miss or match several. Selectors, refs, waits, policy checks, and action execution stay in code.
- Call the HTTP API with the `reqwest` client the crate already depends on. No Rust SDK exists, so this is a small typed client in a new `cli/src/native/jev.rs`, called from the daemon where refs live.
- Gate it at runtime with an env var plus explicit per-command use. The crate has no Cargo features and no optional dependencies, and its existing opt-in features are runtime gates.
- Treat egress as the main risk. Snapshots leave the machine. Ship with input-value redaction, a size cap, an origin allowlist for Jev calls, and docs that state what is sent. The existing `--allowed-domains` filter covers page traffic, not the daemon's calls to the API.
- Run a go/no-go spike on saved snapshots before writing product code. Neither the Jev docs nor this repo has accuracy, latency, or calibration data for agent-browser snapshots.

## What Jev does

Primitives ([primitives](https://docs.typesafe.ai/primitives.md), [choice](https://docs.typesafe.ai/primitives/choice.md), [noul](https://docs.typesafe.ai/primitives/noul.md), [score](https://docs.typesafe.ai/primitives/score.md)):

| Primitive | Returns | Fit for agent-browser |
| --- | --- | --- |
| Choice | `choice`, `probabilities` (all options), `confidence` | Pick one ref or one user-supplied key from a fixed list |
| Noul | `noul` (probability of yes, 0 to 1). No `confidence` field. | Yes/no page-state questions |
| Score | `score`, `legend`, `probabilities`, `confidence` | Grade or rank a small set on ordered levels |
| Box | Edges of a box in an image | Out of scope. Image input needs the `TypeSafe-Danger-Zone-Images` header and is "dreamer-ready" ([basic usage](https://docs.typesafe.ai/sdk/python/usage/basic.md)). |

API facts:

- Endpoint: `POST https://api.typesafe.ai/v1/systemone` with `Authorization: Bearer <API_KEY>` and a JSON body with `model`, `state`, and `questions` ([quickstart](https://docs.typesafe.ai/introduction/quickstart.md), [OpenAPI](https://api.typesafe.ai/openapi.json), reached via [System One](https://docs.typesafe.ai/api/system-one.md)).
- Choice accepts 1 to 255 options (`ChoiceQuestion.criteria.maxProperties: 255` in the OpenAPI spec). Score accepts at most 10 levels (`ScoreQuestion.criteria.maxItems: 10`).
- Body up to 32 MiB, at most 64 nesting levels, finite numbers only. Errors: 400, 401 (bad key), 413, 415, 422 (malformed question), 429 (rate limit), 529 (overloaded) ([errors](https://docs.typesafe.ai/api/errors.md)).
- Context: 64k tokens per request and 32k per question by default. The long-context header raises both to 262k ([models](https://docs.typesafe.ai/models.md)).
- Price: $42 per Btok, which is $0.042 per Mtok of input. Output tokens are free ([models](https://docs.typesafe.ai/models.md)). At that rate a 10k-token snapshot costs about $0.0004. That figure is arithmetic from the listed price, not a measurement.
- Latency: "Most calls complete in about 100 ms" ([how to build](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md)). This is a vendor claim we have not measured.
- Models: `jev-latest` points to `jev-1.14.0`. `jev-preview` currently points to the same model. Each response reports the versioned ID that answered. Pin a versioned ID if thresholds are tuned against one release ([models](https://docs.typesafe.ai/models.md)).
- Rate limits are set per organization in the console. SDKs retry 408, 429, and 5xx by default and honor `Retry-After`. Raw HTTP clients should back off ([retries](https://docs.typesafe.ai/sdk/python/api/retries.md), [errors](https://docs.typesafe.ai/api/errors.md)).
- Keys: the SDKs strip surrounding whitespace and reject empty keys, internal whitespace, control characters, and non-ASCII characters ([basic usage](https://docs.typesafe.ai/sdk/python/usage/basic.md)). Apply the same check.
- Retention: "Jev is not trained on customer requests or responses." Zero data retention is for enterprise customers, requested through a support form ([models](https://docs.typesafe.ai/models.md), [legal](https://docs.typesafe.ai/legal.md)).

Known weaknesses that shape the design ([model jaggedness](https://docs.typesafe.ai/model-jaggedness.md)):

- Reads literally. Write exact instructions.
- Does not count reliably, handle numbers or dates well, or interpolate between Score levels. Keep arithmetic in code.
- Accuracy falls as state grows with irrelevant detail. Filter before sending.
- Leans toward the first Choice option. Reorder options and check consistency.
- Page content is data, not hostile by default. Injected text can move an answer.
- Does not generate text.

## Candidate features mapped to primitives

| Candidate | Primitive | Fit | Cost and latency | Main risk | Verdict |
| --- | --- | --- | --- | --- | --- |
| Pick a ref from a natural-language description when `find` misses or is ambiguous | Choice over pre-filtered refs, plus a companion Noul asking whether any candidate matches | Good. Deterministic locators handle the common case. | One call per use. Input is the candidate list. | Wrong click. Page labels can steer the answer. Order bias. | Phase 1 |
| Check a page condition after an action (for example, "payment failed message shown") | Noul over visible text | Weak as a pass/fail assertion. Useful as a hint in exploratory runs. | One call per check. Polling multiplies cost. | Probabilities are not guarantees. Values between 0.3 and 0.7 are ambiguous. | Later. Never as a CI assertion. Call once after a deterministic wait. |
| Rank search or extraction results | Score per result, or Choice over a short list | Good for choosing among a few results. Poor for counting. | One request can carry many questions. | Score is not numeric. Context grows with result count. | Phase 4, after Phase 1 data |
| Map form fields to user-supplied values | Choice whose options are the user's key names, one question per field | Strong. Code copies the value, as in the [pre-parsed value extraction cookbook](https://docs.typesafe.ai/cookbooks/pre_parsed_value_extraction_cookbook.md). | One call per field or one batched request. | Ambiguous labels. Values leaking into state. | Phase 3. Best privacy profile if only key names are sent. |
| Natural language to agent-browser commands | Choice over a closed command set (as in the [function calling cookbook](https://docs.typesafe.ai/cookbooks/function_calling.md)) | Poor. The command surface is open-ended, and `chat` already uses an LLM for this. | n/a | Generation is outside Jev's design. | Not planned |
| Classify the page state (login wall, error page, empty results) for diagnostics | Choice or Noul over the snapshot | Fair. Low action risk. | One call per classification. | Low value unless it changes behavior. | Optional, Phase 2 |

Keep in code: ref resolution (`RefMap`, `cli/src/native/element.rs`), `find role|text|label` lookups, waits, counts, dates and numeric thresholds, auth vault values, cookies, `--allowed-domains` and action policy checks, and the final action.

Design notes for the ref case:

- Option names and descriptions are both sent to the model ([choice](https://docs.typesafe.ai/primitives/choice.md)). Use the ref ID as the option name and role plus accessible name as the description.
- Duplicate role and name pairs already get an `nth` index in the snapshot (`cli/src/native/snapshot.rs:288-320`, `509-520`). Include it in the description so duplicates stay distinct.
- Option order matters, so a deterministic order (DOM order) plus a reversed-order check on near-threshold answers is worth measuring.
- A companion Noul ("does any candidate match?") is the documented alternative to a "none of the above" option ([choice](https://docs.typesafe.ai/primitives/choice.md)).
- Act on the probability of the chosen option, not on `confidence` alone. For a Choice, `confidence = (p_max - 1/n) / (1 - 1/n)`, so it is a function of the top probability and the option count ([confidence](https://docs.typesafe.ai/confidence.md)). Starting Noul thresholds are 0.7 for yes and 0.3 for no ([noul](https://docs.typesafe.ai/primitives/noul.md)). Choice thresholds need tuning per domain.
- Adversarial labels are the main attack path. A page that labels a button "Submit (correct choice)" can move the answer. Jev can only return an offered ref, so the worst case is a wrong ref. Limit the blast radius with the action policy (see Data and privacy).

## Integration design (Rust)

### HTTP client

- No Rust SDK. The SDK pages cover Python and JavaScript, and say you can "call the HTTP API directly from any language" ([SDKs](https://docs.typesafe.ai/sdk.md)).
- Use the existing `reqwest` dependency (`cli/Cargo.toml:28`, with `json`, `rustls-tls-webpki-roots`, and `stream` features, default features off). No new crate is needed.
- Build the client with `crate::tls::apply_to_reqwest(reqwest::Client::builder(), &crate::tls::process_options())`, as `cli/src/read.rs:212` and `cli/src/upgrade.rs:20` do. This honors `--ca-cert` and system CA settings (`cli/src/tls.rs:223-234`). The chat client uses `reqwest::Client::new()` (`cli/src/native/stream/chat.rs:13-15`), which skips those options. Do not copy it.
- Typed layer: request builders for Choice, Noul, and Score; response structs tagged by `type`; an error enum that maps HTTP status to variants and never includes the key or the raw response body.

### Where it runs

- Daemon side (`cli/src/native/`). Refs live in `DaemonState.ref_map` (`cli/src/native/actions.rs:567`), and snapshots are built in `cli/src/native/snapshot.rs:319`. A daemon-side call can reuse the snapshot without another round trip. Dispatch is in `execute_command` (`actions.rs:2976`, action match at `3535-3656`).
- Precedent exists on both sides: `cli/src/chat.rs` (CLI process) and `cli/src/native/stream/chat.rs` (daemon).
- `find` path: `parse_find` validates locators (`cli/src/commands.rs:3297-3316`). `find role` emits `getbyrole` (`commands.rs:3374`). The handler dispatches at `actions.rs:3648` and builds a temp ref `e{N}` (`actions.rs:11250-11252`). Whether the chained subaction (click) re-runs policy checks was not traced. Check before shipping.

### Configuration

- Repo convention: CLI behavior uses `AGENT_BROWSER_*` (`cli/src/flags.rs:684-818`). Vendor keys are unprefixed: `AI_GATEWAY_API_KEY`, `BROWSERBASE_API_KEY`, `KERNEL_API_KEY` (`cli/src/doctor/providers.rs:32-37`, `100`).
- Proposed: `TYPESAFE_API_KEY` for the key and `TYPESAFE_BASE_URL` for an override. Both match the SDK names ([quickstart](https://docs.typesafe.ai/introduction/quickstart.md), [SDK constants](https://docs.typesafe.ai/sdk/python/api/constants.md)). Default base URL is `https://api.typesafe.ai`. Default model `jev-latest`.
- Proposed gate: `AGENT_BROWSER_JEV=1`. Parsed with `env_var_is_truthy` like `AGENT_BROWSER_JSON` (`flags.rs:684-686`).
- Do not add Jev keys to `PROJECT_CONFIG_ALLOWED_KEYS` (`cli/src/flags.rs:11-23`). The automatic `./agent-browser.json` is repo-controlled and filtered to that list (`flags.rs:280-309`). Letting a repo turn on third-party egress would be a trust-boundary failure ([trust boundaries](../../skill-data/core/references/trust-boundaries.md), line 52).
- The daemon receives settings through `cmd.env` (`cli/src/connection.rs:600-669`). It inherits the parent environment. Confirm this for every launch path, including dashboard-started daemons.

### Secrets

- Send the key only in the `Authorization` header. Never put it in argv, URLs, JSON output, error strings, or debug logs. The `--debug` path was not audited for header dumps.
- `doctor` reports presence only, like `providers.chat` (`cli/src/doctor/providers.rs:100-118`). If the base URL is overridden, show its host.
- Follow the repo's rule: "Never echo, paste, cat, write, or emit a secret value." ([trust boundaries](../../skill-data/core/references/trust-boundaries.md), lines 30-43.)

### Color and output

- Colored output goes through `cli/src/color.rs` only. Colors are off by default and `NO_COLOR` disables them (`cli/src/color.rs:1-28`). AGENTS.md: "Never use hardcoded ANSI color codes."

## Opt-in and feature flags

- `cli/Cargo.toml` has no `[features]` table and no `optional = true` dependency. Opt-in is runtime: `env_var_is_truthy` (`flags.rs:684-686`) and chat's env presence check (`cli/src/chat.rs:19-33`, `cli/src/native/stream/chat.rs:17-19`).
- Recommendation: two runtime keys. `AGENT_BROWSER_JEV=1` enables the feature, and the user opts in per command by writing `find ai ...`. Do not enable on API key presence alone. Chat does that (`stream/chat.rs:17-19`), which is acceptable because the user runs `chat` on purpose. A Jev call inside `find` would send page content without a separate step.
- Compile-time feature: not recommended. Installs from npm, Homebrew, and Cargo would differ, and the repo has no such precedent. `reqwest` is already linked, so the binary change is small. Size was not measured.

## Repo rules that apply

Quoted from the repo AGENTS.md (`/Users/flippy/.t3/worktrees/agent-browser/t3-ff66d97b/AGENTS.md`) unless noted:

1. CLI and MCP parity: "When adding or changing any CLI command, flag, behavior, output, environment variable, or parser semantics, update the MCP server in `cli/src/mcp.rs` in the same change." A new `find` locator is a parser change. Update the `find` tool schema (`TOOL_FIND` at `cli/src/mcp.rs:73`) and the call mapping. Parity tests are at `mcp.rs:5935-5964` and `5997`.
2. Feature docs: update `cli/src/output.rs` (`--help`), `skill-data/core/references/*.md`, `docs/content/docs/`, and inline doc comments. For skills: "Do **not** put feature content in `skills/agent-browser/SKILL.md`." Put flags and edge cases in the matching reference, and "add a routing row when you add a reference." The routing table is at `skill-data/core/SKILL.md:53-65`. The routing test is `cli/src/skills.rs:809`.
3. README: change it only when install, quick start, or the command groups table changes.
4. MDX tables: "always use HTML `<table>` syntax for tables (not markdown pipe tables)." Also: "do not duplicate the title as an H1 in the body."
5. Docs checks: run `pnpm --filter docs test`, `pnpm --filter docs type-check`, `pnpm --filter docs build`, and `pnpm --filter docs test:routes`.
6. Dashes: "In documentation and markdown, never use double hyphens (`--`) or em dashes as dashes. Rephrase the sentence instead."
7. Prose: "Do not hard-wrap prose in documentation files."
8. Flags: "CLI flags must always use kebab-case (e.g., `--auto-connect`, `--allow-file-access`). Never use camelCase for flags."
9. Colors: "CLI colored output uses `cli/src/color.rs`... Never use hardcoded ANSI color codes."
10. Writing: "Use the `technical-writer` skill... when you write or edit prose for this repo, including docs pages, READMEs, changelog entries, commit messages, PR descriptions, and issues."
11. Testing: `cd cli && cargo test` for unit tests. E2E tests are `#[ignore]`d and run with `cd cli && cargo test e2e -- --ignored --test-threads=1`, which requires Chrome and serial execution.
12. Emojis: none in code, output, or documentation.
13. Global instructions (`~/.dotfiles/.config/opencode/AGENTS.md`): be extremely concise in interactions and commit messages.

## Data and privacy

What the slice-1 call would send:

| Data | Sent? | Control |
| --- | --- | --- |
| Page URL and title (snapshot header) | Yes | Origin allowlist decides whether the call happens |
| Roles, accessible names, visible text | Yes | Size cap and budget (context is 32k per question) |
| Input values (AX `value`) | Should not be | Snapshot prints `value_text` with no password-type check (`cli/src/native/snapshot.rs:1340-1343`). Redact before building the request. |
| Link hrefs (only with `-u`) | Only if the user asks | Omit by default |
| The agent's natural-language description | Yes | Visible to the caller |
| Cookies, storage, auth vault, state files | Never | Not part of the payload builder |
| Network and console bodies, eval output | Never in slice 1 | Not part of the payload builder |
| Screenshots | Never | Out of scope (image input is gated) |

Other facts:

- The default endpoint is `https://api.typesafe.ai`. A base URL override sends the same data to a gateway: OpenRouter, Vercel AI Gateway, or Pydantic AI Gateway ([basic usage](https://docs.typesafe.ai/sdk/python/usage/basic.md), "Configuring the base URL").
- The existing `--allowed-domains` filter blocks non-allowed page requests, WebSocket, EventSource, and sendBeacon traffic (`docs/content/docs/security.mdx:213`; [trust boundaries](../../skill-data/core/references/trust-boundaries.md), line 66). It does not cover the daemon's HTTP calls to Jev. Decide whether Jev calls must obey it.
- Precedent: `chat` posts its message list to the AI Gateway (`cli/src/chat.rs:132`). The chat docs do not say what page data leaves ([commands](../content/docs/commands.mdx), lines 555-575). The Jev docs should be explicit.

Controls to ship:

1. Two-key opt-in (see above).
2. Origin allowlist for Jev calls (for example `AGENT_BROWSER_JEV_ALLOWED_DOMAINS`). Refuse on other origins.
3. Redaction: send labels and key names, not input values. Never send password fields.
4. Size cap: truncate the candidate list and snapshot to a token budget, and refuse calls that would exceed the context limit.
5. Audit: log origin, model ID, call count, and token usage from the response. Never log content.
6. Visibility: `doctor` shows the endpoint host. Docs state retention terms and point enterprise users to ZDR.
7. Action policy: Jev-chosen destructive actions (submit, delete, pay) go through the existing confirmation policy (`cli/src/native/policy.rs:19-59`; `AGENT_BROWSER_CONFIRM_ACTIONS` in `flags.rs`). Prompt injection can then only cause a wrong ref, and destructive actions still need confirmation.

Untrusted-content rule: page text is untrusted ([trust boundaries](../../skill-data/core/references/trust-boundaries.md), lines 7-18). Jev's answer is data, not instructions. The code must not follow any text from the model.

## Testing plan

Unit tests (`cargo test` in `cli/`, no network):

- Config: env parsing, using the `EnvGuard` pattern in `cli/src/flags.rs` tests (around line 2371).
- Request builder: at most 255 options, option names are ref IDs, companion Noul present, no input values in the payload, snapshot truncated to budget, deterministic order.
- Response parsing: fixtures copied from the OpenAPI examples and the quickstart raw response. Cover Choice, Score, Noul, and 400 and 422 bodies.
- Error mapping: 401, 422, 429, 529, and connection failure. Assert that neither the key nor the body appears in the error string.
- Routing: table tests for act, suggest, and fail. Include the threshold boundary and ties.
- HTTP mock: a `TcpListener::bind("127.0.0.1:0")` server, as in `cli/src/read.rs:1423-1445`, with `TYPESAFE_BASE_URL` pointed at it. Assert the `Authorization` header and JSON shape. Assert no retry loop on 4xx.
- MCP parity: schema and call mapping for the new `find` locator (`cli/src/mcp.rs:5935-5964`, `5997`).
- Doctor: presence-only output, following `cli/src/doctor/providers.rs`.
- Skills: `cli/src/skills.rs:809` if a reference file is added.

E2E tests (`#[ignore]`, serial, in `cli/src/native/e2e_tests.rs` style):

- `e2e_jev_find_ai_stub`: real Chrome on a fixture page from `cli/src/native/test_fixtures/`, with a stub Jev server on localhost returning a fixed Choice. Assert the click hit the chosen ref. Run: `cd cli && cargo test e2e_jev -- --ignored --test-threads=1`.
- `e2e_jev_live`: additionally requires `AGENT_BROWSER_JEV_LIVE_TEST=1` and `TYPESAFE_API_KEY`. Assert response shape and that answers are among the offered options. Do not assert exact answers, because the model can change ([models](https://docs.typesafe.ai/models.md)).

Offline evaluation (not in CI):

- Build 50 to 100 saved snapshots with labeled target refs, outside the repo or in a git-ignored scratch folder. Keep secrets out.
- Measure top-1 accuracy, accuracy by probability bucket (calibration for thresholds), sensitivity to option order ([model jaggedness](https://docs.typesafe.ai/model-jaggedness.md), "Choice option order"), repeat-run stability ([self-consistency cookbook](https://docs.typesafe.ai/cookbooks/consistency_choice_cookbook.md)), p50 and p95 latency, and tokens per call.
- Define pass criteria before running.

## Phased plan

Phase 0, spike (no repo change): a Python SDK script over saved snapshots. Decide go or no-go from accuracy, calibration, order sensitivity, latency, and token cost.

Phase 1, smallest useful slice: `find ai "<description>" [action] [text]`.

- `cli/src/native/jev.rs` (new): config, request and response types, client, routing rules, error mapping.
- `cli/src/native/mod.rs`: add `pub mod jev;` to the module list (lines 1-54).
- `cli/src/commands.rs`: add `ai` to the `parse_find` locator list (around line 3298), add a usage string (around line 3313), and emit a new `getbyai` action.
- `cli/src/native/actions.rs`: dispatch arm near line 3648, and a handler near `handle_getbyrole` (line 11213).
- `cli/src/mcp.rs`: `TOOL_FIND` schema (line 73), call mapping, and parity tests.
- `cli/src/output.rs`: `find` help text and the environment variable list (around lines 4740-4803).
- `cli/src/doctor/providers.rs`: presence check (follow lines 100-118).
- Unit tests in the new module and the touched test modules.

Phase 2, docs and rollout:

- `docs/content/docs/commands.mdx` (find section), `docs/content/docs/configuration.mdx` (environment table, HTML rows near line 337), and `docs/content/docs/security.mdx` (egress). A new page would need a navigation entry, which was not inspected.
- `skill-data/core/references/commands.md` (find and environment variables), `skill-data/core/references/trust-boundaries.md` (Jev egress rule), and a new `skill-data/core/references/jev.md` with a routing row in `skill-data/core/SKILL.md`. The core loop does not change, so the workflow section stays as is.
- Origin allowlist and audit log.

Phase 3: label-only form mapping (Choice over user key names), and an exploratory assertion helper that calls Jev once after a deterministic wait.

Phase 4: Score ranking for `read` and search results. Revisit intent routing, which is likely out of scope.

## Open questions for the user

1. Should agent-browser ship a feature that sends page snapshots to a third-party API at all, even opt-in? This is the go or no-go.
2. Which first feature: ref selection (`find ai`, recommended) or label-only form mapping (lower data risk, narrower use)?
3. Env var names: `TYPESAFE_API_KEY` (vendor and SDK name, recommended) or `AGENT_BROWSER_JEV_API_KEY`?
4. Gate: env var only, or env var plus a per-command flag? The recommendation is both.
5. Model: `jev-latest` (moves with releases) or a pinned versioned ID (stable thresholds)?
6. Should a Jev choice ever run a click or fill automatically, or should it return the ref and let the agent act? Recommendation: auto-run only above threshold, and only for non-destructive actions.
7. Should Jev calls be blocked when `--allowed-domains` is set? Recommendation: block unless explicitly allowed, since the allowlist is meant to contain egress.
8. Should base URL overrides (gateways) be allowed? Each one widens the set of parties that see page content.
9. Is enterprise zero data retention required before the docs recommend Jev for sensitive sites?
10. Which MCP profile should expose it: core, or debug alongside `chat` (`cli/src/mcp.rs:469`)?
11. Should there be a per-session call budget?
12. Locator name: `ai` (recommended, matches the AI Gateway naming) or `jev`?

## Gaps and unverified items

- Accuracy, latency, and cost on agent-browser snapshots: not measured.
- Whether policy checks re-run for `find` subactions: the path from `handle_getbyrole` to the subaction was not traced to the end.
- Whether Chrome exposes password values in the accessibility tree: not checked. `snapshot.rs:1340-1343` prints whatever value it receives.
- The `find` help block in `cli/src/output.rs` was not read. Locate it with a search before editing.
- Whether every daemon launch path inherits `TYPESAFE_API_KEY`: only `connection.rs:600-669` was read.
- Not fetched from the Jev docs: `box`, `images`, most patterns, most cookbooks, and the `docs/content/docs` navigation configuration.
- The Jev docs say "As of November 14, 2026" for image support ([basic usage](https://docs.typesafe.ai/sdk/python/usage/basic.md)). That date is later than the environment date (2026-11-08). Treat it as unverified. Images are out of scope here.
- Prices and rate limits can change. Rate limits are set in the console, not in the docs ([models](https://docs.typesafe.ai/models.md)).

## Sources

Jev docs fetched on 2026-11-08: [llms.txt](https://docs.typesafe.ai/llms.txt), [quickstart](https://docs.typesafe.ai/introduction/quickstart.md), [System One API](https://docs.typesafe.ai/api/system-one.md) (OpenAPI at [api.typesafe.ai/openapi.json](https://api.typesafe.ai/openapi.json)), [errors](https://docs.typesafe.ai/api/errors.md), [Choice](https://docs.typesafe.ai/primitives/choice.md), [Noul](https://docs.typesafe.ai/primitives/noul.md), [Score](https://docs.typesafe.ai/primitives/score.md), [primitives](https://docs.typesafe.ai/primitives.md), [confidence](https://docs.typesafe.ai/confidence.md), [models](https://docs.typesafe.ai/models.md), [how to build](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md), [state](https://docs.typesafe.ai/concepts/state.md), [coding agents](https://docs.typesafe.ai/introduction/coding-agents.md), [model jaggedness](https://docs.typesafe.ai/model-jaggedness.md), [SDKs](https://docs.typesafe.ai/sdk.md), [Python constants](https://docs.typesafe.ai/sdk/python/api/constants.md), [Python basic usage](https://docs.typesafe.ai/sdk/python/usage/basic.md), [Python retries](https://docs.typesafe.ai/sdk/python/api/retries.md), [JavaScript ENV](https://docs.typesafe.ai/sdk/javascript/api/variables/ENV.md), [legal](https://docs.typesafe.ai/legal.md), and the cookbooks [function calling](https://docs.typesafe.ai/cookbooks/function_calling.md), [pre-parsed value extraction](https://docs.typesafe.ai/cookbooks/pre_parsed_value_extraction_cookbook.md), and [citation check](https://docs.typesafe.ai/cookbooks/citation_check.md).

Repo files read: `cli/Cargo.toml`, `cli/src/chat.rs`, `cli/src/native/stream/chat.rs`, `cli/src/native/mod.rs`, `cli/src/native/actions.rs`, `cli/src/native/snapshot.rs`, `cli/src/native/policy.rs`, `cli/src/native/parity_tests.rs`, `cli/src/commands.rs`, `cli/src/flags.rs`, `cli/src/connection.rs`, `cli/src/mcp.rs`, `cli/src/output.rs`, `cli/src/color.rs`, `cli/src/tls.rs`, `cli/src/read.rs`, `cli/src/doctor/providers.rs`, `skill-data/core/SKILL.md`, `skill-data/core/references/trust-boundaries.md`, `skill-data/core/references/snapshot-refs.md`, `docs/content/docs/commands.mdx`, `docs/content/docs/security.mdx`, `docs/content/docs/configuration.mdx`, `AGENTS.md`.
