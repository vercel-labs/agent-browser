'use client';

// Adapted from hero-demo (measured pointer contact) and the motion rules for
// marquees and carried items: the agent draws a box around part of a page,
// the region flashes as it is captured, and a copy of it flies to sit under
// the cursor's label. A second capture stacks its copy on the first, then the
// stack fades as the next selection begins. The cycle pauses halfway through it.
import { Bar, Stage, type Act, type Point } from '../../stageshow/vendor/kit';
import type { ComponentType } from 'react';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import { StageAgentLabel } from './stage-agent-label';
import { Marquee } from './marquee';
import './vercel.css';
import './recording-scene.css';

type Box = { x: number; y: number; w: number; h: number };
type Carry = 'none' | 'lift' | 'held' | 'gone';
/** `left`/`top`: where the copy rests, from the hotspot. */
/** `rotate`: its tilt at rest, in degrees (clockwise), so the stack fans like a hand of cards. */
type Copy = { carry: Carry; from: Box | null; left: number; top: number; w: number; h: number; scale: number; rotate: number };
/** `fromEnd`: the drag started at the box's lower right, so its dashes anchor there. */
type State = { preview: boolean; box: Box | null; fromEnd: boolean; radius: number; marquee: boolean; shot: boolean; flash: number; para: Copy; chart: Copy };

/** Copies rest beneath the label (left 10, 24 wide), right-aligned to its
 * right edge so the stack hangs back over the page and stays in frame when the
 * cursor works near the page's right side. Offsets are from the hotspot. */
const SHOT = { right: 34, top: 48 };
const CHART_SCALE = 0.5;
const PAD = 8;
/** The tile's corner radius (.ab-shot-tile in the CSS). The marquee sits PAD
 * outside what it captures, so its corners are concentric at radius + PAD. */
const TILE_RADIUS = 8;
const TILE = { w: 168, h: 96 };
const COPY_W = TILE.w * CHART_SCALE;
/** Where each copy rests: fanned like a deck of cards. Nearly the same size,
 * both turned clockwise about their top-left corner; the front one sits a
 * little right of the back one and turns further, so the pair reads as fanned
 * out rather than as a smaller card stacked on a bigger one. */
type Rest = { right: number; top: number; w: number; rotate: number };
const BACK: Rest = { right: SHOT.right, top: SHOT.top, w: COPY_W, rotate: 3 };
const FRONT: Rest = { right: SHOT.right + 8, top: SHOT.top + 2, w: COPY_W - 4, rotate: 8 };
/** How long the outline holds at full blue after the flash, while the copy flies. */
const BLUE_HOLD = 380;
/** The copy leaves at the flash's peak, not after it. */
const FLASH_PEAK = 70;
const heights = [40, 62, 50, 76, 58, 88];

const none: Copy = { carry: 'none', from: null, left: 0, top: 0, w: 0, h: 0, scale: 1, rotate: 0 };
const resting = (rest: Rest, region: { w: number; h: number }): Copy => {
  const scale = rest.w / region.w;
  return { carry: 'held', from: null, left: rest.right - rest.w, top: rest.top, w: rest.w, h: region.h * scale, scale, rotate: rest.rotate };
};
const initial: State = { preview: false, box: null, fromEnd: false, radius: PAD, marquee: false, shot: false, flash: 0, para: none, chart: none };
/** Halfway through drawing the first capture, in either responsive layout. */
const poster: State = {
  ...initial,
  preview: true,
  radius: TILE_RADIUS + PAD,
  marquee: true,
};

const straight = (t: number, from: Point, to: Point) => ({ x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t });
/** Whole pixels, so the 1px dashes land on the pixel grid at full strength
 * instead of antialiasing across two rows at half. */
const boxFrom = (start: Point, at: Point): Box => {
  const x = Math.round(Math.min(start.x, at.x)), y = Math.round(Math.min(start.y, at.y));
  return { x, y, w: Math.round(Math.max(start.x, at.x)) - x, h: Math.round(Math.max(start.y, at.y)) - y };
};

