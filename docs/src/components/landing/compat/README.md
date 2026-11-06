# Homepage primitives

The homepage keeps its shared layout, controls, artwork, styles, and browser checks together in this directory. Only add a variant when the page actually uses it.

- `button.tsx` provides the large rounded primary and secondary actions.
- `section.tsx` provides the section spacing and responsive headings.
- `layout.tsx` provides the page container and the two grid layouts.
- `styles.css` owns their typography, sizing, colors, and breakpoints.
- `brand-mark.tsx`, `brand-mark.module.css`, `check-circle-fill.tsx`, and `assets/` provide the artwork used by the illustrations and action buttons.
- `index.ts` also exposes the public tabs and check/copy icons.

Run `pnpm --filter docs test:routes` after a production build. `parity.test.mjs` checks the rendered page at mobile and desktop widths in light and dark themes, including heading metrics, button shape and sizing, hover states, grid placement, asset loading, overflow, and product switching. The checks run in both CI environments.

For a running server, set `DOCS_TEST_URL` and run the colocated test directly. `DOCS_BROWSER_CLI` can select an installed agent-browser executable. Update the expected measurements deliberately when changing the design.
