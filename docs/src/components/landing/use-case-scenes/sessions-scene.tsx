'use client';

// The "Multiple agents" card. Three empty windows and three agents taking
// turns resizing each other: Lil Pix top-left, Claude bottom-left, Gemini in
// a tall window on the right that runs off the card's right edge. The windows
// are empty on purpose: the back and forth over space is the subject.
//
// Two edges, each moving on its own:
//   x  the vertical edge between the left column and Gemini's window
//   y  the horizontal edge between Lil Pix's window and Claude's
// The windows are rects built from them, so they never overlap, and every
// edge stays inside limits that keep each window's traffic lights in view.
//
// Agents pull the way people resize: an agent reaches across, holds the edge
// with its arrow pointing back at its own window and its avatar over the
// neighbour, and drags the edge into the neighbour. Every trip moves at one
// travel speed with one ease. Only the avatar and its mark squeeze with
// effort; the arrow stays rigid.
//
// The story (the poster is mid-way through beat 1; playback starts there):
//   1  Lil Pix pulls its bottom edge down into Claude's window.
//   2  Claude reaches up and pulls it back, past even.
//   3  Gemini pulls the shared vertical edge left, into both.
//   4  Lil Pix grabs it from Gemini's side before Gemini has let go: a short
//      tug, the edge creeping back, until Gemini lets go and Lil Pix takes
//      the space back. Everyone goes home and it holds.
//
// Rendering is a pure function of the playhead: every frame is a lookup, so
// seeks and the poster are identical.
import type { CSSProperties } from 'react';
import { Stage, Window, clock, type Act, type Layout } from '../../stageshow/vendor/kit';
import { bezier } from '../../stageshow/vendor/kit/ease';
import { BrandMark } from '../compat';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import { VercelArrow } from '../shared/vercel-cursor';
import '../shared/vercel.css';
import './sessions-scene.css';

/** How an agent's avatar (disc and mark) deforms while it strains at an
 * edge. 'across': drawn out along the pull and thinner across it, volume
 * kept, as if the edge were tugging it; the side nearest the arrow stays on
 * the arrow. 'along': squashed flat against the pull, as if pushing. */
const SQUEEZE_AXIS: 'along' | 'across' = 'across';
/** How far the avatar leans back from the edge at full effort, px (screen
 * pixels, beside the arrow); 0 for none. The arrow stays on the edge. */
const STRAIN_LEAN = 2;

/** The playhead, ms into the story. */
type State = { time: number };

type Who = 'agent' | 'claude' | 'gemini';
type Edge = 'x' | 'y';
type XY = { x: number; y: number };
type Box = { l: number; t: number; r: number; b: number };
const CAST: Who[] = ['claude', 'gemini', 'agent'];

const ss = (u: number) => (u <= 0 ? 0 : u >= 1 ? 1 : u * u * (3 - 2 * u));
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
const lerp = (a: number, b: number, u: number) => a + (b - a) * u;

/** A smooth curve through [ms, value] keys: flat outside, smoothstep between. */
type Curve = [number, number][];
function curve(keys: Curve, t: number) {
  if (t <= keys[0][0]) return keys[0][1];
  for (let i = 1; i < keys.length; i++) {
    const [t1, v1] = keys[i];
    if (t > t1) continue;
    const [t0, v0] = keys[i - 1];
    return v0 + (v1 - v0) * ss((t - t0) / (t1 - t0));
  }
  return keys[keys.length - 1][1];
}

// The card's geometry, in stage pixels.
const HEIGHT = SCENE_SIZE.height;
const WIDTH: Record<Layout, number> = { wide: SCENE_SIZE.width, compact: SCENE_SIZE.compact.width };
/** Authored in wide-card pixels; sideways distances scale for the compact card. */
const SX: Record<Layout, number> = { wide: 1, compact: SCENE_SIZE.compact.width / SCENE_SIZE.width };
const INSET = 16;
const GAP = 8;
const HALF = GAP / 2;
/** The window chrome's fixed title bar and corner radius. */
const BAR = 38;
const RADIUS = 12;
const BOTTOM = HEIGHT - INSET;
/** The vertical edge's centre at rest (wide), and the horizontal edge's: two
 * even rows. */
const REST_X = 224;
const REST_Y = INSET + (HEIGHT - INSET * 2 - GAP) / 2 + HALF;
/** Gemini's window is this wide, so it always runs off the right edge. */
const TALL = 240;

/** The edges in stage pixels from their offsets (wide px) from rest. */
function edgesAt(time: number, layout: Layout) {
  return { x: INSET + (REST_X - INSET + curve(X, time)) * SX[layout], y: REST_Y + curve(Y, time) };
}

