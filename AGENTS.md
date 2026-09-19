# AGENTS.md

Instructions for AI coding agents working with this codebase.

## Package Manager

This project uses **pnpm**. Always use `pnpm` instead of `npm` or `yarn` for installing dependencies, running scripts, etc. (e.g., `pnpm install`, `pnpm run build`).

## Code Style

- Do not use emojis in code, output, or documentation. Unicode symbols (✓, ✗, →, ⚠) are acceptable.
- In documentation and markdown, never use double hyphens (`--`) as a dash. Use an emdash (—) sparingly when needed. Prefer rewriting the sentence to avoid dashes entirely.
- CLI colored output uses `cli/src/color.rs`. This module respects the `NO_COLOR` environment variable. Never use hardcoded ANSI color codes.
- CLI flags must always use kebab-case (e.g., `--auto-connect`, `--allow-file-access`). Never use camelCase for flags (e.g., `--autoConnect` is wrong).

## Documentation

Do not hard-wrap prose in documentation files such as Markdown, MDX, and READMEs; let the editor or renderer wrap text naturally.

When adding or changing user-facing features (new flags, commands, behaviors, environment variables, etc.), update **all** of the following:

1. `cli/src/output.rs` — `--help` output (flags list, examples, environment variables)
2. `README.md` — Options table, relevant feature sections, examples
3. `skill-data/core/SKILL.md` (and its `references/`) — so AI agents know about the feature when they load the core skill. Edit `skill-data/core/SKILL.md` for overview/workflow changes; edit `skill-data/core/references/*.md` for detailed reference content. Do **not** put feature content in `skills/agent-browser/SKILL.md` — that file is an intentionally thin discovery stub for `npx skills add` and exists only to redirect agents to `agent-browser skills get core`.
4. `docs/content/docs/` — the Geistdocs site (MDX pages); routing and API handlers live in `docs/src/app/`
5. Inline doc comments in the relevant source files

This applies to changes that either human users or AI agents would need to know about. Do not skip any of these locations.

## CLI/MCP Parity

When adding or changing any CLI command, flag, behavior, output, environment variable, or parser semantics, update the MCP server in `cli/src/mcp.rs` in the same change. MCP tools should stay in sync with canonical CLI behavior by delegating through the normal CLI parser where possible. If a CLI command has no dedicated MCP tool, add one or document why it is intentionally omitted. Add or update tests that prove the CLI and MCP surfaces remain aligned.

In the `docs/content/docs/` MDX files, always use HTML `<table>` syntax for tables (not markdown pipe tables). This matches the existing convention across the docs site. Page titles and descriptions live in frontmatter; do not duplicate the title as an H1 in the body.

For documentation changes, run `pnpm --filter docs test`, `pnpm --filter docs type-check`, `pnpm --filter docs build`, and `pnpm --filter docs test:routes`. The route suite starts its own production server. Its frozen migration fixtures cover the original public routes, metadata, content, anchors, and API contracts; only update affected fixtures when deliberately changing that contract.

The Vercel docs project uses `docs` as its Root Directory and must enable **Include source files outside of the Root Directory in the Build Step** (`sourceFilesOutsideRootDirectory`). Installation requires the repository's `pnpm-workspace.yaml`, root `pnpm-lock.yaml`, and `patches/`. Keep the docs `packageManager` pin aligned with the root and use the Corepack commands in `docs/vercel.json`; do not create a separate docs lockfile or disable frozen installs.

## Dashboard (packages/dashboard)

- Never use native browser dialogs (`alert`, `confirm`, `prompt`). Use shadcn/ui components (`Dialog`, `AlertDialog`, etc.) instead.
- Use param-case (kebab-case) for all file and folder names (e.g., `session-tree.tsx`, not `SessionTree.tsx`). The `ui/` directory follows shadcn conventions which already uses param-case.

## Releasing

Releases are manual, single-PR affairs. There is no changesets automation. The maintainer controls the changelog voice and format.

To prepare a release:

