import type { ReactNode } from 'react';
import type { Rect } from '../../stageshow/vendor/kit';
import '../../stageshow/vendor/kit/motion/Camera.css';

/**
 * The kit Camera's zoom, worked out from the stage's known design size
 * instead of a measured one. The kit Camera starts at size 0 and only zooms
 * once a layout effect has measured its frame, so server HTML (and every
 * frame until hydration) showed the whole desktop unzoomed, then jumped.
 * The hero's frame is always its design canvas, so the transform is known
 * up front: the first paint is already zoomed. Same markup and classes as
 * the kit Camera (and its CSS), same formula, pad 0, no feather.
 *
 * `animate` turns the transition on once the scene is showing, so only a
 * scope change moves the camera, never the first paint.
 */
export function HeroCamera({ focus, size, max, duration, animate, children }: {
  focus: Rect | null;
  /** The stage's design size for the current layout (the camera's frame). */
  size: { w: number; h: number };
  max: number;
  duration: number;
  animate: boolean;
  children: ReactNode;
}) {
  const zoom = heroCameraZoom(focus, size, max);
  let transform = 'none';
  if (focus && zoom > 1) {
    const tx = Math.min(0, Math.max(size.w - size.w * zoom, size.w / 2 - (focus.x + focus.width / 2) * zoom));
    const ty = Math.min(0, Math.max(size.h - size.h * zoom, size.h / 2 - (focus.y + focus.height / 2) * zoom));
    transform = `translate(${tx}px, ${ty}px) scale(${zoom})`;
  }
  const transition = animate && duration > 0 ? `transform ${duration}ms cubic-bezier(0.45, 0, 0.1, 1)` : 'none';
  return (
    <div className="ss-camera-view" data-zoomed={transform === 'none' ? undefined : true} data-feather={0} style={{ ['--ss-feather' as string]: '0px' }}>
      <div className="ss-camera" style={{ transform, transition }}>
        {children}
      </div>
    </div>
  );
}

/** The kit Camera's scale for a focus (pad 0): 1 when there is no focus. */
export function heroCameraZoom(focus: Rect | null, size: { w: number; h: number }, max: number) {
  if (!focus) return 1;
  return Math.max(1, Math.min(max, size.w / focus.width, size.h / focus.height));
}
