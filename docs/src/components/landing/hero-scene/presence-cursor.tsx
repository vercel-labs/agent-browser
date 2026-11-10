import type { CSSProperties } from 'react';
import { BrandMark } from '../compat';
import { VercelArrow } from '../shared/vercel-cursor';
import type { Peer } from './hero-timeline';
import './cursor-command-label.css';

type CursorBounds = { left: number; top: number; right: number; bottom: number };

/** Every participant's label is a 22px ink circle around its 12px mark (5px padding), no words,
 * as on the use-case cards. It always hangs below and right of the arrow; it
 * never changes sides (a carried file rides up and left of the hand instead). */
const LABEL = 22;
const OFFSET = { x: 12, y: 16 };
/** The label's box from the hotspot, in cursor units (multiply by the cursor's scale). */
export const LABEL_BOX = { x: OFFSET.x, y: OFFSET.y, size: LABEL } as const;
const BOUNDS_INSET = 10;

/** Optical sizes: Lil Pix is a solid, heavy mark at 12; the brand marks are
 * lighter line work, so they sit 2px larger to match its weight. */
const MARK_SIZE: Record<'agent' | 'claude' | 'openai' | 'gemini', number> = { agent: 12, claude: 14, openai: 14, gemini: 14 };

/** Ease into each boundary without changing which side carries the label. */
function clampLabel(value: number, min: number, max: number) {
  if (max <= min) return min;
  const clamped = Math.max(min, Math.min(max, value));
  const knee = Math.min(18, (max - min) / 2);
  if (clamped < min + knee) {
    const t = (clamped - min) / knee;
    return min + knee * t * t * (2 - t);
  }
  if (clamped > max - knee) {
    const t = (max - clamped) / knee;
    return max - knee * t * t * (2 - t);
  }
  return clamped;
}

/**
 * A participant's pointer and its label. The pointer is the Vercel arrow, or
 * the kit's I-beam while filling a field, crossfading between them.
 *
 * Constant on screen: the cursor ignores the camera. Its CSS size is
 * `--ab-cursor / --ab-zoom`, and `--ab-zoom` eases with the camera (same
 * curve and duration), so the pointer and label keep one size through the
 * zoom between scopes. `scale` is that size at the camera's target, used
 * only to keep the label inside `bounds`.
 */
export function PresenceCursor({ who, x, y, alpha = 1, pressed = false, kind = 'arrow', scale = 1, bounds }: {
  who: Peer; x: number; y: number; alpha?: number; pressed?: boolean;
  kind?: 'arrow' | 'ibeam'; scale?: number; bounds?: CursorBounds;
}) {
  const mark = who in MARK_SIZE ? who as keyof typeof MARK_SIZE : undefined;
  const size = LABEL * scale;
  // Clamp in world pixels, then hand CSS the offset in cursor units
  const labelX = (bounds ? clampLabel(x + OFFSET.x * scale, bounds.left + BOUNDS_INSET, bounds.right - BOUNDS_INSET - size) - x : OFFSET.x * scale) / scale;
  const labelY = (bounds ? clampLabel(y + OFFSET.y * scale, bounds.top + BOUNDS_INSET, bounds.bottom - BOUNDS_INSET - size) - y : OFFSET.y * scale) / scale;
  return (
    <span className="ab-peer" data-peer={who} data-kind={kind} style={{ transform: `translate(${x.toFixed(2)}px, ${y.toFixed(2)}px)`, opacity: alpha } as CSSProperties}>
      <span className="ab-peer-body">
        <span className="ab-peer-pointer" data-pressed={pressed || undefined}>
          <VercelArrow className="ab-peer-arrow" />
          {/* The kit's I-beam (hotspot 5, 8), with its thin light edge */}
          <svg className="ab-peer-ibeam" width="10" height="16" viewBox="0 0 10 16" fill="none" overflow="visible" aria-hidden>
            <path d="M3 1H7M3 15H7M5 1V15" stroke="var(--ds-background-100, #fff)" strokeWidth="2.6" strokeLinecap="round" />
            <path d="M3 1H7M3 15H7M5 1V15" stroke="var(--ds-gray-1000, #171717)" strokeWidth="1" strokeLinecap="round" />
          </svg>
        </span>
        <span className="ab-peer-tile" style={{ '--lx': `${labelX.toFixed(2)}px`, '--ly': `${labelY.toFixed(2)}px` } as CSSProperties}>
          {mark && <BrandMark name={mark} size={MARK_SIZE[mark]} />}
        </span>
      </span>
    </span>
  );
}
