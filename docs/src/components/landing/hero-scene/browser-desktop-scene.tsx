'use client';

import '../../stageshow/host-layers.css';
import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Stage, type Layout, type Rect, useReducedMotion } from '../../stageshow/vendor/kit';
import { LogoApple } from './geist-glyphs';
import { wallpaperAt } from './wallpapers';
import { HeroCamera } from './hero-camera';
import { layoutRectOf } from '../../stageshow/vendor/kit/measure';
import { HERO_INITIAL, HERO_STILL, useHeroPlayback } from './hero-playback';
import { PREVIEW_URL, PRODUCTION_URL, PR_URL, SHOT_GAP, T, heroFrame, peerPosition, type Targets } from './hero-timeline';
import { HeroTabPicker, type HeroTab } from './hero-tab-picker';
import { LABEL_BOX, PresenceCursor } from './presence-cursor';
import { ATTACH_SCALE, BrowserWindow, HeadlineShot, shotSize } from './hero-apps';
import { DesktopApps } from './desktop-apps';
import { desktopFrame, desktopPeerPosition, desktopPeerAlpha } from './desktop-timeline';
import { Marquee } from '../use-case-scenes/marquee';
import { useHairline } from '../use-case-scenes/hairline';
import '../shared/vercel.css';
import './browser-desktop-scene.css';

/** Browser scope's crop, matched to each stage's aspect: a gutter at the
 * window's left and top, the window running off the right and bottom edges.
 * Desktop apps stay outside it. */
const FOCUS: Record<Layout, Rect> = {
  wide: { x: 0, y: 52, width: 500, height: 500 * 750 / 1080 },
  compact: { x: 0, y: 52, width: 312, height: 312 * 1050 / 960 },
};

/** Landscape (1.44:1), so the hero sits beside the copy without towering over
 * it. The canvas is large (1080 × 750) so the desktop has room to breathe
 * around a 560 × 360 browser; browser scope's crop is the same size in canvas
 * pixels as before, so the zoomed-in browser renders at the same size. */
/** The stage's design size for a layout: the camera's frame. */
const designSize = (layout: Layout) => layout === 'wide'
  ? { w: STAGE_SIZE.width, h: STAGE_SIZE.height }
  : { w: STAGE_SIZE.compact.width, h: STAGE_SIZE.compact.height };

/** Browser scope's zoom is the canvas over the crop (2.16 wide, 3.08 compact); the cap only guards. */
const CAMERA_MAX = 3.2;
/** The camera's move between scopes (HeroCamera's transition, its travel curve). */
const CAMERA_MS = 1100;
/** Entering Desktop, the wallpaper holds until the zoom-out is this far along:
 * 900 of 1100ms, about 98% of the way on the travel curve, so it is still
 * while the frame moves and the series only takes over as the camera settles. */
const WALL_GATE = 900;
/** Cursors render at true screen pixels: their CSS size is --ss-unscale (the
 * stage's measured fit, inverted) over the camera's zoom. These are only the
 * usual fits' inverses, used to keep labels inside the frame. */
const CURSOR_SIZE = { wide: 1.6, compact: 2.7 } as const;
const STAGE_SIZE = { width: 1080, height: 750, compact: { below: 560, width: 960, height: 1050 } };
/** The marquee's corners: the screenshot card's radius for a region with
 * square corners (its PAD, the same as SHOT_GAP). */
const SELECTION_RADIUS = 8;
/** The carried screenshots hang just beneath the cursor's label: centered under
 * it, 6px (screen) below it, 96 canvas px wide. Two fan like the screenshot
 * card's deck: the first behind, turned a little; the second in front, a touch
 * smaller, nudged right and turned further. Offsets are canvas px. */
const CARRY = { w: 96, gap: 6 };
const DECK = [{ dx: 0, dy: 0, w: CARRY.w, rotate: -3 }, { dx: 6, dy: 8, w: CARRY.w - 4, rotate: 4 }] as const;
/** The copies' corners: square on the page, 3px carried, the slot's 6px landed (visible px). */
const CARRY_RADIUS = { held: 3, landed: 6 };
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

