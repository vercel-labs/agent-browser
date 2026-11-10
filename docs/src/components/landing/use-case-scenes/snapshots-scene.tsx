'use client';

// Read only what matters: an abstract page, only its text lines, sits directly
// on the card's ground at the left. A solid gray outline surrounds part of it,
// everything outside washed back, and a token count, a small dark panel in the
// terminals' shape, sits vertically centered on the right reading what the
// outline holds.
//
// Beats: the page opens bare, no outline yet, beside 48,210. A first pass
// brings the outline in as it shrinks from the whole page to half of it (it
// fades up over the first part of the move, the wash with it) and the count
// drops to 16,840; a shorter hold, and a second pass shrinks it onto one
// paragraph as the count drops to 1,284, and holds. Two passes, the second
// sooner, so the reduction reads as two steps with a rhythm. The poster is the
// result. A host that cues the scene opens it on `setup` (the bare page at
// mark `pass1`); otherwise the script resets from the poster itself.
//
// Real strings: the count and "tokens". Everything else is placeholder.
import { Bar, Stage, type Act } from '../../stageshow/vendor/kit';
import { bezier } from '../../stageshow/vendor/kit/ease';
import { clock } from '../../stageshow/vendor/kit/clock';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import './vercel.css';
import './snapshots-scene.css';

/** `pos`: where the outline is, 0 around most of the page, 1 around half of
 * it, 2 on the single paragraph; in between, it is shrinking. `scope`: the
 * outline and its wash's opacity. `tokens`: the count. `head`: how open the
 * count's ten-thousands cell is, 1 to 0. */
type State = { pos: number; tokens: number; head: number; scope: number };

type Box = { x: number; y: number; w: number; h: number };
type Rect = { x: number; y: number; width: number; height: number };

/** The outline sits PAD outside what it surrounds, at one radius throughout. */
const PAD = 8;
/** Around most of the page, the outline runs to a gutter above the stage's
 * foot; the lines carry on under it, washed. */
const FOOT = SCENE_SIZE.height - 24;
/** The count at each stop: most of the page, half of it, one paragraph. */
const COUNTS = [48210, 16840, 1284];
/** Reading time on the opening region, and the shorter beat between passes,
 * so the second pass comes sooner than the first. */
const OPEN_MS = 1500;
const BETWEEN_MS = 1150;
/** Each pass: an on-screen morph, so ease-in-out, short and decisive. The
 * count rides the same eased value, so outline and number land together. */
const PASS_MS = 850;
const shrinkEase = bezier([0.65, 0, 0.25, 1]);
/** The outline fades up over this first share of the first pass, on the same
 * ease, so it arrives while it is already moving. */
const ENTER = 0.45;
/** Uncued replays only: the outline fades and the count climbs back. */
const RESET_MS = 450;
/** The ten-thousands cell closes as an ease-out from the frame the count drops
 * under 10,000 to the frame the outline lands. */
const headEase = bezier([0.25, 1, 0.5, 1]);
/** The result holds before the scene reports done. */
const REST_MS = 900;

/** The bare page at 48,210: the opening frame, and the state at `pass1`. */
const open: State = { pos: 0, tokens: COUNTS[0], head: 1, scope: 0 };
const poster: State = { pos: 2, tokens: COUNTS[2], head: 0, scope: 1 };
const setup = { at: 'pass1', state: open };

const lerp = (from: number, to: number, t: number) => from + (to - from) * t;
/** The box PAD outside everything in `rs`. */
const around = (...rs: Rect[]): Box => {
  const x = Math.min(...rs.map(r => r.x)), y = Math.min(...rs.map(r => r.y));
  const r = Math.max(...rs.map(r => r.x + r.width)), b = Math.max(...rs.map(r => r.y + r.height));
  return { x: x - PAD, y: y - PAD, w: r - x + PAD * 2, h: b - y + PAD * 2 };
};
/** Whole pixels, so the outline's edges stay on the pixel grid. */
const boxAt = (from: Box, to: Box, k: number): Box => {
  const x = Math.round(lerp(from.x, to.x, k)), y = Math.round(lerp(from.y, to.y, k));
  return { x, y, w: Math.round(lerp(from.x + from.w, to.x + to.w, k)) - x, h: Math.round(lerp(from.y + from.h, to.y + to.h, k)) - y };
};

/** Drive `fn` with linear progress 0 to 1 over `ms`, once per frame. A wait
 * that returns with no time passed (a cued host skipping to its cue) ends the
 * tween at once, so it never spins. */
async function tween(a: Act<State>, ms: number, fn: (t: number) => void) {
  const t0 = clock.now();
  for (let t = 0; t < 1;) {
    t = Math.min(1, (clock.now() - t0) / ms);
    fn(t);
    if (t >= 1) break;
    const before = clock.now();
    await a.wait(16);
    if (clock.now() === before) { fn(1); return; }
  }
}

/** One shrink from stop `i` to stop `i + 1`. The count's fifth cell starts
 * closing on the very frame its digit leaves, and is shut when it lands. */
