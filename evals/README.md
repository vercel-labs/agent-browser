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
