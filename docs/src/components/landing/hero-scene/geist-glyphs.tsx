import type { ReactNode } from 'react';

/**
 * Geist icons and the Apple logo for the hero's desktop, vendored as inline
 * SVG: the paths are copied verbatim from @vercel/geistcn-assets 2.3.0
 * (icons/icon-globe, icon-image, icon-location, icon-trash and
 * logos/logo-icon-apple-svg), which the docs package does not depend on.
 * Each draws in currentColor on Geist's 16px grid.
 */
function Glyph({ size = 16, children }: { size?: number; children: ReactNode }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" style={{ display: 'block' }}>
      {children}
    </svg>
  );
}

export function IconGlobe({ size }: { size?: number }) {
  return <Glyph size={size}><path fillRule="evenodd" clipRule="evenodd" d="M10.27 14.1a6.5 6.5 0 0 0 3.67-3.45q-1.24.21-2.7.34-.31 1.83-.97 3.1M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16m.48-1.52a7 7 0 0 1-.96 0H7.5a4 4 0 0 1-.84-1.32q-.38-.89-.63-2.08a40 40 0 0 0 3.92 0q-.25 1.2-.63 2.08a4 4 0 0 1-.84 1.31zm2.94-4.76q1.66-.15 2.95-.43a7 7 0 0 0 0-2.58q-1.3-.27-2.95-.43a18 18 0 0 1 0 3.44m-1.27-3.54a17 17 0 0 1 0 3.64 39 39 0 0 1-4.3 0 17 17 0 0 1 0-3.64 39 39 0 0 1 4.3 0m1.1-1.17q1.45.13 2.69.34a6.5 6.5 0 0 0-3.67-3.44q.65 1.26.98 3.1M8.48 1.5l.01.02q.41.37.84 1.31.38.89.63 2.08a40 40 0 0 0-3.92 0q.25-1.2.63-2.08a4 4 0 0 1 .85-1.32 7 7 0 0 1 .96 0m-2.75.4a6.5 6.5 0 0 0-3.67 3.44 29 29 0 0 1 2.7-.34q.31-1.83.97-3.1M4.58 6.28q-1.66.16-2.95.43a7 7 0 0 0 0 2.58q1.3.27 2.95.43a18 18 0 0 1 0-3.44m.17 4.71q-1.45-.12-2.69-.34a6.5 6.5 0 0 0 3.67 3.44q-.65-1.27-.98-3.1" /></Glyph>;
}

export function IconImage({ size }: { size?: number }) {
  return <Glyph size={size}><path d="m16 8.44.56.56-.56.56v2.94l-.01.26a2.5 2.5 0 0 1-2.23 2.23l-.26.01h-11l-.26-.01a2.5 2.5 0 0 1-2.23-2.23L0 12.5v-1.69l-.06-.06.06-.06V1h16zm-8 .12 2.06 2.06L9 11.68 6.32 9H3.8L1.5 11.31v1.19a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V9.06L11 5.56zm-6.5.63L3.19 7.5h3.75l3.53-3.53h1.06l2.97 2.97V2.5h-13z" /></Glyph>;
}

export function IconLocation({ size }: { size?: number }) {
  return <Glyph size={size}><path fillRule="evenodd" clipRule="evenodd" d="M11.53 10.26 8 13.86l-3.53-3.6a5.2 5.2 0 0 1 0-7.27 4.9 4.9 0 0 1 7.06 0 5.2 5.2 0 0 1 0 7.27m1.07 1.05-3.55 3.62L8 16l-1.05-1.07L3.4 11.3a6.7 6.7 0 0 1 0-9.37 6.4 6.4 0 0 1 9.2 0 6.7 6.7 0 0 1 0 9.37M9 6.5a1 1 0 1 1-2 0 1 1 0 0 1 2 0M8 9a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5" /></Glyph>;
}

export function IconTrash({ size }: { size?: number }) {
  return <Glyph size={size}><path fillRule="evenodd" clipRule="evenodd" d="M6.75 2.75a1.25 1.25 0 1 1 2.5 0V3h-2.5zM5.25 3v-.25a2.75 2.75 0 0 1 5.5 0V3H15v1.5h-1.12l-.7 9.2a2.5 2.5 0 0 1-2.5 2.3H5.32c-1.31 0-2.4-1-2.5-2.3l-.7-9.2H1V3h4.25m-.93 10.58-.7-9.08h8.76l-.7 9.08a1 1 0 0 1-1 .92H5.32a1 1 0 0 1-1-.92" /></Glyph>;
}

export function LogoApple({ size }: { size?: number }) {
  return <Glyph size={size}><path d="M14.1 5.45a3.6 3.6 0 0 0-1.73 3.05 3.5 3.5 0 0 0 2.14 3.24q-.38 1.21-1.1 2.27c-.68.98-1.4 1.97-2.49 1.97-1.08 0-1.36-.63-2.62-.63-1.22 0-1.66.65-2.65.65-1 0-1.7-.91-2.5-2.03a10 10 0 0 1-1.66-5.3C1.49 5.56 3.5 3.9 5.5 3.9c1.06 0 1.94.7 2.6.7.64 0 1.62-.74 2.82-.74a3.8 3.8 0 0 1 3.18 1.58m-3.75-2.9c.53-.62.83-1.41.85-2.23q0-.15-.03-.32a3.6 3.6 0 0 0-3.23 3.38q0 .15.03.3l.22.01a3 3 0 0 0 2.16-1.14" /></Glyph>;
}
