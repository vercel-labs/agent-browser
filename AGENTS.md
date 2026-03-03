# AGENTS.md

Instructions for AI coding agents working with this codebase.

## Package Manager

This project uses **pnpm**. Always use `pnpm` instead of `npm` or `yarn` for installing dependencies, running scripts, etc. (e.g., `pnpm install`, `pnpm run build`).

## Code Style

- Do not use emojis in code, output, or documentation. Unicode symbols (✓, ✗, →, ⚠) are acceptable.
- CLI colored output uses `cli/src/color.rs`. This module respects the `NO_COLOR` environment variable. Never use hardcoded ANSI color codes.
- CLI flags must always use kebab-case (e.g., `--auto-connect`, `--allow-file-access`). Never use camelCase for flags (e.g., `--autoConnect` is wrong).

## Documentation

When adding or changing user-facing features (new flags, commands, behaviors, environment variables, etc.), update **all** of the following:

1. `cli/src/output.rs` -- `--help` output (flags list, examples, environment variables)
2. `README.md` -- Options table, relevant feature sections, examples
3. `skills/agent-browser/SKILL.md` -- so AI agents know about the feature
4. `docs/src/app/` -- the Next.js docs site (MDX pages)
5. Inline doc comments in the relevant source files

This applies to changes that either human users or AI agents would need to know about. Do not skip any of these locations.

In the `docs/src/app/` MDX files, always use HTML `<table>` syntax for tables (not markdown pipe tables). This matches the existing convention across the docs site.

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