async function pass(a: Act<State>, i: number) {
  const from = COUNTS[i], to = COUNTS[i + 1];
  let cross = from >= 10000 && to < 10000 ? -1 : 2;
  await tween(a, PASS_MS, t => {
    const k = shrinkEase(t);
    const tokens = Math.round(lerp(from, to, k));
    if (tokens < 10000 && cross < 0) cross = t;
    const head = to >= 10000 || cross < 0 ? 1 : cross >= 1 ? 0 : 1 - headEase((t - cross) / (1 - cross));
    a.put({ pos: i + k, tokens, head, ...(i === 0 && { scope: shrinkEase(Math.min(1, t / ENTER)) }) });
  });
}

async function script(a: Act<State>) {
  a.mark('resume');

  // Reset (uncued only; a cued host skips to `pass1` and opens on `open`):
  // the outline and wash fade off the paragraph while the count climbs back,
  // its fifth cell opening on the frame it passes 10,000.
  a.mark('reset');
  let cross = -1;
  await tween(a, RESET_MS, t => {
    const k = shrinkEase(t);
    const tokens = Math.round(lerp(COUNTS[2], COUNTS[0], k));
    if (tokens >= 10000 && cross < 0) cross = t;
    const head = cross < 0 ? 0 : cross >= 1 ? 1 : headEase((t - cross) / (1 - cross));
    a.put({ tokens, head, scope: 1 - shrinkEase(Math.min(1, t / 0.6)) });
  });
  a.put(open);
  await a.wait(OPEN_MS);

  // First pass: the outline enters as it shrinks from the whole page to half
  // of it; the count drops a little.
  a.mark('pass1');
  await pass(a, 0);
  await a.wait(BETWEEN_MS);

  // Second pass, sooner: half the page to one paragraph; the count drops a lot.
  a.mark('pass2');
  await pass(a, 1);
  a.put(poster);
  a.mark('payoff');
  await a.wait(REST_MS);
  a.mark('done');
}

/** An abstract page: a title and four paragraphs of lines, the last running
 * off the foot. Line widths are fractions of the text column, so the
 * compact layout narrows the column and every line follows. */
function Page() {
  return <div className="ab-snap-page">
    <Bar w="44%" h={8} tone={4} data-target="title" />
    <div className="ab-snap-body">
      <div className="ab-snap-para" style={{ width: '100%' }} data-target="p1"><Bar w="94%" /><Bar w="100%" /><Bar w="58%" /></div>
      <div className="ab-snap-para" style={{ width: '90%' }} data-target="p2"><Bar w="100%" /><Bar w="94%" /><Bar w="56%" /></div>
      <div className="ab-snap-para" style={{ width: '98%' }}><Bar w="92%" /><Bar w="100%" /><Bar w="84%" /><Bar w="68%" /></div>
      <div className="ab-snap-para" style={{ width: '94%' }}><Bar w="100%" /><Bar w="88%" /></div>
    </div>
  </div>;
}

/** The count in Geist Mono, one fixed 1ch cell per digit, so the panel never
 * jitters while the value changes. The ten-thousands cell's width is `head`
 * of a digit, set every frame by the script on the count's own clock; it is
 * blank under 10,000, so no clipped 1 reads 9,065 as 19,065. */
function TokenCount({ tokens, head }: { tokens: number; head: number }) {
  const lead = tokens >= 10000 ? String(Math.floor(tokens / 10000)) : '';
  const rest = String(tokens % 10000).padStart(4, '0');
  return <div className="ab-snap-count">
    <span className="ab-snap-num">
      <span className="ab-snap-cell" style={{ width: `${head}ch` }}>{lead}</span>
      <span className="ab-snap-cell">{rest[0]}</span>
      <span className="ab-snap-comma">,</span>
      {[1, 2, 3].map(i => <span key={i} className="ab-snap-cell">{rest[i]}</span>)}
    </span>
    <span className="ab-snap-unit">tokens</span>
  </div>;
}

export function SnapshotsScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster, setup });
  return <SceneFrame kind="snapshots" onPointerEnter={onPointerEnter} caption="An abstract page of text lines sits on the left of the card beside a token count of 48,210. A gray outline fades in around the page as it shrinks to half of it, washing out the rest, and the count drops to 16,840. Then it shrinks again onto a single paragraph as the count drops to 1,284 tokens.">
    <Stage key={run} {...SCENE_SIZE} {...playback} poster={poster} theme="light" unframed className="look-vercel ab-snapshots">
      {(s, rect) => {
        const title = rect('title'), p1 = rect('p1'), p2 = rect('p2');
        let box: Box | null = null;
        if (title.width > 0 && p1.width > 0 && p2.width > 0) {
          const page = around(title, p1, p2);
          const stops = [{ ...page, h: FOOT - page.y }, around(p1, p2), around(p2)];
          const i = Math.min(1, Math.floor(s.pos));
          box = boxAt(stops[i], stops[i + 1], s.pos - i);
        }
        return <>
          <Page />
          {/* The outline and the wash are one element: its border is the
              outline, its spread shadow covers the rest. */}
          {box && <div className="ab-snap-scope" style={{ left: box.x, top: box.y, width: box.w, height: box.h, opacity: s.scope }} />}
          <TokenCount tokens={s.tokens} head={s.head} />
        </>;
      }}
    </Stage>
  </SceneFrame>;
}