/** The three windows from the two edges. */
function windowsAt(e: XY): Record<Who, Box> {
  return {
    agent: { l: INSET, t: INSET, r: e.x - HALF, b: e.y - HALF },
    claude: { l: INSET, t: e.y + HALF, r: e.x - HALF, b: BOTTOM },
    gemini: { l: e.x + HALF, t: INSET, r: e.x + HALF + TALL, b: BOTTOM },
  };
}

/** The pointer's diagonals used here: the classic up-left, and its mirrors. */
const UL = -135;
const DL = 135;
const DR = 45;

/** Where an agent is: home in its own window, or holding an edge. */
type Pos = 'home' | Edge;

/** Home: a spot in its own window's body (fractions of the visible width and
 * of the body under the title bar, riding it as it resizes), the arrow
 * pointing toward the middle of the card. */
const HOME: Record<Who, { fx: number; fy: number; a: number }> = {
  agent: { fx: 0.28, fy: 0.5, a: DL },
  claude: { fx: 0.45, fy: 0.65, a: DR },
  gemini: { fx: 0.46, fy: 0.5, a: DL },
};
/** Each agent's hold on the edges it pulls: where along the span the two
 * windows share (clear of their corners), and the heading, pointing back at
 * its own window with the avatar trailing over the neighbour. */
const HOLD: Record<string, { f: number; a: number }> = {
  'agent:y': { f: 0.45, a: UL },
  'claude:y': { f: 0.8, a: DR },
  'agent:x': { f: 0.2, a: UL },
  'gemini:x': { f: 0.68, a: DR },
};

function posAt(who: Who, pos: Pos, time: number, layout: Layout): XY {
  const e = edgesAt(time, layout);
  const span = (lo: number, hi: number, f: number) => lo + RADIUS + (hi - lo - RADIUS * 2) * f;
  if (pos === 'x') return { x: e.x, y: span(INSET, BOTTOM, HOLD[`${who}:x`].f) };
  if (pos === 'y') return { x: span(INSET, e.x - HALF, HOLD[`${who}:y`].f), y: e.y };
  const b = windowsAt(e)[who];
  const r = Math.min(b.r, WIDTH[layout]);
  const { fx, fy } = HOME[who];
  return { x: lerp(b.l, r, fx), y: b.t + BAR + (b.b - b.t - BAR) * fy };
}
const headingOf = (who: Who, pos: Pos) => (pos === 'home' ? HOME[who].a : HOLD[`${who}:${pos}`].a);

/** Every trip: one speed (px/s), one ease, never shorter than MIN_TRIP. */
const SPEED = 300;
const MIN_TRIP = 200;
const travel = bezier([0.42, 0, 0.22, 1]);

/** An agent's track: it is at `at` by `t`; between two keys at different
 * places it travels, setting off `dur` before arriving. */
type Key = { t: number; at: Pos; dur?: number };

// <story>
/** The edges' offsets from rest, wide px: x positive right (the left column
 * grows), y positive down (Lil Pix grows). */
const X: Curve = [[0, 0]];
const Y: Curve = [[0, 0]];
const TRACK: Record<Who, Key[]> = { agent: [{ t: 0, at: 'y' }], claude: [{ t: 0, at: 'home' }], gemini: [{ t: 0, at: 'home' }] };
/** When each agent holds an edge. */
const GRIPS: { who: Who; edge: Edge; t0: number; t1: number }[] = [];
/** Each agent's effort: how hard its avatar squeezes. */
const EFFORT: Record<Who, Curve> = { agent: [[0, 0]], claude: [[0, 0]], gemini: [[0, 0]] };

/** Sets off at `depart` for `at`, arriving after its length over the speed
 * (measured in the wide card, to where the goal will be on arrival). */
function go(who: Who, depart: number, at: Pos) {
  const track = TRACK[who];
  const last = track[track.length - 1];
  if (depart > last.t) track.push({ t: depart, at: last.at });
  const from = posAt(who, last.at, depart, 'wide');
  let dur = MIN_TRIP;
  for (let i = 0; i < 6; i++) {
    const to = posAt(who, at, depart + dur, 'wide');
    dur = Math.max(MIN_TRIP, (Math.hypot(to.x - from.x, to.y - from.y) / SPEED) * 1000);
  }
  track.push({ t: depart + dur, at, dur });
  return depart + dur;
}
/** A grab ramps the effort in; a release lets it go. */
const GRAB = 150;
function hold(who: Who, edge: Edge, t0: number, t1: number) {
  GRIPS.push({ who, edge, t0, t1 });
}
const effort = (who: Who, ...keys: Curve) => EFFORT[who].push(...keys);

