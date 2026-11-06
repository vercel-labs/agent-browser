import type { Point } from '../../stageshow/vendor/kit';
import type { Targets } from './hero-timeline';

// A lived-in desktop: the browser leading, Maps beside it, a few loose files.
// Three agents work in parallel on one pulse (B = 850ms, 20 beats a loop).
// Inactivity decides presence: an agent enters (up from below) just before
// its first action and leaves (down) once it has nothing to do for a while.
// The browser agent is always there, and Gemini, OpenAI and Claude overlap,
// so all four cursors share the screen every loop (about 2.4–4.8s).
// Presses that share a beat land together, one rhythm rather than solos:
//   Claude   drags Maps away by its title bar and widens it from its left edge
//            (quick, decisive drags), taps zoom out on OpenAI's second and third
//            picks, pans west and back east, taps zoom in, narrows Maps and drags
//            it home, then leaves (idle across the seam)
//   Gemini   box-selects the loose files and drags them to the Trash (b5), leaves
//   OpenAI   opens Photos from the dock (b3.5, well before Gemini's drop), picks
//            three photos one a beat, deletes them, closes Photos (b9), leaves
// Every zoom is about the map's center. The loop ends where it began: the
// files settle back in near the seam, Photos is closed, Maps is home, in close
// and centered, and no desktop agent is on screen.

type DesktopPeer = 'claude' | 'openai' | 'gemini';
/** A route stop: a target, a point inside it (fractions), and an offset in pixels. */
type Stop = readonly [time: number, target: string, x?: number, y?: number, dx?: number, dy?: number];

/** The pulse: every press lands on a beat or a half beat. */
export const BEAT = 850;
const b = (beats: number) => Math.round(beats * BEAT);
const END = 17_000;

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
  geminiIn: b(1.1) - 900, boxStart: b(1.1), boxEnd: b(2.5), pickup: b(3.5), drop: b(5), geminiOut: b(5.3),
  openaiIn: b(3) - 900, dock: b(3), picks: [b(4.5), b(5.5), b(6.5)], deleteHold: b(7.35), deleted: b(7.5), close: b(9), openaiOut: b(9.2),
  // Maps moves are quick, decisive drags (450ms)
  claudeIn: b(1.8) - 900, mapsMove: [b(1.8), b(1.8) + 450], mapsGrow: [b(3.2), b(3.2) + 450],
  // Moves are given room: no hand travels faster than ~15px a frame. The first
  // pan starts on OpenAI's delete (b7.5)
  zoomOut: [b(5.5), b(6.5)], pans: [[b(7.5), b(9)], [b(10), b(11.5)]],
  // The tail runs later so the seam has little dead air: Claude leaves ~14.7s,
  // new files arrive from 15.4s, Gemini enters at the top of the next loop
  zoomIn: [b(13), b(14)], mapsShrink: [b(15), b(15) + 450], mapsBack: [b(16.5), b(16.5) + 450], claudeOut: b(16.5) + 700,
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
    // One photo a beat
    [T.picks[0] - 80, 'photo-0', .6, .6], [T.picks[0] + 100, 'photo-0', .6, .6],
    [T.picks[1] - 80, 'photo-1', .6, .6], [T.picks[1] + 100, 'photo-1', .6, .6],
    [T.picks[2] - 80, 'photo-2', .6, .6],
    // A held press on the last pick deletes all three
    [T.deleted + 150, 'photo-2', .6, .6],
    // Close Photos with its close light
    [T.close - 80, 'photos-close'], [T.openaiOut, 'photos-close'],
    // Leaves down
    [T.openaiOut + RUN, 'photos-close', .5, .5, EXIT_TO.dx, EXIT_TO.dy], [END, 'photos-close', .5, .5, EXIT_TO.dx, EXIT_TO.dy],
  ],
  // Claude never leaves. Its targets are all in Maps, measured at Maps' home;
  // its route adds Maps' current offset, so holding the title bar or the edge
  // through a drag carries the hand exactly with the window.
  claude: [
    // Enters up from below, onto Maps' title bar (it was idle across the seam)
    [0, 'maps-bar', .5, .5, ENTER_FROM.dx, ENTER_FROM.dy], [T.claudeIn, 'maps-bar', .5, .5, ENTER_FROM.dx, ENTER_FROM.dy],
    [T.claudeIn + RUN, 'maps-bar'],
    // Drag Maps a little by its title bar
    [T.mapsMove[0] - 80, 'maps-bar'], [T.mapsMove[1] + 100, 'maps-bar'],
    // Widen it from its left edge
    [T.mapsGrow[0] - 80, 'maps-edge'], [T.mapsGrow[1] + 100, 'maps-edge'],
    [T.zoomOut[0] - 80, 'map-zoom-out'], [T.zoomOut[1] + 120, 'map-zoom-out'],
    // Two pans along one axis: west, then back east
    [T.pans[0][0] - 50, 'map-pan-1a'], [T.pans[0][0], 'map-pan-1a'], [T.pans[0][1], 'map-pan-1b'],
    [T.pans[1][0] - 50, 'map-pan-2a'], [T.pans[1][0], 'map-pan-2a'], [T.pans[1][1], 'map-pan-2b'],
    [T.zoomIn[0] - 80, 'map-zoom-in'], [T.zoomIn[1] + 120, 'map-zoom-in'],
    // Narrow Maps again, then drag it home
    [T.mapsShrink[0] - 80, 'maps-edge'], [T.mapsShrink[1] + 100, 'maps-edge'],
    [T.mapsBack[0] - 80, 'maps-bar'], [T.claudeOut, 'maps-bar'],
    // Done until the next loop, so it leaves down
    [T.claudeOut + RUN, 'maps-bar', .5, .5, EXIT_TO.dx, EXIT_TO.dy], [END, 'maps-bar', .5, .5, EXIT_TO.dx, EXIT_TO.dy],
  ],
};