1. Create a branch (e.g. `prepare-v0.24.0`)
2. Bump `version` in `package.json`
3. Run `pnpm version:sync` to update `cli/Cargo.toml`, `cli/Cargo.lock`, and `packages/dashboard/package.json`
4. Write the changelog entry in `CHANGELOG.md` at the top, under a new `## <version>` heading, wrapped in `<!-- release:start -->` and `<!-- release:end -->` markers. Remove the `<!-- release:start -->` and `<!-- release:end -->` markers from the previous release entry so only the new release has markers.
5. Add a matching entry to `docs/content/docs/changelog.mdx` at the top (below the frontmatter)
6. Open a PR and merge to `main`

When the PR merges, CI compares `package.json` version to what's on npm. If it differs, it builds all 7 platform binaries, publishes to npm, and creates the GitHub release automatically. The GitHub release body is extracted from the content between the `<!-- release:start -->` and `<!-- release:end -->` markers in `CHANGELOG.md`.

### Writing the changelog

Review the git log since the last release and write the entry in `CHANGELOG.md`. Follow the existing format and voice. Group changes under `### New Features`, `### Bug Fixes`, `### Improvements`, etc. Bold the feature/fix name, then describe it concisely. Reference PR numbers in parentheses.

Wrap the release notes (everything between the `## <version>` heading and the previous version) in markers so CI can extract them for the GitHub release. Only the current release should have markers; remove the `<!-- release:start -->` and `<!-- release:end -->` markers from any previous release entry:

```markdown
## 0.24.1

<!-- release:start -->
### Bug Fixes

- Fixed **baz** not working when qux is enabled (#1235)

### Contributors

- @ctate
<!-- release:end -->

## 0.24.0

### New Features

- **Foo command** - Added `foo` command for bar (#1234)
```

Include a `### Contributors` section listing the GitHub usernames (with `@` prefix) of everyone who contributed to the release. Check the git log between the previous tag and HEAD to find them.

Do not prefix entries with commit hashes. Do not use the changesets `### Patch Changes` / `### Minor Changes` headings. Use descriptive section names instead.

### Docs changelog

The docs changelog at `docs/content/docs/changelog.mdx` mirrors `CHANGELOG.md` but uses a slightly different format. Each entry uses:

- A `v` prefix on the version (e.g. `## v0.24.0`)
- A date line with the full date: `<p className="text-[#888] text-sm">March 30, 2026</p>`
- A `---` separator between entries

Match the existing style in that file.

## Architecture

This is a Rust codebase. The browser automation daemon lives in `cli/src/native/` (daemon, actions, browser, CDP client, snapshot, state). The `--engine` flag selects Chrome vs Lightpanda. The `install` command downloads Chrome from Chrome for Testing directly.

## Testing

### Unit Tests

```bash
cd cli && cargo test
```

Runs all unit tests (~320 tests). These are fast and don't require Chrome.

### End-to-End Tests

```bash
cd cli && cargo test e2e -- --ignored --test-threads=1
```

Runs 18 e2e tests that launch real headless Chrome instances and exercise the full native daemon command pipeline. Requirements:

- Chrome must be installed
- Must run serially (`--test-threads=1`) to avoid Chrome instance contention
- Tests are `#[ignore]`'d so they don't run during normal `cargo test`

The e2e tests live in `cli/src/native/e2e_tests.rs` and cover: launch/close, navigation, snapshots, screenshots, form interaction, cookies, storage, tabs, element queries, viewport/emulation, domain filtering, diff, state management, error handling, and Phase 8 commands.

### Linting and Formatting

```bash
cd cli && cargo fmt -- --check   # Check formatting
cd cli && cargo clippy            # Lint
```

<!-- opensrc:start -->

## Source Code Reference

Source code for dependencies is available in `opensrc/` for deeper understanding of implementation details.

See `opensrc/sources.json` for the list of available packages and their versions.

Use this source code when you need to understand how a package works internally, not just its types/interface.

### Fetching Additional Source Code

To fetch source code for a package or repository you need to understand, run:

```bash
npx opensrc <package>           # npm package (e.g., npx opensrc zod)
npx opensrc pypi:<package>      # Python package (e.g., npx opensrc pypi:requests)
npx opensrc crates:<package>    # Rust crate (e.g., npx opensrc crates:serde)
npx opensrc <owner>/<repo>      # GitHub repo (e.g., npx opensrc vercel/ai)
```

<!-- opensrc:end -->
