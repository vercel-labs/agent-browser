# Safe managed Chrome pruning goal

This fork-only document archives the original goal and its present disposition. Keep it with branch `feat/install-prune-old-chrome` while upstream issue #1661 awaits maintainer direction. Remove this file and `INSTALL_PRUNE_STATUS.md` before opening any upstream pull request.

## Original objective

Implement safe pruning of obsolete agent-browser-managed Chrome for Testing versions, then create a pull request against `vercel-labs/agent-browser`. The pull request must not be merged by this work.

`agent-browser install` stores releases in versioned directories under the central managed browser directory, conventionally:

```text
chrome-A.B.C.D
```

Stable updates accumulate old installations indefinitely. Users need an explicit, safe way to retain the current release without deleting profiles, rollback data outside this managed cache, other browsers, external caches, or unrelated state.

The requested initial interface was:

```bash
agent-browser install --prune
```

Cleanup must remain opt-in. Existing install behavior must remain unchanged without `--prune`.

## Required semantics

1. Resolve the current Chrome version through the existing Chrome for Testing manifest.
2. Install the current version or verify that its expected executable already exists.
3. Prune only after successful installation or validation.
4. When the current version is already installed, `install --prune` must still prune rather than returning early.
5. Use `get_browsers_dir()` or its future central replacement. Never hardcode `~/.agent-browser` in cleanup logic.
6. Consider only direct child directories with the exact managed shape `chrome-A.B.C.D`. All four components must be decimal integers.
7. Preserve the exact current resolved version, unknown files and directories, symlinks, malformed or partial names, Puppeteer and Playwright caches, system Chrome installations, profiles, auth data, sessions, screenshots, traces, other state, and other browser engines.
8. Treat the manifest-resolved current version as authoritative. Never choose the retained version through lexicographic directory ordering.
9. Never prune after metadata, download, checksum, extraction, or validation failure.
10. Handle active agent-browser sessions safely. The preferred initial policy was to refuse or skip with a clear recommendation to run `agent-browser close --all`. Never silently terminate sessions or delete a directory used by a running daemon.
11. Make every deletion failure visible through a useful error or named partial-success report.
12. Report the retained current version and removed obsolete versions.
13. Make repeated `install --prune` runs idempotent.
14. Support macOS, Linux, and Windows.
15. Follow existing install JSON conventions and preserve CLI/MCP parity.

## Doctor boundary

Do not make `doctor --fix` the primary cleanup interface. A focused doctor warning may report multiple valid managed versions and recommend the explicit prune command. Generic doctor repair must not silently delete versions without maintainer direction.

## Architecture and documentation constraints

- Follow the repository `AGENTS.md` completely.
- Keep the implementation in Rust.
- Keep CLI flags kebab-case.
- Use `cli/src/color.rs`; never hardcode ANSI sequences.
- Update MCP behavior and tests with every CLI semantic change.
- Update `cli/src/output.rs`, `README.md`, `skill-data/core/SKILL.md`, relevant core skill references, `docs/src/app/`, and inline source documentation.
- Keep docs-site tables in HTML form.
- Avoid unrelated refactors and release, version, or changelog edits.
- Remain compatible with possible XDG/configurable storage and Chrome Headless Shell sharing a version directory.

## Required test coverage

The requested test matrix includes:

1. Recognize `chrome-151.0.7922.76`.
2. Reject malformed names.
3. Retain the exact resolved current version.
4. Remove older valid managed versions.
5. Handle numerically tricky versions such as `.9`, `.34`, and `.76` without lexical retention logic.
6. Preserve unknown directories.
7. Preserve symlinks.
8. Preserve a file whose name resembles a Chrome version.
9. Delete nothing when only one version exists.
10. Treat a missing browser directory as harmless.
11. Remain idempotent.
12. Never prune after failed or invalid current installation.
13. Still prune when the current version was already installed.
14. Never prune without the opt-in flag.
15. Prevent destructive cleanup during active sessions.
16. Report deletion failures correctly.
17. Accept `install --prune` through the CLI parser.
18. Keep help and MCP surfaces aligned.
19. Keep JSON behavior stable if install supports it.
20. Use the upstream path resolver on every platform.

Minimum validation requested:

```bash
cd cli
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Relevant end-to-end and platform-specific checks should run where available. Environment-limited checks must be disclosed.

## Original upstream workflow

1. Inspect current `main`, repository instructions, all overlapping issues and pull requests, and relevant prior implementation work.
2. Confirm no new overlapping pruning work exists.
3. Use an issue first when repository policy or unresolved lifecycle design warrants it.
4. Implement on a narrow feature branch.
5. Keep commits focused and reviewable.
6. Push the branch and open a pull request against upstream `main` only after design is justified.
7. Explain the reproduction, upstream ownership, safety boundary, opt-in policy, running-session behavior, XDG and Headless Shell compatibility, tests, and manual verification in the pull request.
8. Read top-level comments, submitted reviews, thread-aware inline discussions, and CI failures after pushing.
9. Address all actionable feedback.
10. Do not merge.

## Research changed the immediate goal

The upstream audit found maintainer-authored branch `feat/chrome-version-select` at commit `742dfa7`. It adds exact version and channel installation, `chrome list`, and per-run Chrome selection. Under that planned architecture, multiple managed versions can be deliberate. Removing every version except the current Stable release could destroy explicitly installed or pinned versions.

No public issue, pull request, review, or Discussion contained a maintainer decision about pruning or how intentional versions should be identified. Directory names alone cannot distinguish automatic Stable leftovers from deliberately installed versions.

To respect maintainer time, no upstream pull request was opened. Instead, [issue #1661](https://github.com/vercel-labs/agent-browser/issues/1661) requests confirmation of:

- Whether pruning belongs in the project.
- Whether the command should be `install --prune`, `chrome prune`, or another interface.
- Whether intentionally installed Stable, Beta, Dev, Canary, or pinned versions must survive.
- What metadata should identify versions retained by user intent.
- Whether any active agent-browser session should block cleanup.
- Whether doctor should only warn and recommend the explicit operation.

## Archived implementation state

- Personal fork: `https://github.com/donbeave/agent-browser`
- Local and fork branch: `feat/install-prune-old-chrome`
- Prototype commit: `dc44a44`
- Research/status commit: `f13b380`
- Upstream confirmation issue: `https://github.com/vercel-labs/agent-browser/issues/1661`
- Upstream pull request: none

The prototype implements the original `install --prune` interpretation and passed Rust formatting, clippy with warnings denied, the full Rust unit suite, install help rendering, and `git diff --check`. `INSTALL_PRUNE_STATUS.md` records implementation details, completed research, validation limitations, and the resume procedure.

## Current disposition

This goal is intentionally closed as an archived checkpoint, not claimed as an accepted upstream feature. Future work begins only after meaningful maintainer direction appears on issue #1661 or linked upstream work. At that time, treat the response as a new goal, reread all linked context, fetch current upstream state, and revise or discard the prototype according to the confirmed architecture.