async function script(a: Act<State>) {
  a.mark('resume');
  await a.wait(200);
  const tile = a.rect('tile');
  const para = a.rect('para');
  const tileStart = { x: tile.x - PAD, y: tile.y - PAD };
  const tileMiddle = { x: tile.x + tile.width / 2, y: tile.y + tile.height / 2 };

  /** Draw the marquee from one corner to the opposite one. */
  const select = async (start: Point, to: Point, radius: number, duration = 850, resume = false) => {
    if (!resume) await a.move(start, { duration: 700 });
    let end = start;
    const fromEnd = to.x < start.x;
    await a.drag(to, (at) => {
      end = at;
      a.put({ preview: false, box: boxFrom(start, at), fromEnd, radius, marquee: true, shot: false });
    }, { duration, cursor: 'preserve', path: straight });
    return end;
  };

  /** The shutter: the dashes turn blue as a white camera flash runs over the
   * region, and at the flash's peak the copy lifts off `region` at full size,
   * ready to fly to `rest`. */
  const capture = async (region: Box, end: Point, flash: number, rest: Rest): Promise<Copy> => {
    await a.set({ shot: true, flash });
    await a.wait(FLASH_PEAK);
    const held = resting(rest, region);
    const from = { x: region.x - (end.x + held.left), y: region.y - (end.y + held.top), w: region.w, h: region.h };
    return { ...held, carry: 'lift', from };
  };
  /** Fly the copy to rest straight away; the outline fades on its own clock. */
  const fly = async (key: 'chart' | 'para', copy: Copy) => {
    await a.snap({ [key]: copy } as Partial<State>);
    await a.set({ [key]: { ...copy, carry: 'held' } } as Partial<State>);
    await a.wait(BLUE_HOLD - FLASH_PEAK);
    await a.set({ marquee: false });
  };

  // Finish the half-drawn selection already shown in the poster.
  const end1 = await select(tileStart, { x: tile.x + tile.width + PAD, y: tile.y + tile.height + PAD }, TILE_RADIUS + PAD, 425, true);
  a.mark('capture');
  await fly('chart', await capture({ x: tile.x, y: tile.y, w: tile.width, h: tile.height }, end1, 1, BACK));
  // The cursor holds still until the copy has landed, so the flight is one arc.
  await a.wait(600);

  // Second capture: the paragraph beside it. Its copy lands on top, fanned out.
  a.mark('select-para');
  const start2 = { x: para.x - PAD, y: para.y - PAD };
  const end2 = await select(start2, { x: para.x + para.width + PAD, y: para.y + para.height + PAD }, PAD);
  a.mark('capture-para');
  await fly('para', await capture(boxFrom(start2, end2), end2, 2, FRONT));
  await a.wait(300);
  a.mark('result');
  // Done: the hand drifts back across the page, slow and unhurried, carrying
  // the fanned copies, rather than stopping dead on the last capture
  await a.move({ x: tile.x + tile.width * 0.5, y: end2.y + 6 }, { duration: 1400 });
  await a.wait(250);

  // Clear the captures, then draw only the first half of the next selection.
  a.mark('reset');
  await a.set(s => ({ para: { ...s.para, carry: 'gone' }, chart: { ...s.chart, carry: 'gone' } }));
  await a.wait(350);
  await a.snap(initial);
  a.mark('select');
  await select(tileStart, tileMiddle, TILE_RADIUS + PAD, 425);
  await a.set(poster);
  await a.cursor('pointer');
  a.mark('done');
}

function ChartTile() {
  return <div className="ab-shot-chart">
    <Bar w={56} h={6} tone={3} />
    <div className="ab-shot-bars">{heights.map((h, i) => <span key={i} style={{ height: `${h}%` }} />)}</div>
  </div>;
}

function ParaLines() {
  return <><Bar w={84} h={6} /><Bar w={70} h={6} /><Bar w={54} h={6} /></>;
}

/** The paragraph as captured: its lines with the marquee's padding around
 * them, drawn at the measured size of the captured region. */
function ParaShot({ w, h }: { w: number; h: number }) {
  return <div className="ab-shot-para" style={{ width: w, height: h }}><ParaLines /></div>;
}

function Carried({ copy, Body }: { copy: Copy; Body: ComponentType<{ w: number; h: number }> }) {
  const lift = copy.carry === 'lift' && copy.from;
  // Same function list in both states, so the flight turns the copy into its tilt
  const transform = lift
    ? `translate(${copy.from!.x}px, ${copy.from!.y}px) scale(${1 / copy.scale}) rotate(0deg)`
    : `translate(0px, 0px) scale(1) rotate(${copy.rotate}deg)`;
  return <span className="ab-shot-copy" data-carry={copy.carry} style={{ left: copy.left, top: copy.top, width: copy.w, height: copy.h, transform }}>
    <span className="ab-shot-copy-inner" style={{ transform: `scale(${copy.scale})` }}><Body w={copy.w / copy.scale} h={copy.h / copy.scale} /></span>
  </span>;
}

export function RecordingScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster });
  return <SceneFrame kind="verify" onPointerEnter={onPointerEnter} caption="An agent draws dashed boxes around a chart and then a paragraph on an abstract page, capturing each with a brief blue flash. The screenshots stack beneath its cursor.">
    <Stage key={run} {...SCENE_SIZE} {...playback} poster={poster} theme="light" unframed className="look-vercel ab-recording" cursor={{ at: { at: 'tile', x: .5, y: .5 }, style: 'arrow', still: true, idle: false }}>
      {(s, rect) => {
        const tile = rect('tile');
        let box = s.box;
        if (s.preview) {
          box = tile.width > 0 ? boxFrom({ x: tile.x - PAD, y: tile.y - PAD }, { x: tile.x + tile.width / 2, y: tile.y + tile.height / 2 }) : null;
        }
        return <>
        <div className="ab-shot-page" data-target="page">
          <Bar w={112} h={8} tone={3} />
          <div className="ab-shot-row">
            <div className="ab-shot-tile" data-target="tile"><ChartTile /></div>
            <div className="ab-shot-lines ab-shot-side" data-target="para"><ParaLines /></div>
          </div>
          <div className="ab-shot-lines"><Bar w={252} h={6} /><Bar w={164} h={6} /></div>
        </div>
        {box && <Marquee box={box} radius={s.radius} fromEnd={s.fromEnd} className="ab-shot-marquee" data-show={s.marquee || undefined} data-shot={s.shot || undefined} />}
        {s.box && s.flash > 0 && <div key={s.flash} className="ab-shot-flash" style={{ left: s.box.x, top: s.box.y, width: s.box.w, height: s.box.h, borderRadius: s.radius }} />}
        <StageAgentLabel>
          <Carried copy={s.chart} Body={ChartTile} />
          <Carried copy={s.para} Body={ParaShot} />
        </StageAgentLabel>
      </>; }}
    </Stage>
  </SceneFrame>;
}
