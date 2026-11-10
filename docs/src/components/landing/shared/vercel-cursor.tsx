import './vercel-cursor.css';

/**
 * The pointer shared by the hero and the use-case cards: the stageshow kit
 * arrow's shape, made angular. Symmetric about the 45° diagonal from the tip
 * (3, 3): wings 15 along it (the kit's depth) and 6.99 either side (the tip
 * opens at 50°, the kit's is ~46°), the notch 78% of the way in (the kit's is
 * ~79%; it opens at about 129°). Straight, mitered edges instead of the
 * kit's rounded ones, and a crisp 1px light edge (a 2px stroke under the
 * fill) so it stays legible on black buttons, with no blur.
 */
export const VERCEL_ARROW_PATH = 'M3 3 L18.55 8.66 L11.27 11.27 L8.66 18.55 Z';
/** Where the tip sits in the 22 × 22 box: the hotspot. */
export const VERCEL_ARROW_TIP = { x: 3, y: 3 } as const;

export function VercelArrow({ className, pressed }: { className?: string; pressed?: boolean }) {
  return (
    <svg className={`ab-vercel-arrow${className ? ` ${className}` : ''}`} data-pressed={pressed || undefined} width="22" height="22" viewBox="0 0 22 22" overflow="visible" aria-hidden>
      <path className="ab-vercel-arrow-edge" d={VERCEL_ARROW_PATH} />
      <path className="ab-vercel-arrow-fill" d={VERCEL_ARROW_PATH} />
    </svg>
  );
}
