import type { Point } from '../../stageshow/vendor/kit';
import { bezier } from '../../stageshow/vendor/kit/ease';
import { smooth, type Targets } from './hero-timeline';

// A lived-in desktop: the browser leading, Maps beside it, a few loose files.
// Three agents work in parallel on one pulse (B = 850ms). The desktop runs a
// short cycle (CYCLE, 10 beats) twice per 17s loop, so the agents are busy at
// the same time instead of taking turns: all three are on screen together for
// most of each cycle's first half (about 1.3–4.4s), twice a loop.
// Inactivity decides presence: an agent enters (up from below) just before its
// first action and leaves (down) once it has nothing to do for a while. Claude
// always has work, so it stays; Gemini and OpenAI come and go every cycle.
//   Gemini   box-selects the loose files and drags them to the Trash, leaves
//   OpenAI   opens Photos from the dock (well before Gemini's drop), clicks a
//            photo (it becomes the wallpaper), clicks another (the wallpaper
//            changes again), deletes the first, closes Photos, leaves. The
//            wallpaper carries over, so it steps through a series across loops.
//   Claude   drags Maps away by its title bar, zooms out once, pans west and
//            back east, zooms in once, drags Maps home
// Every zoom is about the map's center. Each cycle ends where it began: new
// files settle in, Photos is closed, Maps is home, in close and centered.

type DesktopPeer = 'claude' | 'openai' | 'gemini';
/** A route stop: a target, a point inside it (fractions), and an offset in pixels. */
type Stop = readonly [time: number, target: string, x?: number, y?: number, dx?: number, dy?: number];

/** The pulse: every press lands on a beat or a half beat. */
export const BEAT = 850;
const b = (beats: number) => Math.round(beats * BEAT);
/** The desktop's cycle: half the 17s loop, so it plays twice and the seam holds. */
const CYCLE = 8_500;
const END = CYCLE;
/** Loop time folded into the cycle. */
const cyc = (time: number) => ((time % CYCLE) + CYCLE) % CYCLE;

/** Enter and leave by direction, snappily: a 350ms run over ~150px (decelerating
 * in, accelerating out), with a quick fade at the start of an entrance and at
 * the end of an exit, so the motion carries it rather than the opacity.
 * A hard rule: entrances come up from below, exits go down, the same lean for all. */
const RUN = 350;
/** Each agent arrives ~0.55s before its first press and settles: a clear stop,
 * so the entrance reads as its own motion, not the start of the work. */
const ENTER_FROM = { dx: -30, dy: 150 } as const;
const EXIT_TO = { dx: 30, dy: 150 } as const;
const ENTER = { fade: 220 };
const EXIT = { delay: 150 };

/** Presses, on the pulse. */
const T = {
  geminiIn: b(1.1) - 900, boxStart: b(1.1), boxEnd: b(2.3), pickup: b(3.1), drop: b(4.5), geminiOut: b(4.8),
  // OpenAI clicks the dock 1.6s before Gemini's drop beside it, so they never read as linked
  openaiIn: b(2.6) - 900, dock: b(2.6), picks: [b(3.9), b(4.75)], deleteHold: b(5.65), deleted: b(5.85), close: b(7.05), openaiOut: b(7.25),
  // Claude: Maps drags are quick and decisive (450ms); one zoom step each way.
  // Moves are given room: no hand travels faster than ~15px a frame.
  mapsMove: [b(0.6), b(0.6) + 450], zoomOut: b(1.9), pans: [[b(3), b(4.2)], [b(4.7), b(5.9)]],
  zoomIn: b(6.6), mapsBack: [b(7.6), b(7.6) + 450],
} as const;