/** Letting go: the disc springs back to round from the stretch it held, with
 * no velocity at the start, one small overshoot past round and settled in
 * about 0.4s. Its period and decay, ms. */
const RELEASE = { period: 360, decay: 110, span: 600 };
/** How strained an agent's avatar is at `time`: its effort, except through a
 * release (a fall to 0 from a real hold), where it is a damped spring instead,
 * so it dips just past round (negative) and settles. Pure in the playhead. */
function strainAt(who: Who, time: number) {
  const keys = EFFORT[who];
  for (let i = 1; i < keys.length; i++) {
    const [t0, peak] = keys[i - 1];
    if (keys[i][1] !== 0 || peak < 0.2) continue;
    const u = time - t0;
    if (u < 0 || u > RELEASE.span) continue;
    return peak * Math.exp(-u / RELEASE.decay) * Math.cos((2 * Math.PI * u) / RELEASE.period);
  }
  return Math.max(0, curve(keys, time));
}

// 1  Lil Pix, on the edge from the start, takes hold and pulls its bottom
//    edge down into Claude's.
Y.push([1300, 28]);
effort('agent', [GRAB, 0.7], [1300, 0.7], [1450, 0]);
hold('agent', 'y', GRAB, 1400);
go('agent', 1450, 'home');

// 2  Claude reaches up and pulls it back, past even.
let t = go('claude', 1650, 'y');
hold('claude', 'y', t, t + GRAB + 1150 + 120);
effort('claude', [t, 0], [t + GRAB, 0.75], [t + GRAB + 1150, 0.75], [t + GRAB + 1300, 0]);
Y.push([t + GRAB, 28], [t + GRAB + 1150, -26]);
t = go('claude', t + GRAB + 1300, 'home');

// 3  Gemini pulls the shared vertical edge left, into both.
const g = go('gemini', t - 100, 'x');
const gPull = g + GRAB;
X.push([gPull, 0], [gPull + 1000, -48]);
effort('gemini', [g, 0], [gPull, 0.75]);

// 4  Lil Pix grabs it from Gemini's side before Gemini has let go: a short
//    tug, the edge creeping back, then Gemini lets go and Lil Pix takes it.
const p = go('agent', gPull + 450, 'x');
const tug = Math.max(p + GRAB, gPull + 1000);
X.push([tug + 400, -44], [tug + 650, -46], [tug + 1050, -39], [tug + 1250, -41], [tug + 1600, -34]);
effort('gemini', [gPull + 1000, 0.8], [tug + 1600, 1.15], [tug + 1720, 0]);
effort('agent', [p, 0], [p + GRAB, 0.8], [tug + 1600, 1.15], [tug + 1700, 0.85], [tug + 2250, 0.7], [tug + 2400, 0]);
const win = tug + 1700;
X.push([win, -34], [win + 600, 20]);
hold('gemini', 'x', g, tug + 1660);
go('gemini', tug + 1760, 'home');
hold('agent', 'x', p, win + 800);
const home = go('agent', win + 850, 'home');
// </story>

/** The payoff: everyone home, Lil Pix's window wide again. */
const PAYOFF = home;
/** The story's end: the payoff held. */
const LENGTH = PAYOFF + 1200;
/** Most people see only this: Lil Pix half-way through its first pull, the
 * held edge lit, all three agents on the card. */
const POSTER = 700;
/** Reduced motion's still: beat 4's tug, Lil Pix and Gemini both on the lit
 * vertical edge with Claude in its own window, so all three agents read as
 * separate and two visibly contest the same space. */
const STILL: State = { time: tug + 850 };

/** Idle sway for a parked agent: small, slow and in each one's own rhythm,
 * so it breathes rather than jitters. */
const IDLE: Record<Who, { ax: number; ay: number; px: number; py: number; rot: number; pr: number; phase: number }> = {
  agent: { ax: 1.2, ay: 1, px: 2300, py: 2900, rot: 1.5, pr: 3700, phase: 0 },
  claude: { ax: 0.9, ay: 0.7, px: 4100, py: 5300, rot: 1, pr: 6100, phase: 1.7 },
  gemini: { ax: 1.1, ay: 0.9, px: 2100, py: 2600, rot: 1.5, pr: 3100, phase: 4.4 },
};

/** The shortest turn from one heading to another. */
const turn = (a: number, b: number, u: number) => a + ((((b - a + 540) % 360) - 180) * u);