/** Both scopes share one retained world, one script and one seekable playhead. */
export function BrowserDesktopScene({ desktop }: { desktop: boolean }) {
  const reduced = useReducedMotion();
  const playback = useHeroPlayback(reduced);
  const host = useRef<HTMLElement>(null);
  useHairline(host);
  // Measured: the Stage has fit its scale and useHairline has read the
  // cursor's on-screen scale, in the frames after mount. Until then CSS
  // supplies the same fit (browser-desktop-scene.css), so the server-rendered
  // first paint is already the opening frame, in a box already at its final
  // size (CSS aspect-ratio): no fade, and the page never shifts. Also turns on
  // the camera's transitions.
  const [ready, setReady] = useState(false);
  useLayoutEffect(() => {
    let second = 0;
    const first = requestAnimationFrame(() => { second = requestAnimationFrame(() => setReady(true)); });
    return () => { cancelAnimationFrame(first); cancelAnimationFrame(second); };
  }, []);
  // Reduced motion: the still's browser tabs can be clicked through. The
  // choice only changes which open tab is active; null means the still's own.
  const still = heroFrame(playback.time);
  const [tab, setTab] = useState<HeroTab | null>(null);
  // Every change of the preference starts the still from its own tab again
  // ("adjust state when a prop changes", guarded so it settles at once)
  const [tabFor, setTabFor] = useState(reduced);
  if (tabFor !== reduced) { setTabFor(reduced); setTab(null); }
  const selectedTab = reduced ? (tabFor === reduced ? tab : null) ?? still.active : null;

  return (
    <figure ref={host} className="ab-hero-demo" data-hero-scene data-ready={ready || undefined} data-scope={desktop ? 'desktop' : 'browser'}>
      <figcaption className="ab-scene-description">An agent notices a bug and proves the fix: on the production site, open in the browser, the headline reads Build better prpducts, fadter, and the agent screenshots the hero. It opens the Preview deployment of the fix in a new tab, where the headline reads Build better products, faster, and screenshots that too. Carrying both, it opens the pull request, Fix typos in hero heading #128, and pastes them into the description side by side as Before and After, then closes the pull request and the preview, back on production. Desktop reveals three more agents: Gemini selects three loose files and drags them to the Trash, OpenAI opens Photos from the dock, deletes three photos and closes it, and Claude zooms out on a map and pans around.</figcaption>
      <div aria-hidden="true">
        <Stage {...STAGE_SIZE} fill initial={HERO_INITIAL} poster={HERO_STILL} script={playback.script} play="loop" theme="light" fidelity="standard" className="ab-scene look-vercel">
          {(s, _rect, layout) => (
            <HeroCamera focus={desktop ? null : FOCUS[layout]} size={designSize(layout)} max={CAMERA_MAX} duration={CAMERA_MS} animate={ready}>
              <HeroWorld time={reduced ? playback.time : s.time} loop={reduced ? 0 : s.loop} layout={layout} desktop={desktop} playing={playback.playing} selectedTab={selectedTab} />
            </HeroCamera>
          )}
        </Stage>
      </div>
      {reduced && selectedTab && (
        <HeroTabPicker
          host={host}
          open={{ production: true, preview: still.previewTab, github: still.githubTab }}
          selected={selectedTab}
          onSelect={setTab}
          measureKey={`${desktop ? 'desktop' : 'browser'} ${ready ? 'ready' : 'placing'}`}
        />
      )}
    </figure>
  );
}

/** The targets the opening frame uses, as measured in the wide layout (world
 * pixels), so the server-rendered first paint already has the hand resting on
 * the page. Measurement replaces them before the client's first paint, and a
 * phone column, which the server cannot render, waits for it anyway. Keep in
 * step with .ab-home and SHOT_REGION.wide if either moves. */
const SERVER_TARGETS: Targets = {
  home: { x: 274, y: 348, width: 1, height: 1 },
  'shot-region': { x: 36, y: 188, width: 436, height: 184 },
};
/** The dock's icons wait this long after mount, then for the browser to be idle. */
const ASSETS_AFTER = 2_500;