export const DESKTOP_ROUTES: Record<DesktopPeer, readonly Stop[]> = {
  gemini: [
    // Enters up from below, onto the box-select's corner
    [0, 'desk-marquee', 0, 0, ENTER_FROM.dx, ENTER_FROM.dy], [T.geminiIn, 'desk-marquee', 0, 0, ENTER_FROM.dx, ENTER_FROM.dy],
    [T.geminiIn + RUN, 'desk-marquee', 0, 0],
    // Settles on the corner a moment before pressing
    [T.boxStart - 180, 'desk-marquee', 0, 0], [T.boxStart, 'desk-marquee', 0, 0],
    // Box-select: one steady drag, top-left to bottom-right
    [T.boxEnd, 'desk-marquee', 1, 1],
    // Pick up the middle file near its lower-right corner (the stack rides up-left)
    [T.pickup - 50, 'desk-file-1', .85, .7], [T.pickup, 'desk-file-1', .85, .7],
    [T.drop, 'dock-trash'], [T.geminiOut, 'dock-trash'],
    // Leaves down
    [T.geminiOut + RUN, 'dock-trash', .5, .5, EXIT_TO.dx, EXIT_TO.dy], [END, 'dock-trash', .5, .5, EXIT_TO.dx, EXIT_TO.dy],
  ],
  openai: [
    // Enters up from below, onto Photos in the dock
    [0, 'dock-photos', .5, .72, ENTER_FROM.dx, ENTER_FROM.dy], [T.openaiIn, 'dock-photos', .5, .72, ENTER_FROM.dx, ENTER_FROM.dy],
    [T.openaiIn + RUN, 'dock-photos', .5, .72],
    [T.dock - 80, 'dock-photos', .5, .72], [T.dock + 120, 'dock-photos', .5, .72],
    // Click a photo (the wallpaper follows), then another
    [T.picks[0] - 80, 'photo-0', .6, .6], [T.picks[0] + 100, 'photo-0', .6, .6],
    [T.picks[1] - 80, 'photo-1', .6, .6], [T.picks[1] + 100, 'photo-1', .6, .6],
    // Back to the first, a held press deletes it
    [T.deleteHold - 80, 'photo-0', .6, .6], [T.deleted + 150, 'photo-0', .6, .6],
    // Close Photos with its close light
    [T.close - 80, 'photos-close'], [T.openaiOut, 'photos-close'],
    // Leaves down
    [T.openaiOut + RUN, 'photos-close', .5, .5, EXIT_TO.dx, EXIT_TO.dy], [END, 'photos-close', .5, .5, EXIT_TO.dx, EXIT_TO.dy],
  ],
  // Claude never leaves. Its targets are all in Maps, measured at Maps' home;
  // its route adds Maps' current offset, so holding the title bar or the edge
  // through a drag carries the hand exactly with the window.
  claude: [
    // Waits on Maps' title bar across the seam (it never runs out of work)
    [0, 'maps-bar'],
    // Drag Maps away by its title bar
    [T.mapsMove[0] - 80, 'maps-bar'], [T.mapsMove[1] + 100, 'maps-bar'],
    [T.zoomOut - 80, 'map-zoom-out'], [T.zoomOut + 120, 'map-zoom-out'],
    // Two pans along one axis: west, then back east
    [T.pans[0][0] - 50, 'map-pan-1a'], [T.pans[0][0], 'map-pan-1a'], [T.pans[0][1], 'map-pan-1b'],
    [T.pans[1][0] - 50, 'map-pan-2a'], [T.pans[1][0], 'map-pan-2a'], [T.pans[1][1], 'map-pan-2b'],
    [T.zoomIn - 80, 'map-zoom-in'], [T.zoomIn + 120, 'map-zoom-in'],
    // Drag Maps home, and hold the title bar for the next cycle
    [T.mapsBack[0] - 80, 'maps-bar'], [END, 'maps-bar'],
  ],
};

/** When each agent is on screen: its fade in and its fade out. */
const PRESENCE: Record<Exclude<DesktopPeer, 'claude'>, { in: number; out: number }> = {
  gemini: { in: T.geminiIn, out: T.geminiOut },
  openai: { in: T.openaiIn, out: T.openaiOut },
};

