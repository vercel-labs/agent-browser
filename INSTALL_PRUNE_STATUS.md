# Chrome pruning work status

This is a fork-only checkpoint for branch `feat/install-prune-old-chrome`. Do not include this status file in an upstream pull request.

## Status

Work is paused pending maintainer direction in [upstream issue #1661](https://github.com/vercel-labs/agent-browser/issues/1661). No upstream pull request has been opened. The implementation prototype is preserved in commit `dc44a44` so work can resume without recreating it.

## Why confirmation is required

Repeated `agent-browser install` runs leave older Chrome for Testing releases under the managed browser directory. Those installations consume substantial disk space and there is no focused cleanup command.

The initially proposed contract was an explicit `agent-browser install --prune` operation that validates the manifest-resolved current Stable version, refuses while sessions are active, and removes only obsolete direct child directories matching `chrome-A.B.C.D`.

Deep upstream research found maintainer-authored branch `feat/chrome-version-select` at commit `742dfa7`. That branch makes multiple installed Chrome versions intentional by adding exact version and channel installation, `chrome list`, and per-run Chrome selection. Consequently, treating every non-current version as obsolete may delete deliberately installed Stable, Beta, Dev, Canary, or pinned versions. Existing directory names do not record why a version was installed, so the correct retention policy cannot be inferred safely.

Opening an implementation pull request before maintainers choose that policy would waste review time and risk conflicting with planned architecture.

## Research completed

The audit covered current upstream `main`, every open issue and pull request title and body, focused closed searches, upstream branches and commits, and all available repository Discussions. No earlier public prune proposal, implementation, or maintainer rejection was found.

Adjacent work read in full, including comments, reviews, and inline threads where present:

- PR #1254: doctor diagnostics and repair boundaries
- PR #1189: possible Chrome Headless Shell installation inside the same version directory
- PR #1472: cached-browser architecture and host architecture validation
- PR #1362 and predecessor #1297: XDG and configurable managed paths
- Issue #1361: XDG path requirements
- Issue #1306: full uninstall boundaries
- PRs #1296 and #1314: comparable install flags
- PR #1058: macOS Chrome archive symlink preservation
- Issue #1577: atomic and verified fallback downloads
- Issue #1108: possible global browser installation
- PR #871: possible Chrome manifest and download mirrors
- PR #1656: current install dependency work
- Upstream branch `feat/chrome-version-select`: planned multi-version lifecycle

## Prototype behavior

Commit `dc44a44` currently:

- Adds CLI flag `install --prune` and MCP boolean `prune`.
- Preserves behavior when pruning is absent.
- Fetches the current Stable manifest before any cleanup.
- Runs cleanup after an existing current executable is validated or a new extraction produces the expected executable.
- Refuses cleanup when `walk_daemons()` reports any active session and recommends `agent-browser close --all`.
- Uses `get_browsers_dir()` rather than a hardcoded cleanup path.
- Considers only direct, non-symlink directories whose names contain exactly four ASCII decimal components after `chrome-`.
- Retains the exact manifest-resolved current version.
- Preserves files, symlinks, malformed names, unknown entries, other engines, external caches, and user state.
- Reports retained, removed, and failed paths. Partial deletion failure exits nonzero after naming every failed path.
- Validates the extracted executable before reporting install success, which also prevents pruning after invalid extraction.
- Updates CLI help, MCP parity, README, core skill content and reference, docs installation page, docs commands page, inline documentation, and focused tests.

## Validation completed

The prototype passed:

```bash
cargo fmt --manifest-path cli/Cargo.toml -- --check
cargo clippy --manifest-path cli/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path cli/Cargo.toml
cargo run --quiet --manifest-path cli/Cargo.toml -- install --help
git diff --check
```

`pnpm` was unavailable in the local shell, so no docs-site build was run. Native end-to-end tests do not exercise installer cache lifecycle and were not run. Cross-platform behavior is covered by platform-neutral filesystem code plus conditional symlink tests, but Windows runtime validation remains outstanding.

## Maintainer decisions requested

Issue #1661 asks maintainers to confirm:

1. Whether managed Chrome pruning is wanted.
2. Whether the interface should be `install --prune`, `chrome prune`, or another shape.
3. Whether explicit and non-Stable installations must survive pruning.
4. What persistent metadata should identify intentionally retained versions, because existing directories are ambiguous.
5. Whether every active agent-browser session should block cleanup.
6. Whether doctor should only warn and recommend the explicit cleanup command.

## Resume procedure

1. Read all new issue #1661 comments and any linked issue, pull request, branch, or maintainer discussion in full.
2. Fetch upstream `main` and recheck for overlapping lifecycle work.
3. Rebase this branch onto current upstream `main`.
4. Revise or replace commit `dc44a44` according to confirmed retention and command semantics. Do not preserve the prototype merely because it already exists.
5. Remove both fork-only archive files, `INSTALL_PRUNE_GOAL.md` and `INSTALL_PRUNE_STATUS.md`, before opening an upstream pull request.
6. Rerun all repository-prescribed validation, including platform checks made necessary by the accepted design.
7. Push the revised branch and open an upstream pull request only after the issue provides sufficient direction.
8. Read and address top-level comments, submitted reviews, thread-aware inline discussions, and CI failures. Do not merge.
