'use client';

import { useLayoutEffect, type RefObject } from 'react';

/**
 * Keep borders inside a scaled stage at one real pixel, like the Geist card
 * around it. A fitted stage scales its design (0.9×, 1.12×…), which turns a
 * 1px border into a soft line that reads heavier than the card's crisp 1px.
 * Sets `--ss-hairline: calc(1px / scale)` on the host; borders use
 * `var(--ss-hairline, 1px)`. Also sets `--ss-unscale: 1 / scale` (a plain
 * number), for things drawn at true screen pixels whatever the fit: cursors
 * and their labels.
 */
export function useHairline(host: RefObject<HTMLElement | null>, deps: readonly unknown[] = []) {
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    let raf = 0;
    const update = () => {
      cancelAnimationFrame(raf);
      // After the stage has applied its own fit for this size
      raf = requestAnimationFrame(() => {
        const stage = el.querySelector<HTMLElement>('.ss-stage');
        if (!stage) return;
        const transform = getComputedStyle(stage).transform;
        const scale = transform && transform !== 'none' ? new DOMMatrixReadOnly(transform).a : 1;
        el.style.setProperty('--ss-hairline', `${(1 / (scale || 1)).toFixed(4)}px`);
        el.style.setProperty('--ss-unscale', (1 / (scale || 1)).toFixed(4));
      });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => { observer.disconnect(); cancelAnimationFrame(raf); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
