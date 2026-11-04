'use client';

import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { BrandMark } from '../compat';
import { VercelArrow } from './vercel-cursor';

/** Extend the kit cursor without forking it. The label shares its transform,
 * so it cannot lag behind, flip sides, or change the pointer's hotspot.
 * `children` ride in the same layer, for something the cursor carries. */
export function StageAgentLabel({ children }: { children?: ReactNode }) {
  const anchor = useRef<HTMLSpanElement>(null);
  const [cursor, setCursor] = useState<HTMLElement | null>(null);
  const [pointer, setPointer] = useState<HTMLElement | null>(null);
  useLayoutEffect(() => {
    const target = anchor.current?.closest('.ss-stage')?.querySelector<HTMLElement>('.ss-cursor');
    // The cursor is a sibling owned by Stage; attach before the first paint.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCursor(target ?? null);
    // The Vercel arrow replaces the kit's, inside the kit's own pointer glyph
    setPointer(target?.querySelector<HTMLElement>('.ss-cursor__glyph[data-glyph="pointer"]') ?? null);
  }, []);
  return <>
    <span ref={anchor} hidden />
    {pointer && createPortal(<VercelArrow />, pointer)}
    {cursor && createPortal(<>
      <span className="ab-agent-presence" aria-hidden="true"><BrandMark name="agent" size={12} /></span>
      {children}
    </>, cursor)}
  </>;
}
