# Dashboard (packages/dashboard)

Instructions for AI coding agents working on the dashboard package.

## Component Library

Use shadcn/ui components for all UI primitives. Never use native browser dialogs
(`alert`, `confirm`, `prompt`). Use the shadcn/ui equivalents instead:

- `Dialog` or `AlertDialog` for modal dialogs
- `Toast` for notifications
- `Sheet` for side panels
- `Popover` for floating content

Components live in `src/components/ui/`. Import from `@/components/ui/<name>`.

## File Naming

Use param-case (kebab-case) for all file and folder names. Examples:

- `session-tree.tsx`, not `SessionTree.tsx`
- `browser-panel.tsx`, not `BrowserPanel.tsx`

The `ui/` directory follows shadcn conventions which already uses param-case.

## Project Structure

- `src/app/` — Next.js App Router pages and layouts
- `src/components/` — Shared React components
- `src/components/ui/` — shadcn/ui primitives (do not edit generated files)
- `src/hooks/` — Custom React hooks
- `src/lib/` — Utility functions and shared libraries
- `src/store/` — State management

## Commands

```bash
pnpm --filter dashboard dev     # Start development server
pnpm --filter dashboard build   # Production build
pnpm --filter dashboard start   # Start production server
```

## Dependencies

This package uses Next.js, React, Tailwind CSS, and shadcn/ui. When adding new
UI functionality, check whether a shadcn/ui component already exists before
creating a custom implementation.
