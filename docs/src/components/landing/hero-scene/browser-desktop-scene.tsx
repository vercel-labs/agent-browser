'use client';

import '../../stageshow/host-layers.css';
import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Bar, Stage, type Layout, type Rect, useReducedMotion } from '../../stageshow/vendor/kit';
import { HeroCamera } from './hero-camera';
import { layoutRectOf } from '../../stageshow/vendor/kit/measure';
import { HERO_INITIAL, HERO_STILL, useHeroPlayback } from './hero-playback';
import { BOARD_PAD, heroFrame, peerPosition, type Targets } from './hero-timeline';
import { LABEL_BOX, PresenceCursor } from './presence-cursor';
import { BoardShot, BrowserWindow } from './hero-apps';
import { DesktopApps } from './desktop-apps';
import { desktopFrame, desktopPeerPosition, desktopPeerAlpha } from './desktop-timeline';
import { Marquee } from '../use-case-scenes/marquee';
import { useHairline } from '../use-case-scenes/hairline';
import '../use-case-scenes/vercel.css';
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
/** Cursors render at true screen pixels: their CSS size is --ss-unscale (the
 * stage's measured fit, inverted) over the camera's zoom. These are only the
 * usual fits' inverses, used to keep labels inside the frame. */
const CURSOR_SIZE = { wide: 1.6, compact: 2.7 } as const;
const STAGE_SIZE = { width: 1080, height: 750, compact: { below: 560, width: 960, height: 1050 } };
/** The board's tiles are 10px round; the marquee sits BOARD_PAD outside, concentric. */
const SELECTION_RADIUS = 10 + BOARD_PAD;
/** The board copy: the board's tiles in a white frame, SHOT_PAD all round,
 * drawn at 280 wide and scaled. It lifts off the board's rect grown by the pad. */
const SHOT_W = 280;
const SHOT_PAD = 8;
/** Held at the attachment's size, so the paste only travels. It rides above
 * and right of the arrow, the command pill below and right: neither covers
 * the other, and the copy stays in browser scope's crop wherever the hand
 * works (to the left it left the window at Compose; below, the board's foot is cropped). */
/** The carried screenshot hangs just beneath the cursor's label: centered under
 * it, 6px (screen) below it, at 80 canvas px wide. */
const CARRY = { w: 80, gap: 6 };
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

/** Both scopes share one retained world, one script and one seekable playhead. */
export function BrowserDesktopScene({ desktop }: { desktop: boolean }) {
  const reduced = useReducedMotion();
  const playback = useHeroPlayback(reduced);
  const host = useRef<HTMLElement>(null);
  useHairline(host);
  // Shown once measured: the Stage fits its scale and useHairline reads the
  // cursor's on-screen scale in the frames after mount. Until then the stage
  // is invisible inside a box already at its final size (CSS aspect-ratio),
  // so the first visible frame is the zoomed, correctly scaled scene and the
  // page never shifts.
  const [ready, setReady] = useState(false);
  useLayoutEffect(() => {
    let second = 0;
    const first = requestAnimationFrame(() => { second = requestAnimationFrame(() => setReady(true)); });
    return () => { cancelAnimationFrame(first); cancelAnimationFrame(second); };
  }, []);

  return (
    <figure ref={host} className="ab-hero-demo" data-hero-scene data-ready={ready || undefined} data-scope={desktop ? 'desktop' : 'browser'}>
      <figcaption className="ab-scene-description">An agent logs in to a mood board, draws a box around a block of images and captures it, opens a mail tab carrying the screenshot, composes a message, pastes the screenshot in, sends it and closes its tabs. Desktop reveals three more agents: Gemini selects three loose files and drags them to the Trash, OpenAI opens Photos from the dock, deletes three photos and closes it, and Claude zooms out on a map and pans around.</figcaption>
      <div aria-hidden="true">
        <Stage {...STAGE_SIZE} fill initial={HERO_INITIAL} poster={HERO_STILL} script={playback.script} play="loop" theme="light" fidelity="standard" className="ab-scene look-vercel">
          {(s, _rect, layout) => (
            <HeroCamera focus={desktop ? null : FOCUS[layout]} size={designSize(layout)} max={CAMERA_MAX} duration={1100} animate={ready}>
              <HeroWorld time={reduced ? playback.time : s.time} layout={layout} desktop={desktop} playing={playback.playing} />
            </HeroCamera>
          )}
        </Stage>
      </div>
    </figure>
  );
}

