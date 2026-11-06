'use client';

import { BrowserDesktopScene } from './hero-scene/browser-desktop-scene';

/** The animated browser or desktop scene selected by the hero. */
export function HeroBrowserVisual({ product }: { product: 'browser' | 'desktop' }) {
  return (
    <div className="border border-gray-alpha-400">
      <BrowserDesktopScene desktop={product === 'desktop'} />
    </div>
  );
}