const progress = (time: number, start: number, end: number) => Math.max(0, Math.min(1, (time - start) / (end - start)));
/** Settle: starts at once and eases in (a zoom responding to its tap). */
const settle = (value: number) => 1 - (1 - value) ** 3;
/** Travel: the browser agent's curve. Arrival from nowhere: decelerates in (the stageshow arrival curve). */
const travel = bezier([.45, 0, .1, 1]);
const arrive = bezier([.25, .8, .25, 1]);
/** Leaving: accelerates away, but never so steeply that the last frames jump
 * (end slope 3, ~20px a frame; the cursor is mostly faded by then). */
const depart = bezier([.45, 0, .85, .55]);

function point(stop: Stop, targets: Targets, offset: Point = { x: 0, y: 0 }): Point {
  const rect = targets[stop[1]];
  if (!rect) return { x: -200, y: -200 };
  return { x: rect.x + rect.width * (stop[2] ?? .5) + (stop[4] ?? 0) + offset.x, y: rect.y + rect.height * (stop[3] ?? .5) + (stop[5] ?? 0) + offset.y };
}

/** Maps' displacement from home: a quick drag by the title bar (80px left,
 * 44 down), undone before the cycle ends. Drawn as a transform, so targets
 * inside always measure at home. (`grow` stays 0; Maps keeps its width.) */
export function mapsGeometry(loopTime: number) {
  const time = cyc(loopTime);
  // A hand drag: departure, clear travel, soft arrival (the travel curve)
  const move = travel(progress(time, T.mapsMove[0], T.mapsMove[1])) - travel(progress(time, T.mapsBack[0], T.mapsBack[1]));
  return { dx: -80 * move, dy: 44 * move, grow: 0 };
}
/** Where a point inside Maps (left-anchored) has moved to. */
const mapsOffset = (time: number): Point => {
  const g = mapsGeometry(time);
  return { x: g.dx - g.grow, y: g.dy };
};

/** The pans' drags: west, then back east (one axis, two directions; they sum to zero). */
export const MAP_PANS = T.pans.map(([at, to]) => ({ at, to }));

/** Steady strokes run on a gentle curve: the box-select, the pans, the carry
 * to the Trash (a held file glides), and the short hops between photos inside
 * Photos. Entrances take the arrival curve, exits the departure; every other
 * move travels. */
const STEADY = new Set<number>([T.boxStart, T.pickup, T.picks[0] + 100, T.picks[1] + 100, T.deleted + 150, T.pans[1][1], ...T.pans.map((pan) => pan[0])]);
const ENTRANCES = new Set<number>([T.geminiIn, T.openaiIn]);
const EXITS = new Set<number>([T.geminiOut, T.openaiOut]);

/** Gemini's routine can run ahead of the cycle by GEMINI_LEAD, wrapping across
 * the seam. In the short cycle it starts at the top, right after the new files
 * land (a cause for its entrance). */
export const GEMINI_LEAD = 0;
const geminiTime = (time: number) => (cyc(time) + GEMINI_LEAD) % END;
/** Cycle time for one agent's routine. */
const routineTime = (who: DesktopPeer, time: number) => (who === 'gemini' ? geminiTime(time) : cyc(time));

/** Carried files and their cursor use the same measured, camera-independent route. */
export function desktopPeerPosition(who: DesktopPeer, loopTime: number, targets: Targets): Point {
  const route = DESKTOP_ROUTES[who];
  // Maps moves on the cycle's clock; each routine on its own
  const mapsTime = cyc(loopTime);
  const time = routineTime(who, loopTime);
  for (let i = 1; i < route.length; i++) {
    if (time > route[i][0]) continue;
    // Claude's targets ride with Maps
    const offset = who === 'claude' ? mapsOffset(mapsTime) : undefined;
    const from = point(route[i - 1], targets, offset);
    const to = point(route[i], targets, offset);
    const raw = progress(time, route[i - 1][0], route[i][0]);
    const start = route[i - 1][0];
    const steady = STEADY.has(start);
    const t = steady ? smooth(raw) : ENTRANCES.has(start) ? arrive(raw) : EXITS.has(start) ? depart(raw) : travel(raw);
    const still = from.x === to.x && from.y === to.y;
    const arc = still || steady || ENTRANCES.has(start) || EXITS.has(start) ? 0 : Math.sin(t * Math.PI) * Math.min(14, Math.hypot(to.x - from.x, to.y - from.y) * .04);
    return { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t - arc };
  }
  return point(route[route.length - 1], targets, who === 'claude' ? mapsOffset(mapsTime) : undefined);
}

