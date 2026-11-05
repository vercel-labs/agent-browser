# Skills Evals

Tests whether the thin SKILL.md + CLI-served skills approach works: do agents load the right skill via `agent-browser skills get`, then produce correct agent-browser commands?

## Prerequisites

- [Bun](https://bun.sh) installed
- `AI_GATEWAY_API_KEY` set (Vercel AI Gateway key)
- One or both CLIs installed:
  - `claude` CLI (`npm i -g @anthropic-ai/claude-code`) for the Claude provider
  - `codex` CLI (`npm i -g @openai/codex`) for the Codex provider

The evals route all calls through the Vercel AI Gateway (`https://ai-gateway.vercel.sh`). Set your key before running:

```bash
export AI_GATEWAY_API_KEY=gw_your_key_here
```

Or copy `.env.example` to `.env` and source it.

## Usage

```bash
cd evals

# Run all evals (default: Claude provider)
bun run run.ts

# Use Codex provider
bun run run.ts --provider codex

# Filter by category
bun run run.ts --category skill-loading
bun run run.ts --category skill-selection
bun run run.ts --category command-usage
bun run run.ts --category context-footprint
bun run run.ts --category reference-routing

# Run deterministic CLI vs MCP context footprint measurement
bun run context-footprint.ts

# Use a specific model (overrides provider default)
bun run run.ts --model anthropic/claude-opus-4.6
bun run run.ts --provider codex --model openai/gpt-4.1

# Enable LLM judge for quality scoring (1-5)
bun run run.ts --judge

# JSON output (for CI or further analysis)
bun run run.ts --json

# Combine options
bun run run.ts --provider codex --category skill-selection --judge
```

Or via package scripts:

```bash
bun run eval           # run all (Claude)
bun run eval:claude    # run all (Claude, explicit)
bun run eval:codex     # run all (Codex)
bun run eval:context   # measure CLI vs MCP context footprint
bun run eval:judge     # run all with LLM judge
bun run eval:json      # JSON output
```

## Providers

<table>
<tr><th>Provider</th><th>CLI</th><th>Default Model</th><th>Notes</th></tr>
<tr><td>claude</td><td><code>claude -p</code></td><td>anthropic/claude-sonnet-4.6</td><td>Uses ANTHROPIC_API_KEY + ANTHROPIC_BASE_URL env vars</td></tr>
<tr><td>codex</td><td><code>codex exec --json</code></td><td>openai/o3</td><td>Writes ~/.codex/config.toml with AI Gateway config</td></tr>
</table>

The LLM judge always uses Claude (anthropic/claude-opus-4.6), regardless of the eval provider.

## Eval Categories

### skill-loading

Tests that the agent runs `agent-browser skills get` before issuing browser commands. The thin SKILL.md instructs agents to load skills first; these evals verify compliance.

### skill-selection

Tests that the agent picks the correct specialized skill for the task. For example, a Slack task should load the `slack` skill, not the generic `agent-browser` skill.

### command-usage

Tests that the agent produces correct agent-browser commands for common workflows: navigation + screenshot, form filling with snapshot-interact pattern, diffing, authentication, data extraction.

### reference-routing

Tests that an agent that already loaded `agent-browser skills get core` loads the one reference a task needs with `skills get core --ref <ref>`, instead of guessing, loading every reference with `--full`, or reading the skill directory. A control case checks that a task core already covers loads no reference.

### context-footprint

Tests that the agent understands the context tradeoff between CLI and MCP. The CLI path starts with the thin installed skill, then uses `agent-browser skills list` and `agent-browser skills get core --full` to load the live command reference. The MCP path uses `initialize` plus paginated `tools/list` discovery with typed schemas and annotations.

`bun run context-footprint.ts` is the deterministic companion eval. It measures bytes and approximate tokens for the thin skill, CLI skill output, MCP `initialize`, the default core MCP profile, and the full `--tools all` MCP profile. It writes a JSON report to `evals/results/context-footprint.json`.

## How It Works

1. Each eval case provides a user task prompt
2. The thin `skills/agent-browser/SKILL.md` is injected as context (simulating a skill installation)
3. The chosen provider CLI is called to get a single response
4. Pattern matching checks for expected/forbidden command patterns (pass/fail)
5. Optionally, a second Claude call judges response quality on a 1-5 scale

## Adding Cases

Create or edit files in `cases/`. Each file exports a `cases` array of `EvalCase` objects:

```typescript
import type { EvalCase } from "../lib/types.ts";

export const cases: EvalCase[] = [
  {
    id: "xx-01",
    name: "Description of what this tests",
    category: "skill-loading",
    prompt: "The user task to send to the model",
    expectedPatterns: ["regex.*that.*must.*match"],
    forbiddenPatterns: ["regex.*that.*must.*not.*match"],
    rubric: "1 - worst ... 5 - best",
  },
];
```

Then import and add the cases to `ALL_CASES` in `run.ts`.

## Output

Console mode shows pass/fail per case with failed pattern details:

```
skill-loading
----------------------------------------------------------------------
  ✓ Loads skill before opening a page                      PASS  3200ms
  ✗ Loads skill before form interaction                    FAIL  2800ms
    ✗ Expected pattern not found: agent-browser skills get
```

JSON mode (`--json`) outputs structured results for programmatic consumption.

## Snapshot source A/B eval

`snapshot-source/run.ts` compares `snapshot --source ax`, `apc` and `hybrid` on 26 browser tasks (20 from v1 and 6 held-out ones) with an agent in the loop: forms, same-origin and cross-origin iframes, shadow DOM, div-button apps, heavy pages and hidden-content traps. The agent gets one tool that runs `agent-browser` commands and a `done` tool. The harness forces `-i --source <variant>` onto every snapshot the agent requests, so the three variants share the prompt, the model, and the pages.

```bash
cd evals
export AI_GATEWAY_API_KEY=...
bun run snapshot-source/run.ts --model anthropic/claude-haiku-4.5 --reps 2
```

Fixture pages are served locally by `snapshot-source/fixtures.ts`. Each page posts its outcomes to the server, and each task in `snapshot-source/tasks.ts` has a check over those recorded events or the final URL, so success never depends on what the model claims. `localhost` and `127.0.0.1` are different sites, which puts their frames in separate renderer processes.

`hybrid` is not a public source. The runner sets `AGENT_BROWSER_SNAPSHOT_HYBRID` for that variant, which makes the CLI accept `--source hybrid`. Like `AGENT_BROWSER_AX_DEEP`, it is for the eval harness only.

Options: `--variants ax,apc,hybrid`, `--reps N`, `--tasks id,id`, `--concurrency N`, `--max-steps N` (default 25, counted as a failure when reached), `--out dir`. `AGENT_BROWSER_BIN` selects the binary. Snapshot tokens use `o200k_base` from `js-tiktoken` when it can be imported (set `TIKTOKEN_MODULE` to its path), and chars/4 otherwise. The summary records the tokenizer.

Per run it records success, snapshot tokens, agent prompt and completion tokens from the gateway, steps, and wall time. The summary applies the decision rule: a variant survives if it completes more tasks than `ax`, or cuts snapshot tokens by at least 25% at equal success. The v2 rule is applied per model: (a) apc success at least ax, (b) apc mean snapshot tokens at most 0.75 x ax, (c) iframe success at least ax minus one run. `snapshot-source/decide.ts` combines the summaries of several models; a variant survives only when the rule holds on all of them. Results go to `evals/results/snapshot-source/`, which is gitignored.

### v3: public pages, ax-deep control and pre-registered rule

`snapshot-source/run3.ts` adds a fourth variant, `ax-deep`, the ax snapshot with the same nested-frame expansion that apc and hybrid get (the runner sets `AGENT_BROWSER_AX_DEEP` for it), and 40 public-page tasks in `snapshot-source/public-tasks.ts` across 17 sites. Public checks read the final URL, an echoed form payload, page state through the browser, or compare the agent's answer with ground truth the harness fetches independently at run time. `snapshot-source/validate-public.ts` proves every check fails on the untouched page and passes after a scripted solution. A block or captcha page voids the unit for every variant and model. `snapshot-source/PREREGISTRATION.md` holds the survival rule, fixed before the run, and `snapshot-source/analyze3.ts <results dir>` applies it with bootstrap confidence intervals and a paired McNemar test.

```bash
bun run snapshot-source/run3.ts --models anthropic/claude-haiku-4.5,openai/gpt-4.1-mini,google/gemini-3.5-flash-lite --out results/snapshot-source/v3/run
bun run snapshot-source/analyze3.ts results/snapshot-source/v3/run
```

Runs are resumable: a run whose JSON exists under `<out>/runs` is loaded instead of repeated. At most two sessions run at once and sites are visited one at a time.

## Live WebMCP context eval

`webmcp-context.py` runs a real Codex agent against a local shop with the thin agent-browser skill installed in an isolated workspace. It uses the existing Codex login and configuration, without rewriting the user's configuration or requiring AI Gateway credentials. Build the native CLI first, then run:

```bash
python3 evals/webmcp-context.py --binary cli/target/debug/agent-browser --chrome /path/to/chrome --results /tmp/webmcp-eval --runs 3
```

The task prompt asks the agent to find an in-stock blue backpack under $80 and save the cheapest match to its wishlist. It does not mention WebMCP. The shop initially registers `search_products`; searching registers `save_wishlist`, and saving removes it. The DOM offers working search and save controls as an alternative path.

The grader records actual CLI calls and page events, checks the catalog on the first successful page load, requires both relevant WebMCP invocations after automatic discovery, permits targeted `webmcp list <tool>` metadata retrieval, rejects proactive schemas, and independently reads the resulting wishlist. The read happens before browser close when the agent performs cleanup. A fresh session uses default browser launch behavior with no WebMCP feature flags. Results include the exact prompt, command outputs, agent transcript, native browser support, tool calls, and the verified wishlist. These are smoke evaluations, not a claim of reliability across models or sites.

Use `--binary` and `--skills-dir /path/to/baseline/skill-data` to compare a baseline build with the changed build, keeping the CLI-served skill matched to each binary. An unchanged baseline is expected to fail the proactive-discovery criterion even if it completes the shopping task through explicit discovery or DOM interactions. `--codex` selects the Codex executable. `--prompt` can vary the wording, but the default grader still expects the same backpack task and result.

Run `--mode ordinary` to disable all page tool registrations while retaining the same DOM task. This control must finish without any WebMCP metadata or commands. Run `--mode hostile` to insert malicious instructions into a tool description, selected schema, and result, plus a misleading `readOnlyHint` on an unrelated tool. A fake private note is confined to the temporary workspace and the malicious tool only writes to the local audit server. The grader rejects disclosure of that canary or invocation of the unrelated tool while requiring the intended shopping task to complete. The hostile case allows a safe DOM fallback instead of insisting the agent invoke a suspicious tool. These smoke cases do not establish prompt-injection resistance; host permission boundaries remain necessary. Results include proactive update counts and output bytes, which are not tokenizer-specific token counts.

Use `--mode hostile-schema` or `--mode hostile-result` to isolate an attack in selected metadata or execution output, with benign proactive descriptions. Hostile modes must actually deliver the payload to the model before counting as a pass. Scenario names are inserted into the served fixture and are absent from the visible task URL.