function HeroWorld({ time, layout, desktop, playing }: { time: number; layout: Layout; desktop: boolean; playing: boolean }) {
  const world = useRef<HTMLDivElement>(null);
  const [targets, setTargets] = useState<Targets>({});
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
  const frame = heroFrame(time);
  const desktopState = desktopFrame(time);
  // Targets are re-measured when the scene changes, except from the tab closes
  // to the loop's end: a closing tab shrinks and the board's tab reopens by
  // width, either of which would move a × under the cursor mid-move. Keep the
  // positions taken with both tabs open (page targets don't move).
  const ending = frame.phase === 'closing' || frame.phase === 'closed';
  const measureKey = ending ? 'sent' : frame.phase;

  useLayoutEffect(() => {
    const root = world.current;
    const stage = root?.closest<HTMLElement>('.ss-stage');
    if (!root || !stage) return;
    const next: Targets = {};
    for (const el of root.querySelectorAll<HTMLElement>('[data-target]')) next[el.dataset.target!] = layoutRectOf(stage, el);
    // Measured DOM geometry must be available before the first paint.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setTargets(next);
    // Everything stays mounted. Measure untransformed layout coordinates so
    // camera moves and direct seeks cannot change the peers' destinations.
  }, [layout, measureKey, desktopState.phase, playing]);

  useLayoutEffect(() => {
    const tabs = world.current?.querySelectorAll('.ab-browser .ss-window__tab');
    tabs?.forEach((tab, index) => tab.toggleAttribute('data-active', (index === 0 && frame.pinTab && frame.active === 'pin') || (index === 1 && frame.active === 'mail')));
  }, [frame.pinTab, frame.active]);

  useEffect(() => { world.current?.closest('.ss-stage')?.setAttribute('data-mark', frame.phase); }, [frame.phase]);

  const agent = peerPosition('agent', time, targets);
  const board = targets['board'];
  // The marquee grows from the board's corner with the drag. Whole pixels, so
  // the 1px dashes sit on the pixel grid at full strength.
  const start = board ? { x: Math.round(board.x - BOARD_PAD), y: Math.round(board.y - BOARD_PAD) } : null;
  const capture = board && start && frame.marquee ? {
    ...start,
    w: Math.round(frame.selecting ? Math.max(0, agent.x - start.x) : board.width + BOARD_PAD * 2),
    h: Math.round(frame.selecting ? Math.max(0, agent.y - start.y) : board.height + BOARD_PAD * 2),
  } : undefined;
  const focus = FOCUS[layout];
  // The camera's target zoom (the kit's formula with pad 0). --ab-zoom eases to
  // it with the camera, so cursors and the browser's hairline can ignore it.
  const size = layout === 'wide' ? { w: STAGE_SIZE.width, h: STAGE_SIZE.height } : { w: STAGE_SIZE.compact.width, h: STAGE_SIZE.compact.height };
  const zoom = desktop ? 1 : Math.max(1, Math.min(CAMERA_MAX, size.w / focus.width, size.h / focus.height));
  const cursorSize = unscale ?? CURSOR_SIZE[layout];
  const cursorScale = cursorSize / zoom;
  // The copy: off the board at full size, to the hand, then into the message.
  const attach = targets['attach'];
  let carry: { x: number; y: number; scale: number } | null = null;
  if (board && frame.carryVisible) {
    // The label is drawn at true screen pixels (cursorScale canvas px per unit),
    // so place the copy from the label's own box
    const held = { x: agent.x + (LABEL_BOX.x + LABEL_BOX.size / 2) * cursorScale - CARRY.w / 2, y: agent.y + (LABEL_BOX.y + LABEL_BOX.size + CARRY.gap) * cursorScale, scale: CARRY.w / SHOT_W };
    if (frame.copyFlight < 1) {
      const from = { x: board.x - SHOT_PAD, y: board.y - SHOT_PAD, scale: (board.width + SHOT_PAD * 2) / SHOT_W };
      carry = { x: lerp(from.x, held.x, frame.copyFlight), y: lerp(from.y, held.y, frame.copyFlight), scale: lerp(from.scale, held.scale, frame.copyFlight) };
    } else if (frame.pasteFlight > 0 && attach) {
      carry = { x: lerp(held.x, attach.x, frame.pasteFlight), y: lerp(held.y, attach.y, frame.pasteFlight), scale: held.scale };
    } else carry = held;
  }
  const desktopBounds = { left: 0, top: 0, right: layout === 'wide' ? 1080 : 960, bottom: layout === 'wide' ? 750 : 1050 };
  const cursorBounds = { left: focus.x, top: focus.y, right: focus.x + focus.width, bottom: focus.y + focus.height };

  return (
    <div ref={world} className="ab-world" style={{ '--ab-zoom': zoom } as CSSProperties} data-desktop={desktop} data-playing={playing} data-time={Math.round(time)} data-phase={frame.phase}>
      <div className="ab-wallpaper" />
      <div className="ab-menu"><span className="ab-menu-mark" /><Bar w={40} h={7} tone={3} /><Bar w={40} h={7} tone={2} /><Bar w={40} h={7} tone={2} /><span className="ab-menu-right"><Bar w={40} h={7} tone={2} /></span></div>
      <BrowserWindow frame={frame} />
      <DesktopApps time={time} targets={targets} />
      {board && frame.flash > 0 && <div className="ab-board-flash" style={{ left: board.x, top: board.y, width: board.width, height: board.height, opacity: frame.flash }} />}
      {capture && <div className="ab-capture" style={{ opacity: frame.marqueeAlpha }}>
        <Marquee box={capture} radius={SELECTION_RADIUS} className="ab-capture-selection" data-recording={frame.captured || undefined} />
      </div>}
      {carry && <div className="ab-carry" style={{ transform: `translate3d(${carry.x}px, ${carry.y}px, 0) scale(${carry.scale})`, boxShadow: frame.copyFlight < 1 ? 'none' : undefined }}><BoardShot /></div>}
      <PresenceCursor who="agent" {...agent} kind={frame.agentKind} scale={cursorScale} bounds={desktop ? desktopBounds : cursorBounds} alpha={board ? 1 : 0} pressed={frame.agentPress} />
      <PresenceCursor who="claude" scale={cursorScale} bounds={desktopPeerAlpha('claude', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('claude', time, targets)} pressed={desktopState.claudePress} alpha={desktop && targets['map-zoom-out'] ? desktopPeerAlpha('claude', time) : 0} />
      <PresenceCursor who="openai" scale={cursorScale} bounds={desktopPeerAlpha('openai', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('openai', time, targets)} pressed={desktopState.openaiPress} alpha={desktop && targets['dock-photos'] ? desktopPeerAlpha('openai', time) : 0} />
      <PresenceCursor who="gemini" scale={cursorScale} bounds={desktopPeerAlpha('gemini', time) < 1 ? undefined : desktopBounds} {...desktopPeerPosition('gemini', time, targets)} pressed={desktopState.geminiPress} alpha={desktop && targets['dock-trash'] ? desktopPeerAlpha('gemini', time) : 0} />
    </div>
  );
}