/** When each agent is on screen: its fade in and its fade out. */
const PRESENCE: Record<DesktopPeer, { in: number; out: number }> = {
  gemini: { in: T.geminiIn, out: T.geminiOut },
  openai: { in: T.openaiIn, out: T.openaiOut },
  claude: { in: T.claudeIn, out: T.claudeOut },
};

const progress = (time: number, start: number, end: number) => Math.max(0, Math.min(1, (time - start) / (end - start)));
const smooth = (value: number) => value * value * (3 - 2 * value);
/** Settle: starts at once and eases in (a zoom responding to its tap). */
const settle = (value: number) => 1 - (1 - value) ** 3;
function bezier(x1: number, y1: number, x2: number, y2: number) {
  return (t: number) => {
    const bx = (s: number) => 3 * x1 * s * (1 - s) ** 2 + 3 * x2 * s * s * (1 - s) + s ** 3;
    const by = (s: number) => 3 * y1 * s * (1 - s) ** 2 + 3 * y2 * s * s * (1 - s) + s ** 3;
    let s = t;
    for (let i = 0; i < 6; i++) {
      const d = (bx(s + 1e-4) - bx(s)) / 1e-4 || 1;
      s = Math.max(0, Math.min(1, s - (bx(s) - t) / d));
    }
    return by(s);
  };
}
/** Travel: the browser agent's curve. Arrival from nowhere: decelerates in (the stageshow arrival curve). */
const travel = bezier(.45, 0, .1, 1);
const arrive = bezier(.25, .8, .25, 1);
/** Leaving: accelerates away, but never so steeply that the last frames jump
 * (end slope 3, ~20px a frame; the cursor is mostly faded by then). */
const depart = bezier(.45, 0, .85, .55);

function point(stop: Stop, targets: Targets, offset: Point = { x: 0, y: 0 }): Point {
  const rect = targets[stop[1]];
  if (!rect) return { x: -200, y: -200 };
  return { x: rect.x + rect.width * (stop[2] ?? .5) + (stop[4] ?? 0) + offset.x, y: rect.y + rect.height * (stop[3] ?? .5) + (stop[5] ?? 0) + offset.y };
}

/** Maps' displacement from home: a quick drag by the title bar (80px left,
 * 44 down) and a widening from the left edge (80px), each undone near the loop's end.
 * Drawn as a transform plus width, so targets inside always measure at home. */