/** Where an agent's tip is and which way its arrow points. */
function actorAt(who: Who, time: number, layout: Layout): { at: XY; a: number } {
  const keys = TRACK[who];
  let i = keys.findIndex(k => time <= k.t);
  if (i < 0) i = keys.length - 1;
  const k1 = keys[i];
  const k0 = keys[Math.max(0, i - 1)];
  if (k1.dur && k0.at !== k1.at) {
    // A straight line from where it set off to where its goal will be when
    // it gets there; from then on it rides it
    const start = k1.t - k1.dur;
    const from = posAt(who, k0.at, Math.min(time, start), layout);
    const to = posAt(who, k1.at, k1.t, layout);
    const u = travel(clamp((time - start) / k1.dur, 0, 1));
    return { at: { x: lerp(from.x, to.x, u), y: lerp(from.y, to.y, u) }, a: turn(headingOf(who, k0.at), headingOf(who, k1.at), u) };
  }
  const at = posAt(who, k1.at, time, layout);
  let a = headingOf(who, k1.at);
  if (k1.at === 'home') {
    // Parked: it breathes, easing in and out so a trip never starts with a jump
    const next = keys[i + 1];
    const { ax, ay, px, py, rot, pr, phase } = IDLE[who];
    const s = ss((time - k0.t) / 400) * (next ? ss((next.t - (next.dur ?? 0) - time) / 400) : 1);
    const w = (period: number) => Math.sin((time / period) * Math.PI * 2 + phase);
    at.x += ax * w(px) * s;
    at.y += ay * w(py) * s;
    a += rot * w(pr) * s;
  }
  return { at, a };
}

/** How strongly an edge shows as held: any hand on it fades its line in. */
const FADE = 160;
function heldEdge(edge: Edge, time: number) {
  let v = 0;
  for (const g of GRIPS) if (g.edge === edge) v = Math.max(v, Math.min(clamp((time - g.t0 + FADE) / FADE, 0, 1), clamp((g.t1 + FADE - time) / FADE, 0, 1)));
  return v;
}

/** The axis the agent's grip pulls along (degrees): the squeeze runs on it. */
function axisAt(who: Who, time: number) {
  let axis = 90;
  for (const g of GRIPS) if (g.who === who && g.t0 <= time) axis = g.edge === 'x' ? 0 : 90;
  return axis;
}

async function script(a: Act<State>) {
  // Every run starts on the Stage's initial, no visible reset: the poster, or
  // the story's start when a host cues the setup. The clock runs on from
  // whichever it is.
  a.mark('resume');
  a.mark('reset');
  let from = POSTER;
  await a.set(st => {
    from = st.time;
    return {};
  });
  const t0 = clock.now();
  let paid = false;
  for (;;) {
    const time = Math.min(LENGTH, from + clock.now() - t0);
    a.put({ time });
    if (!paid && time >= PAYOFF) {
      paid = true;
      a.mark('payoff');
    }
    if (time >= LENGTH) break;
    await a.wait(16);
  }
  a.mark('done');
}

const poster: State = { time: POSTER };
/** For hosts that cue the scene: it opens on the story's very start (the
 * even layout, Lil Pix just taking hold) and plays from `reset`. */
const setup = { at: 'reset', state: { time: 0 } satisfies State };

const place = (e: Box): CSSProperties => ({ left: e.l, top: e.t, width: e.r - e.l, height: e.b - e.t });

/** The arrow's tip to the avatar's centre, straight back along the arrow. */
const REACH = 31;
/** The disc's radius. */
const DISC = 11;
/** The arrow as drawn points up-left. */
const ARROW = -135;
const MARK: Record<Who, number> = { agent: 12, claude: 14, gemini: 14 };

type Mat = [number, number, number, number];
/** Scale by `along` on an axis at `deg` and by `across` perpendicular to it. */
function stretch(deg: number, along: number, across: number): Mat {
  const r = (deg * Math.PI) / 180;
  const c = Math.cos(r);
  const s = Math.sin(r);
  return [c * c * along + s * s * across, c * s * (along - across), c * s * (along - across), s * s * along + c * c * across];
}

/** An agent: the arrow, rigid, turned to its heading about the tip, and the
 * avatar straight behind it. Only the disc deforms with effort, on the pull
 * axis (SQUEEZE_AXIS), about its centre, kept on the arrow; the mark inside
 * keeps its aspect and only scales down a little, uniformly. Both lean back
 * from the edge (STRAIN_LEAN) and spring back to round on release. True screen pixels whatever the card's fit. */
