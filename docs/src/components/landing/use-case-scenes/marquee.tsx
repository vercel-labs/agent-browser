/** A rectangle in stage coordinates. */
export type MarqueeBox = { x: number; y: number; w: number; h: number };

/**
 * A dashed, rounded marquee: two open paths that both start at the drag's
 * fixed corner and run around either side to meet at the far corner. The
 * dash pattern runs evenly through the corners, and both edges leaving the
 * fixed corner keep their dash origin there, so they hold still while the box
 * grows and only the far edges march, as a selection marquee does. (One
 * closed path made the last edge's dash phase depend on the whole perimeter,
 * so it stuttered every frame as the box grew.) Colors and
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
  const cw = (x: number, y: number) => `A ${a} ${a} 0 0 1 ${x} ${y}`;
  const ccw = (x: number, y: number) => `A ${a} ${a} 0 0 0 ${x} ${y}`;
  // Each pair starts on the fixed corner's arc and splits: one way clockwise
  // through two corners, the other counterclockwise through one; they meet at
  // the far corner, where the moving edges are.
  const paths = fromEnd
    ? [
      `M ${w - i} ${h - r} ${cw(w - r, h - i)} H ${r} ${cw(i, h - r)} V ${r} ${cw(r, i)}`,
      `M ${w - i} ${h - r} V ${r} ${ccw(w - r, i)} H ${r}`,
    ]
    : [
      `M ${i} ${r} ${cw(r, i)} H ${w - r} ${cw(w - i, r)} V ${h - r} ${cw(w - r, h - i)}`,
      `M ${i} ${r} V ${h - r} ${ccw(r, h - i)} H ${w - r}`,
    ];
  return <svg {...data} className={className} width={w} height={h} style={{ position: 'absolute', left: box.x, top: box.y, overflow: 'visible' }} aria-hidden="true">
    {paths.map((d) => <path key={d.slice(0, 12)} d={d} />)}
  </svg>;
}
