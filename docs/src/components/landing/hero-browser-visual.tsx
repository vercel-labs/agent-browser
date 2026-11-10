'use client';

import { BrowserDesktopScene } from './hero-scene/browser-desktop-scene';

/** The animated browser or desktop scene selected by the hero, in the same
 * frame as the homepage use-case cards: a 1px gray-alpha-400 hairline with
 * 2px corners, on the page's own background. Where the hero stacks (below the
 * page's @lg container size) it runs edge to edge: it reaches past the page's
 * 24px padding, and keeps only its top and bottom hairlines, since side
 * borders and corners would sit on the screen's edge. */
export function HeroBrowserVisual({ product }: { product: 'browser' | 'desktop' }) {
  return (
    <div className="overflow-hidden rounded-[2px] border border-gray-alpha-400 bg-background-200 @max-lg:-mx-6 @max-lg:rounded-none @max-lg:border-x-0">
      <BrowserDesktopScene desktop={product === 'desktop'} />
    </div>
  );
}