function Agent({ who, at, a, sq, axis }: { who: Who; at: XY; a: number; sq: number; axis: number }) {
  const rad = (a * Math.PI) / 180;
  const cx = -Math.cos(rad) * REACH;
  const cy = -Math.sin(rad) * REACH;
  // Drawn out (volume kept) or squashed, on the pull axis
  const along = SQUEEZE_AXIS === 'across' ? 1 + 0.16 * sq : Math.max(0.45, 1 - 0.2 * sq);
  const across = SQUEEZE_AXIS === 'across' ? 1 / along : 1 + 0.13 * sq;
  const m = stretch(axis, along, across);
  // Its near side stays on the arrow: nudged toward the tip when squashed,
  // away from it when drawn out
  const toward = Math.abs(Math.cos(((axis - a) * Math.PI) / 180)) * DISC * (1 - along);
  // The lean: back along the pull axis, on the avatar's side of the edge
  const ux = Math.cos((axis * Math.PI) / 180);
  const uy = Math.sin((axis * Math.PI) / 180);
  const back = Math.cos(rad) * ux + Math.sin(rad) * uy > 0 ? -1 : 1;
  const lean = STRAIN_LEAN * Math.min(1, sq);
  const dx = Math.cos(rad) * toward + back * ux * lean;
  const dy = Math.sin(rad) * toward + back * uy * lean;
  // Only the disc deforms; the mark rides the same offset and keeps its
  // shape, shrinking a touch, uniformly, while the disc is drawn thin
  const disc = `matrix(${m.map(v => v.toFixed(4)).join(', ')}, ${dx.toFixed(2)}, ${dy.toFixed(2)})`;
  const fit = 1 - 0.07 * Math.min(1.15, Math.max(0, sq));
  const mark = `translate(${dx.toFixed(2)}px, ${dy.toFixed(2)}px) scale(${fit.toFixed(4)})`;
  return (
    <span className="ab-peer" data-peer={who} style={{ transform: `translate(${at.x.toFixed(2)}px, ${at.y.toFixed(2)}px)` }}>
      <span className="ab-peer-body">
        <span className="ab-peer-pointer" style={{ '--rot': `${(a - ARROW).toFixed(2)}deg` } as CSSProperties}>
          <VercelArrow className="ab-peer-arrow" />
        </span>
        <span className="ab-peer-tile" style={{ '--lx': `${(cx - DISC).toFixed(2)}px`, '--ly': `${(cy - DISC).toFixed(2)}px` } as CSSProperties}>
          <span className="ab-peer-squash">
            <span className="ab-peer-disc" style={{ transform: disc }} />
            <span className="ab-peer-mark" style={{ transform: mark }}>
              <BrandMark name={who} size={MARK[who]} />
            </span>
          </span>
        </span>
      </span>
    </span>
  );
}

function Desktop({ time, layout }: { time: number; layout: Layout }) {
  const e = edgesAt(time, layout);
  const w = windowsAt(e);
  // Each held edge's line runs the length its windows share, short of their
  // rounded corners, centred in the gap
  const lines: Record<Edge, Box> = {
    x: { l: e.x - 1, r: e.x + 1, t: INSET + RADIUS, b: BOTTOM - RADIUS },
    y: { l: INSET + RADIUS, r: e.x - HALF - RADIUS, t: e.y - 1, b: e.y + 1 },
  };
  return <>
    {(['agent', 'claude', 'gemini'] as const).map(who => (
      <Window key={who} className="ab-sessions-window" style={place(w[who])} body={{ className: 'ab-sessions-body' }} />
    ))}
    {(['x', 'y'] as const).map(edge => {
      const o = heldEdge(edge, time);
      return o > 0 ? <span key={edge} className="ab-sessions-edge" style={{ ...place(lines[edge]), opacity: o }} /> : null;
    })}
    {CAST.map(who => {
      const { at, a } = actorAt(who, time, layout);
      return <div key={who} className="ab-sessions-actor">
        <Agent who={who} at={at} a={a} sq={strainAt(who, time)} axis={axisAt(who, time)} />
      </div>;
    })}
  </>;
}

export function SessionsScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster, setup });
  return <SceneFrame kind="sessions" onPointerEnter={onPointerEnter} caption="Three windows, one agent in each. Lil Pix pulls its window down into Claude's and Claude pulls the edge back up. Gemini pulls the shared edge left into both, Lil Pix grabs it from the other side, and after a short tug Gemini lets go and Lil Pix takes the space back.">
    <Stage key={run} {...SCENE_SIZE} {...playback} poster={STILL} theme="light" unframed className="look-vercel ab-sessions">
      {(st, _rect, layout) => <Desktop time={st.time} layout={layout} />}
    </Stage>
  </SceneFrame>;
}