/** 0 → 1 as an agent enters, 1 → 0 as it leaves; 0 while it has nothing to do. */
export function desktopPeerAlpha(who: DesktopPeer, loopTime: number) {
  // Claude always has work, so it never leaves
  if (who === 'claude') return 1;
  const time = routineTime(who, loopTime);
  const p = PRESENCE[who];
  return smooth(progress(time, p.in, p.in + ENTER.fade)) * (1 - smooth(progress(time, p.out + EXIT.delay, p.out + RUN)));
}

const near = (time: number, at: number) => Math.abs(time - at) < 80;

export const DESK_FILE_COUNT = 3;
/** When the next cycle's files start arriving: after Photos has closed, a beat
 * before Gemini comes for them (they settle by ~8.5s). On Gemini's clock below. */
const FILES_BACK = b(9.2);
const FILES_BACK_G = FILES_BACK + GEMINI_LEAD;
/** The box reaches each file as it grows (at about .2, .45 and .73 of the drag). */
const SELECTED_AT = [.2, .45, .73].map((f) => Math.round(T.boxStart + (T.boxEnd - T.boxStart) * f));
export const PHOTO_COUNT = 18;
/** The two photos OpenAI clicks (row 1 col 3, then row 2 col 2); the first is
 * deleted once the second is the wallpaper. */
export const PHOTO_PICKS = [2, 7] as const;
/** How long the wallpaper takes to crossfade to a clicked photo. */
const WALL_FADE = 600;

/** The map's zoom: in close, out one step, in again one step. */
export const MAP_ZOOM = { near: 2.2, far: 1.4 };