export function mapsGeometry(time: number) {
  // A hand drag: departure, clear travel, soft arrival (the travel curve)
  const move = travel(progress(time, T.mapsMove[0], T.mapsMove[1])) - travel(progress(time, T.mapsBack[0], T.mapsBack[1]));
  const grow = 80 * (travel(progress(time, T.mapsGrow[0], T.mapsGrow[1])) - travel(progress(time, T.mapsShrink[0], T.mapsShrink[1])));
  return { dx: -80 * move, dy: 44 * move, grow };
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
const ENTRANCES = new Set<number>([T.geminiIn, T.openaiIn, T.claudeIn]);
const EXITS = new Set<number>([T.geminiOut, T.openaiOut, T.claudeOut]);

/** Gemini's routine runs ahead of the loop by GEMINI_LEAD, wrapping across the
 * seam: it enters right after the next loop's files land (about 16.3s), so the
 * end of the loop has little dead air and its entrance has a cause. */
export const GEMINI_LEAD = 735;
const geminiTime = (time: number) => (time + GEMINI_LEAD) % END;
/** Loop time for one agent's routine. */
const routineTime = (who: DesktopPeer, time: number) => (who === 'gemini' ? geminiTime(time) : time);
/** A moment on Gemini's clock, on the loop's. */
const fromGemini = (local: number) => (local - GEMINI_LEAD + END) % END;

/** Carried files and their cursor use the same measured, camera-independent route. */
export function desktopPeerPosition(who: DesktopPeer, loopTime: number, targets: Targets): Point {
  const route = DESKTOP_ROUTES[who];
  // Maps moves on the loop's clock; each routine on its own
  const mapsTime = loopTime;
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
  const time = routineTime(who, loopTime);
  const p = PRESENCE[who];
  return smooth(progress(time, p.in, p.in + ENTER.fade)) * (1 - smooth(progress(time, p.out + EXIT.delay, p.out + RUN)));
}

const near = (time: number, at: number) => Math.abs(time - at) < 80;

export const DESK_FILE_COUNT = 3;
/** When the next loop's files start arriving (loop clock): just after Claude
 * leaves, a beat before Gemini comes for them. On Gemini's clock below. */
const FILES_BACK = 15_400;
const FILES_BACK_G = FILES_BACK + GEMINI_LEAD;
/** The box reaches each file as it grows (at about .2, .45 and .73 of the drag). */
const SELECTED_AT = [.2, .45, .73].map((f) => Math.round(T.boxStart + (T.boxEnd - T.boxStart) * f));
export const PHOTO_COUNT = 18;
/** The three photos OpenAI picks: not neighbors (row 1 col 1, row 1 col 3, row 2 col 2). */
export const PHOTO_PICKS = [0, 2, 7] as const;

/** The map's zoom: in close, out in two steps, in again in two at the loop's end. */
export const MAP_ZOOM = { near: 2.2, mid: 1.6, far: 1 };

export function desktopFrame(time: number) {
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
  const picked = T.picks.map((at) => time >= at && time < T.deleted);
  const deleteT = smooth(progress(time, T.deleted, T.deleted + 250));
  const closeT = smooth(progress(time, T.deleted + 100, T.deleted + 600));
  const deleted = time >= T.deleted && time < T.close + 400;

  // Claude's map: two steps out, two pans, two steps in, always about the center
  const step = (from: number, to: number, start: number) => from + (to - from) * settle(progress(time, start, start + 420));
  const zoom = time < T.zoomOut[1] ? step(MAP_ZOOM.near, MAP_ZOOM.mid, T.zoomOut[0])
    : time < T.zoomIn[0] ? step(MAP_ZOOM.mid, MAP_ZOOM.far, T.zoomOut[1])
      : time < T.zoomIn[1] ? step(MAP_ZOOM.far, MAP_ZOOM.mid, T.zoomIn[0])
        : step(MAP_ZOOM.mid, MAP_ZOOM.near, T.zoomIn[1]);
  /** Each pan's progress, 0 → 1 while dragged (the map moves exactly with the hand). */
  const pans = MAP_PANS.map((pan) => smooth(progress(time, pan.at, pan.to)));

  return {
    phase: time < T.dock ? 'start' : time < T.deleted ? 'pick' : time < T.close ? 'deleted' : time < T.zoomIn[0] ? 'panned' : 'tidy',
    files,
    marquee,
    marqueeAlpha,
    carrying,
    liftT,
    dropT,
    trashHover: g >= T.drop - 300 && g < T.drop + 200,
    trashBump: g >= T.drop && g < T.drop + 200,
    photosOpen,
    /** 0 → 1 as Photos grows out of its dock icon; 1 → 0 as it closes. */
    photosT: time < T.close + 50 ? openT : 1 - closeWinT,
    photosRunning: time >= T.dock && time < T.close + 400,
    dockPhotosPress: near(time, T.dock),
    picked,
    deleteT,
    closeT,
    deleted,
    zoom,
    pans,
    geminiPress: (marquee && g < T.boxEnd) || carrying,
    geminiCarrying: carrying,
    openaiPress: near(time, T.dock) || T.picks.some((at) => near(time, at)) || (time >= T.deleteHold - 100 && time < T.deleted) || near(time, T.close),
    claudePress: [...T.zoomOut, ...T.zoomIn].some((at) => near(time, at)) || MAP_PANS.some((pan) => time >= pan.at && time < pan.to)
      || [T.mapsMove, T.mapsGrow, T.mapsShrink, T.mapsBack].some(([at, to]) => time >= at && time < to),
  };
}

/** Key moments, for the storyboard's stills. */
export const DESKTOP_BEATS = {
  boxSelect: fromGemini(T.boxStart + 900),
  carry: fromGemini(T.pickup + 700),
  picked: T.picks[2] + 200,
  deleted: T.deleted + 700,
  zoomedOut: T.zoomOut[1] + 400,
  panned: T.pans[0][1],
  zoomIn: T.zoomIn[1] + 200,
};