function HeroWorld({ time, loop = 0, layout, desktop, playing, selectedTab = null }: { time: number; loop?: number; layout: Layout; desktop: boolean; playing: boolean; selectedTab?: HeroTab | null }) {
  const world = useRef<HTMLDivElement>(null);
  const [targets, setTargets] = useState<Targets>(SERVER_TARGETS);
  // The dock's icons are desktop-only, off camera in browser scope, so they are
  // not fetched with the page: they load once Desktop is chosen, or when the
  // browser is idle a while after mount. (The wallpapers are CSS gradients.)
  const [idle, setIdle] = useState(false);
  const wantAssets = desktop || idle;
  useEffect(() => {
    if (idle) return;
    const w = window as Window & { requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number; cancelIdleCallback?: (id: number) => void };
    let handle = 0;
    const timer = window.setTimeout(() => {
      if (w.requestIdleCallback) handle = w.requestIdleCallback(() => setIdle(true), { timeout: 3_000 });
      else setIdle(true);
    }, ASSETS_AFTER);
    return () => { window.clearTimeout(timer); if (handle) w.cancelIdleCallback?.(handle); };
  }, [idle]);
  // The stage's measured fit, inverted (useHairline's --ss-unscale): cursors are
  // drawn at true screen pixels, so anything placed beside them uses this too
  const [unscale, setUnscale] = useState<number | null>(null);
  useLayoutEffect(() => {
    const root = world.current;
    if (!root) return;
    const read = () => {
      const value = parseFloat(getComputedStyle(root).getPropertyValue('--ss-unscale'));
      setUnscale(Number.isFinite(value) && value > 0 ? value : null);
    };
    read();
    const stage = root.closest('.ss-stage');
    const observer = new ResizeObserver(() => requestAnimationFrame(read));
    if (stage?.parentElement) observer.observe(stage.parentElement);
    return () => observer.disconnect();
  }, [layout]);
  // A tab chosen in the reduced-motion still (only an open one) becomes the
  // active tab, its page and its address; everything else stays the still's
  const timed = heroFrame(time);
  const chosen = selectedTab && (selectedTab === 'production' || (selectedTab === 'preview' ? timed.previewTab : timed.githubTab)) ? selectedTab : null;
  const frame = chosen && chosen !== timed.active
    ? { ...timed, active: chosen, url: chosen === 'github' ? PR_URL : chosen === 'preview' ? PREVIEW_URL : PRODUCTION_URL }
    : timed;
  const desktopState = desktopFrame(time, loop);
  // The wallpaper only changes where it can be seen. In browser scope it holds
  // the last one shown. Choosing Desktop keeps holding it through the camera's
  // zoom-out, and only once that is almost done (WALL_GATE) does the series take
  // over, re-based so it carries on from the held wallpaper: nothing that
  // happened off camera or mid-zoom catches up, and the next change is a Photos
  // click the viewer sees. A fade already under way when the gate opens (a
  // click made mid-zoom) is pinned to the held wallpaper until the next click.
  // React's "adjust state when a prop changes": scope changes and the gate's
  // opening set the new state during render (guarded, so it settles at once).
  const absolute = desktopState.wall;
  /** The wallpaper the series is showing or heading to. */
  const heading = absolute.mix > 0 ? absolute.next : absolute.base;
  const [wallState, setWallState] = useState({ desktop, open: desktop, offset: 0, frozen: 0, pin: null as number | null });
  const [gateDue, setGateDue] = useState(desktop);
  let wallTrack = wallState;
  if (desktop !== wallState.desktop) {
    // Leaving Desktop freezes what was on screen; entering it waits for the gate
    const shown = wallState.open && !(wallState.pin !== null && absolute.base === wallState.pin)
      ? (absolute.mix > .5 ? absolute.next : absolute.base) - wallState.offset
      : wallState.frozen;
    wallTrack = { desktop, open: false, offset: wallState.offset, frozen: shown, pin: null };
    setWallState(wallTrack);
    setGateDue(false);
  } else if (desktop && !wallState.open && gateDue) {
    wallTrack = { ...wallState, open: true, offset: heading - wallState.frozen, pin: absolute.mix > 0 ? absolute.base : null };
    setWallState(wallTrack);
  }
  useEffect(() => {
    if (!desktop) return;
    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const timer = window.setTimeout(() => setGateDue(true), reduce ? 0 : WALL_GATE);
    return () => window.clearTimeout(timer);
  }, [desktop]);
  const pinned = wallTrack.pin !== null && absolute.base === wallTrack.pin;
  const wall = desktop && wallTrack.open && !pinned
    ? { base: absolute.base - wallTrack.offset, next: absolute.next - wallTrack.offset, mix: absolute.mix }
    : { base: wallTrack.frozen, next: wallTrack.frozen, mix: 0 };
  // Photos' thumbnails follow the same re-basing; until the gate opens they use
  // the offset it will set, so they don't reshuffle when it does
  const photoOffset = !desktop ? 0 : wallTrack.open ? wallTrack.offset : heading - wallTrack.frozen;
  // Targets are re-measured when the scene changes, except from the tab closes
  // to the loop's end: a closing tab shrinks and production's tab widens again,
  // either of which would move a × under the cursor mid-move. Keep the
  // positions taken with all three tabs open (page targets don't move). The
  // desktop's own beats still re-measure during the closes, so the tab strip's
  // targets are carried over from the last measure rather than re-read then.
  const ending = frame.phase === 'closing';
  const measureKey = ending ? 'pasted' : frame.phase;

  useLayoutEffect(() => {
    const root = world.current;
    const stage = root?.closest<HTMLElement>('.ss-stage');
    if (!root || !stage) return;
    const next: Targets = {};
    for (const el of root.querySelectorAll<HTMLElement>('[data-target]')) next[el.dataset.target!] = layoutRectOf(stage, el);
    const strip = (key: string) => key.startsWith('close-') || key.startsWith('plus-') || key === 'tab-plus';
    // Measured DOM geometry must be available before the first paint.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setTargets(prev => ending ? Object.fromEntries(Object.entries(next).map(([key, rect]) => [key, strip(key) ? prev[key] ?? rect : rect])) : next);
    // Everything stays mounted. Measure untransformed layout coordinates so
    // camera moves and direct seeks cannot change the peers' destinations.
  }, [layout, measureKey, ending, desktopState.phase, playing]);

  useLayoutEffect(() => {
    const tabs = world.current?.querySelectorAll('.ab-browser .ss-window__tab');
    const open = [true, frame.previewTab, frame.githubTab];
    const order = ['production', 'preview', 'github'] as const;
    tabs?.forEach((tab, index) => tab.toggleAttribute('data-active', index < 3 && open[index] && frame.active === order[index]));
  }, [frame.previewTab, frame.githubTab, frame.active]);

  useEffect(() => { world.current?.closest('.ss-stage')?.setAttribute('data-mark', frame.phase); }, [frame.phase]);

  const agent = peerPosition('agent', time, targets);
  const region = targets['shot-region'];
  const { width: shotW, height: shotH } = shotSize(layout);
  // The marquee grows from the drag's fixed corner (the region's top left, or
  // its lower right when the drag starts there) to the hand. Whole pixels, so
  // the 1px dashes sit on the pixel grid at full strength.
  const full = region ? { x: Math.round(region.x - SHOT_GAP), y: Math.round(region.y - SHOT_GAP), w: Math.round(region.width + SHOT_GAP * 2), h: Math.round(region.height + SHOT_GAP * 2) } : null;
  let capture: { x: number; y: number; w: number; h: number } | undefined;
  if (full && frame.marquee) {
    if (!frame.marqueeSelecting) capture = full;
    else if (frame.marqueeFromEnd) {
      const end = { x: full.x + full.w, y: full.y + full.h };
      const x = Math.round(Math.min(end.x, agent.x)), y = Math.round(Math.min(end.y, agent.y));
      capture = { x, y, w: end.x - x, h: end.y - y };
    } else capture = { x: full.x, y: full.y, w: Math.round(Math.max(0, agent.x - full.x)), h: Math.round(Math.max(0, agent.y - full.y)) };
  }
  const focus = FOCUS[layout];
  // The camera's target zoom (the kit's formula with pad 0). --ab-zoom eases to
  // it with the camera, so cursors and the browser's hairline can ignore it.
  const size = layout === 'wide' ? { w: STAGE_SIZE.width, h: STAGE_SIZE.height } : { w: STAGE_SIZE.compact.width, h: STAGE_SIZE.compact.height };
  const zoom = desktop ? 1 : Math.max(1, Math.min(CAMERA_MAX, size.w / focus.width, size.h / focus.height));
  const cursorSize = unscale ?? CURSOR_SIZE[layout];
  const cursorScale = cursorSize / zoom;
  // The copies, each placed by its center, scale and turn, all interpolated
  // together: lifted off the page at full size to the hand (the deck), then,
  // on the paste, one eased flight from the hand to its slot in the
  // description, landing at exactly the slot's size, so the slot's own image
  // takes over without a change.
  type Pose = { cx: number; cy: number; scale: number; rotate: number };
  const mix = (a: Pose, b: Pose, t: number): Pose => ({ cx: lerp(a.cx, b.cx, t), cy: lerp(a.cy, b.cy, t), scale: lerp(a.scale, b.scale, t), rotate: lerp(a.rotate, b.rotate, t) });
  // The label is drawn at true screen pixels (cursorScale canvas px per unit),
  // so the deck hangs from the label's own box
  const hangFrom = (hand: { x: number; y: number }) => ({ x: hand.x + (LABEL_BOX.x + LABEL_BOX.size / 2) * cursorScale, y: hand.y + (LABEL_BOX.y + LABEL_BOX.size + CARRY.gap) * cursorScale });
  const hang = hangFrom(agent);
  const carries = region ? frame.copies.map((copy, i) => {
    if (!copy.visible) return null;
    const deck = DECK[i];
    const heldScale = deck.w / shotW;
    const page: Pose = { cx: region.x + region.width / 2, cy: region.y + region.height / 2, scale: 1, rotate: 0 };
    // Once let go, a copy flies from where the hand was at that moment, so the
    // hand moving on never bends its path
    const from = copy.paste > 0 ? hangFrom(peerPosition('agent', T.paste[i][0], targets)) : hang;
    const held: Pose = { cx: from.x + deck.dx, cy: from.y + deck.dy + shotH * heldScale / 2, scale: heldScale, rotate: deck.rotate };
    const slot = targets[`slot-${i}`];
    const landed: Pose = slot ? { cx: slot.x + slot.width / 2, cy: slot.y + slot.height / 2, scale: ATTACH_SCALE[layout], rotate: 0 } : held;
    const pose = mix(mix(page, held, copy.lift), landed, copy.paste);
    // Corners and ring are drawn before the scale, so divide by it to keep their
    // visible size; the lift shadow rises off the page and settles on landing
    const radius = lerp(lerp(0, CARRY_RADIUS.held, copy.lift), CARRY_RADIUS.landed, copy.paste) / pose.scale;
    const lift = Math.min(copy.lift, 1 - copy.paste);
    const k = 1 / pose.scale;
    const boxShadow = `0 0 0 calc(var(--ab-hair) / var(--ab-zoom, 1) * ${k.toFixed(4)}) color-mix(in srgb, var(--ss-line) ${(copy.lift * 100).toFixed(1)}%, transparent), 0 ${(4 * lift * k).toFixed(2)}px ${(14 * lift * k).toFixed(2)}px ${(-4 * k).toFixed(2)}px rgba(0, 0, 0, ${(.16 * lift).toFixed(3)})`;
    return { i, pose, radius, boxShadow };
  }) : [];
  const desktopBounds = { left: 0, top: 0, right: layout === 'wide' ? 1080 : 960, bottom: layout === 'wide' ? 750 : 1050 };
  const cursorBounds = { left: focus.x, top: focus.y, right: focus.x + focus.width, bottom: focus.y + focus.height };

  return (
    <div ref={world} className="ab-world" style={{ '--ab-zoom': zoom } as CSSProperties} data-desktop={desktop} data-playing={playing} data-time={Math.round(time)} data-phase={frame.phase} data-assets={wantAssets || undefined}>
      {/* The wallpaper: a base and the photo OpenAI clicked fading in over it;
          the grain and triangle sit on top (CSS) */}
      <div className="ab-wallpaper">
        <span className="ab-wallpaper-layer" style={{ backgroundImage: wallpaperAt(wall.base) }} />
        <span className="ab-wallpaper-layer" style={{ backgroundImage: wallpaperAt(wall.next), opacity: wall.mix }} />
      </div>
      {/* macOS menu bar: the Apple logo, then File, Edit, View; the right side left empty */}
      <div className="ab-menu"><LogoApple size={18} /><span>File</span><span>Edit</span><span>View</span></div>
      <BrowserWindow frame={frame} layout={layout} />
      <DesktopApps time={time} loop={loop} wallOffset={photoOffset} targets={targets} />
      {/* The flash fills the marquee's own box and corners, as on the screenshot card */}
      {region && frame.flash > 0 && <div className="ab-shot-flash" style={{ left: region.x - SHOT_GAP, top: region.y - SHOT_GAP, width: region.width + SHOT_GAP * 2, height: region.height + SHOT_GAP * 2, borderRadius: SELECTION_RADIUS, opacity: frame.flash }} />}
      {capture && <div className="ab-capture" style={{ opacity: frame.marqueeAlpha }}>
        {/* Placed by transform, not left/top: a box growing up and left from its
            lower right would otherwise move in layout every frame (a layout shift) */}
        <div className="ab-capture-box" style={{ transform: `translate(${capture.x}px, ${capture.y}px)` }}>
          <Marquee box={{ x: 0, y: 0, w: capture.w, h: capture.h }} radius={SELECTION_RADIUS} fromEnd={frame.marqueeFromEnd} className="ab-capture-selection" data-recording={frame.captured || undefined} />
        </div>
      </div>}
      {carries.map(carry => carry && (
        <div key={carry.i} className="ab-carry" data-copy={carry.i} style={{ width: shotW, height: shotH, transform: `translate3d(${(carry.pose.cx - shotW / 2).toFixed(2)}px, ${(carry.pose.cy - shotH / 2).toFixed(2)}px, 0) rotate(${carry.pose.rotate.toFixed(3)}deg) scale(${carry.pose.scale.toFixed(5)})`, borderRadius: carry.radius, boxShadow: carry.boxShadow }}>
          <HeadlineShot layout={layout} fixed={carry.i === 1} />
        </div>
      ))}
      <PresenceCursor who="agent" {...agent} kind={frame.agentKind} scale={cursorScale} bounds={desktop ? desktopBounds : cursorBounds} alpha={region ? 1 : 0} pressed={frame.agentPress} />
      <PresenceCursor who="claude" scale={cursorScale} bounds={desktopPeerAlpha('claude', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('claude', time, targets)} pressed={desktopState.claudePress} alpha={desktop && targets['map-zoom-out'] ? desktopPeerAlpha('claude', time) : 0} />
      <PresenceCursor who="openai" scale={cursorScale} bounds={desktopPeerAlpha('openai', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('openai', time, targets)} pressed={desktopState.openaiPress} alpha={desktop && targets['dock-photos'] ? desktopPeerAlpha('openai', time) : 0} />
      <PresenceCursor who="gemini" scale={cursorScale} bounds={desktopPeerAlpha('gemini', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('gemini', time, targets)} pressed={desktopState.geminiPress} alpha={desktop && targets['dock-trash'] ? desktopPeerAlpha('gemini', time) : 0} />
    </div>
  );
}
