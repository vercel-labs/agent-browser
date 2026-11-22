# agent-browser daemon benchmarks

`bench.ts` compares command latency and system metrics between two agent-browser builds inside a [Vercel Sandbox](https://vercel.com/docs/sandbox) microVM: the latest `agent-browser` release from npm, and a Rust build of a repository branch.

The harness was written to compare the Node.js daemon with the Rust native daemon. Releases since 0.23.0 ship only the Rust daemon, and phase 1 installs the latest npm release without pinning a version, so both phases now measure a Rust daemon. To measure the Node.js daemon, change `installNodeDaemon` in `bench.ts` to install a release older than 0.23.0. The default branch for phase 2 is `ctate/native-2`; pass `--branch main` to build current code.

## What it measures

Command latency for each scenario in `scenarios.ts`, with warmup, measured iterations, and standard deviation:

- `navigate`: page load round trip
- `snapshot`: accessibility tree generation
- `screenshot`: viewport capture
- `evaluate`: JavaScript execution
- `click`: element interaction
- `fill`: form input
- `agent-loop`: a snapshot, click, snapshot cycle
- `full-workflow`: a 7-command sequence

System metrics collected while the daemon runs:

- Cold start time, from daemon spawn to browser ready
- Binary size and total distribution size, including the browser download
- Daemon RSS and peak RSS, separate from browser process memory
- Browser RSS for the Chrome processes
- Daemon CPU time
- Process counts

## Prerequisites

- Node.js and pnpm
- Vercel Sandbox credentials: token, team ID, and project ID

## Setup

```bash
cd benchmarks
pnpm install
cp .env.example .env
```

Fill in your Vercel Sandbox credentials in `.env`:

```
SANDBOX_VERCEL_TOKEN=your_token
SANDBOX_VERCEL_TEAM_ID=your_team_id
SANDBOX_VERCEL_PROJECT_ID=your_project_id
```

## Usage

```bash
pnpm bench                             # 10 iterations, 1 warmup, 8 vCPUs
pnpm bench -- --iterations 20          # more iterations for tighter stats
pnpm bench -- --warmup 2               # extra warmup iterations
pnpm bench -- --json                   # write results.json
pnpm bench -- --branch main            # build phase 2 from another branch
pnpm bench -- --vcpus 16               # more vCPUs for a faster Rust build
```

## How it works

1. Creates a Vercel Sandbox and installs Chromium system dependencies.
2. Phase 1 installs `agent-browser` from npm, runs every scenario, and collects metrics.
3. Phase 2 installs the Rust toolchain, clones the repository at `--branch`, runs `cargo build --release`, replaces the npm binary with the build, and runs the same scenarios.
4. Prints comparison tables and, with `--json`, writes `results.json`.

## Interpreting results

Command latency is dominated by Chrome DevTools Protocol round trips, so per-command differences between daemons are usually small. Use the standard deviation column to separate real differences from noise. Cold start, daemon RSS, and distribution size show differences in the daemon itself, because both phases drive the same Chrome.
