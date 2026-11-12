'use client';

import { type KeyboardEvent, type RefObject, useLayoutEffect, useRef, useState } from 'react';
import { PREVIEW_URL, PRODUCTION_URL, PR_NUMBER, PR_TITLE } from './hero-timeline';

export type HeroTab = 'production' | 'preview' | 'github';

const ORDER: readonly HeroTab[] = ['production', 'preview', 'github'];
const NAMES: Record<HeroTab, string> = {
  production: `Production, ${PRODUCTION_URL}`,
  preview: `Preview, ${PREVIEW_URL}`,
  github: `Pull request, ${PR_TITLE} ${PR_NUMBER}`,
};

type Box = { left: number; top: number; width: number; height: number };

/**
 * Reduced motion only: the still hero's browser tabs, made real. The scene is
 * aria-hidden (its figcaption tells the story), so these buttons sit outside
 * it, over the mock tabs' measured boxes, as one tablist: click or arrow
 * through Production, the Preview and the pull request to see each page as it
 * stands in the still. Selecting one only changes which tab is active; nothing
 * plays, seeks or moves the camera. The mock's close buttons and + stay
 * decorative (they would need a story the still doesn't have).
 */
export function HeroTabPicker({ host, open, selected, onSelect, measureKey }: {
  host: RefObject<HTMLElement | null>;
  /** Which tabs the still has open, in strip order. */
  open: Record<HeroTab, boolean>;
  selected: HeroTab;
  onSelect: (tab: HeroTab) => void;
  /** Re-measure when this changes (the scope, the layout). */
  measureKey: string;
}) {
  const tabs = ORDER.filter(tab => open[tab]);
  // A stable key for the open tabs (the object itself is new every render)
  const openKey = tabs.join(' ');
  const [boxes, setBoxes] = useState<Partial<Record<HeroTab, Box>>>({});
  const buttons = useRef<Partial<Record<HeroTab, HTMLButtonElement | null>>>({});

  useLayoutEffect(() => {
    const figure = host.current;
    if (!figure) return;
    let frame = 0;
    const measure = () => {
      const origin = figure.getBoundingClientRect();
      const strip = figure.querySelectorAll<HTMLElement>('.ab-browser .ss-window__tab');
      const next: Partial<Record<HeroTab, Box>> = {};
      const shown = openKey.split(' ');
      ORDER.forEach((tab, index) => {
        const el = strip[index];
        if (!el || !shown.includes(tab)) return;
        const r = el.getBoundingClientRect();
        if (r.width < 1 || r.height < 1) return;
        next[tab] = { left: r.left - origin.left, top: r.top - origin.top, width: r.width, height: r.height };
      });
      setBoxes(next);
    };
    // The still settles over the first frames (the stage's fit, the layout),
    // so measure after them, and again on any resize
    const later = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(() => { frame = requestAnimationFrame(measure); }); };
    measure();
    later();
    const observer = new ResizeObserver(later);
    observer.observe(figure);
    return () => { observer.disconnect(); cancelAnimationFrame(frame); };
  }, [host, openKey, measureKey]);

  const move = (event: KeyboardEvent<HTMLButtonElement>, from: HeroTab) => {
    const at = tabs.indexOf(from);
    const to = event.key === 'ArrowRight' ? tabs[(at + 1) % tabs.length]
      : event.key === 'ArrowLeft' ? tabs[(at - 1 + tabs.length) % tabs.length]
        : event.key === 'Home' ? tabs[0]
          : event.key === 'End' ? tabs[tabs.length - 1]
            : null;
    if (!to) return;
    event.preventDefault();
    onSelect(to);
    buttons.current[to]?.focus();
  };

  return (
    <div className="ab-tab-picker" role="tablist" aria-label="Browser tabs in the illustration">
      {tabs.map(tab => {
        const box = boxes[tab];
        return (
          <button
            key={tab}
            ref={el => { buttons.current[tab] = el; }}
            type="button"
            role="tab"
            aria-selected={selected === tab}
            aria-label={NAMES[tab]}
            tabIndex={selected === tab ? 0 : -1}
            className="ab-tab-pick"
            data-measured={box ? true : undefined}
            style={box}
            onClick={() => onSelect(tab)}
            onKeyDown={event => move(event, tab)}
          />
        );
      })}
    </div>
  );
}
