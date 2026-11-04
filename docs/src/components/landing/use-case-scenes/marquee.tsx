/** A rectangle in stage coordinates. */
export type MarqueeBox = { x: number; y: number; w: number; h: number };

/**
 * A dashed, rounded marquee: one continuous path, so the dash pattern runs
 * evenly through the corners like any dashed stroke. It starts at the drag's
 * fixed corner, so the edges leaving that corner hold still while the box
 * grows and the far edges march, as a selection marquee does. Colors and
 * fades come from the host's CSS (`color`, `opacity` on `className`).
 * `radius` should be concentric with what it surrounds: its radius plus the gap.
 */
export function Marquee({ box, radius, fromEnd = false, className, ...data }: {
  box: MarqueeBox;
  radius: number;
  /** The drag started at the box's lower right, so the path starts there. */
  fromEnd?: boolean;
  className: string;
} & Record<`data-${string}`, string | boolean | undefined>) {
  const w = Math.max(0, box.w), h = Math.max(0, box.h);
  const r = Math.min(radius, w / 2, h / 2);
  const i = 0.5;
  const a = Math.max(0, r - i);
  const arc = (x: number, y: number) => `A ${a} ${a} 0 0 1 ${x} ${y}`;
  // Clockwise either way; only the starting corner changes.
  const fromStart = `M ${r} ${i} H ${w - r} ${arc(w - i, r)} V ${h - r} ${arc(w - r, h - i)} H ${r} ${arc(i, h - r)} V ${r} ${arc(r, i)} Z`;
  const fromFar = `M ${w - r} ${h - i} H ${r} ${arc(i, h - r)} V ${r} ${arc(r, i)} H ${w - r} ${arc(w - i, r)} V ${h - r} ${arc(w - r, h - i)} Z`;
  return <svg {...data} className={className} width={w} height={h} style={{ position: 'absolute', left: box.x, top: box.y, overflow: 'visible' }} aria-hidden="true">
    <path d={fromEnd ? fromFar : fromStart} />
  </svg>;
}