export function desktopFrame(loopTime: number, loop = 0) {
  const time = cyc(loopTime);
  // Which cycle this is, counting across loops (two cycles a loop): each one
  // starts on wallpaper 2c, clicks to 2c + 1, then to 2c + 2 (the next one's start)
  const cycle = loop * 2 + (loopTime >= CYCLE ? 1 : 0);
  // Gemini's files, on Gemini's clock
  const g = geminiTime(time);
  const marquee = g >= T.boxStart && g < T.boxEnd + 300;
  const marqueeAlpha = 1 - smooth(progress(g, T.boxEnd, T.boxEnd + 300));
  const carrying = g >= T.pickup && g < T.drop;
  const liftT = smooth(progress(g, T.pickup, T.pickup + 160));
  const dropT = progress(g, T.drop, T.drop + 200);
  const gone = g >= T.drop && g < FILES_BACK_G;
  // The next loop's files arrive as new ones (not a rewind): each drops in from
  // just above on the arrival curve, 80ms apart in reading order
  const arrivals = Array.from({ length: DESK_FILE_COUNT }, (_, i) => arrive(progress(g, FILES_BACK_G + i * 80, FILES_BACK_G + i * 80 + 500)));
  // The originals stay put (dimmed) while carried, then fade as the stack
  // drops into the Trash; never removed in one frame
  const files = Array.from({ length: DESK_FILE_COUNT }, (_, i) => ({
    selected: g >= SELECTED_AT[i] && g < T.drop + 200,
    lifted: carrying || (g >= T.drop && g < T.drop + 200),
    shown: g >= FILES_BACK_G ? Math.min(1, arrivals[i] * 1.6) : gone ? 1 - smooth(dropT) : 1,
    /** 0 → 1 as a new file arrives at the loop's end (1 the rest of the time). */
    arrive: g >= FILES_BACK_G ? arrivals[i] : 1,
  }));

  // OpenAI's Photos: opened from the dock, three picked, deleted, closed
  // Opens by zooming out of its dock icon (decelerating), closes back into it (accelerating)
  const openT = settle(progress(time, T.dock + 50, T.dock + 450));
  const closeWinT = progress(time, T.close + 50, T.close + 330) ** 2;
  const photosOpen = time >= T.dock + 50 && time < T.close + 400;
  // The selection follows the clicks: the first photo, then the second, then
  // the first again for the held press that deletes it
  const picked = [
    (time >= T.picks[0] && time < T.picks[1]) || (time >= T.deleteHold - 100 && time < T.deleted),
    time >= T.picks[1] && time < T.deleteHold - 100,
  ];
  // The wallpaper: a base and the clicked photo fading in over it
  const second = time >= T.picks[1];
  const wall = {
    base: 2 * cycle + (second ? 1 : 0),
    next: 2 * cycle + (second ? 2 : 1),
    mix: settle(progress(time, second ? T.picks[1] : T.picks[0], (second ? T.picks[1] : T.picks[0]) + WALL_FADE)),
  };
  const deleteT = smooth(progress(time, T.deleted, T.deleted + 250));
  const closeT = smooth(progress(time, T.deleted + 100, T.deleted + 600));
  const deleted = time >= T.deleted && time < T.close + 400;

  // Claude's map: one step out, two pans, one step in, always about the center
  const step = (from: number, to: number, start: number) => from + (to - from) * settle(progress(time, start, start + 420));
  const zoom = time < T.zoomIn ? step(MAP_ZOOM.near, MAP_ZOOM.far, T.zoomOut) : step(MAP_ZOOM.far, MAP_ZOOM.near, T.zoomIn);
  /** Each pan's progress, 0 → 1 while dragged (the map moves exactly with the hand). */
  const pans = MAP_PANS.map((pan) => smooth(progress(time, pan.at, pan.to)));

  return {
    phase: time < T.dock ? 'start' : time < T.deleted ? 'pick' : time < T.close ? 'deleted' : time < T.zoomIn ? 'panned' : 'tidy',
    files,
    marquee,
    marqueeAlpha,
    carrying,
    liftT,
    dropT,
    trashHover: g >= T.drop - 300 && g < T.drop + 200,
    trashBump: g >= T.drop && g < T.drop + 200,
    /** The Trash's icon, 0 empty to 1 full, as macOS swaps it: full once the files
     * drop in; back to empty in the cycle's last moments, as the next files land
     * on the desk, so the cycle (and the loop) starts on an empty Trash. */
    trashFull: smooth(progress(g, T.drop + 60, T.drop + 220)) * (1 - smooth(progress(g, END - 450, END - 150))),
    photosOpen,
    /** 0 → 1 as Photos grows out of its dock icon; 1 → 0 as it closes. */
    photosT: time < T.close + 50 ? openT : 1 - closeWinT,
    photosRunning: time >= T.dock && time < T.close + 400,
    dockPhotosPress: near(time, T.dock),
    picked,
    wall,
    /** The wallpapers photos A and B show this cycle. */
    photoWalls: [2 * cycle + 1, 2 * cycle + 2] as const,
    cycle,
    deleteT,
    closeT,
    deleted,
    zoom,
    pans,
    geminiPress: (marquee && g < T.boxEnd) || carrying,
    geminiCarrying: carrying,
    openaiPress: near(time, T.dock) || T.picks.some((at) => near(time, at)) || (time >= T.deleteHold && time < T.deleted) || near(time, T.close),
    claudePress: [T.zoomOut, T.zoomIn].some((at) => near(time, at)) || MAP_PANS.some((pan) => time >= pan.at && time < pan.to)
      || [T.mapsMove, T.mapsBack].some(([at, to]) => time >= at && time < to),
  };
}
