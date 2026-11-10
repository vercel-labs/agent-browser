import type { Point, Rect } from '../../stageshow/vendor/kit';

// The browser story, one 17s loop: notice, fix, prove. The production site (a
// low-fi agent-browser homepage) is already open, its headline misspelled; the
// agent screenshots the hero with the typo, opens the Preview deployment of the
// fix and screenshots the corrected hero, then pastes both into the pull
// request's description, Before and After side by side. It closes the pull
// request and the preview, and the loop ends where it began: production open,
// the hand resting on the page.
//   0–0.4s      production (acme.com, a plain black circle favicon)
//   1.4–2.15s   screenshot 1: a dashed box dragged over the hero from its top left to its
//               lower right, it turns blue with a flash, the copy flies to the hand
//   3.1s        carrying it, + opens acme-git-fix-typo.vercel.app (Vercel's preview favicon)
//   4.1–4.85s   screenshot 2, the fixed hero, the same stroke: its copy joins the first
//   5.75s       + opens the pull request on GitHub (the GitHub mark)
//   6.75s       click into the empty description
//   7.25s       Meta+V: on the press the Before and After captions appear and both copies
//               fly to their places beneath them, quickly, in one eased motion each
//   12.85s      the hand closes GitHub, then 13.55s the preview: production is left
//   14.45–17s   back at its resting place on production, the opening frame, the typo there to read
// The cursor goes straight from one action to the next. Travel takes time in
// proportion to distance, and nothing is hurried: moves run 450–900ms,
// presses get their beat.

export type Peer = 'agent' | 'claude' | 'openai' | 'gemini';
export type Targets = Record<string, Rect>;
/** A route stop: a measured target, a point within it (fractions), and an optional pixel offset. */
type Stop = readonly [time: number, target: string, x?: number, y?: number, dx?: number, dy?: number];

/** The marquee's gap around the captured region, as on the screenshot card (its PAD). */
export const SHOT_GAP = 8;

/** Addresses, shared by the tabs and the commands. */
export const PRODUCTION_URL = 'acme.com';
export const PREVIEW_URL = 'acme-git-fix-typo.vercel.app';
export const PR_URL = 'github.com/acme/web/pull/128';
export const PR_TITLE = 'Fix typo in hero heading';
export const PR_NUMBER = '#128';
/** The headline's misspelled word on production, and the fix on the preview. */
export const TYPO = 'prpducts';
export const FIXED = 'products';

/** The two captures: the typo on production, then the fix on the preview.
 * Each is dragged as people drag a selection: top left down to lower right. */
const SHOTS = [
  { select: [1_400, 2_100], capture: 2_150, fromEnd: false },
  { select: [4_100, 4_800], capture: 4_850, fromEnd: false },
] as const;
/** Story beats, shared by the route and the frame. */
export const T = {
  previewPress: 3_100, preview: 3_150,
  githubPress: 5_750, github: 5_800,
  fieldPress: 6_750, pastePress: 7_250,
  /** Each copy's flight into its slot, quick: Before, then After 70ms behind it. */
  paste: [[7_300, 7_680], [7_370, 7_750]],
  closeGithub: 12_850, closePreview: 13_550,
} as const;
/** The flash peaks this long after a capture; the outline holds blue for
 * BLUE_HOLD after that, then fades (the screenshot card's timings). */
const FLASH_PEAK = 70;
const BLUE_HOLD = 380;
/** A copy lifts off the page at the flash's peak and reaches the hand in this long. */
const LIFT = 550;

const G = SHOT_GAP;
/** The marquee's corners around the captured region. */
const TOP_LEFT = ['shot-region', 0, 0, -G, -G] as const;
const BOTTOM_RIGHT = ['shot-region', 1, 1, G, G] as const;
const ROUTE: readonly Stop[] = [
  // The loop opens on production, the hand resting on the page; it goes almost at once
  [0, 'home'], [400, 'home'],
  // Screenshot 1: onto the hero's top left, one steady stroke down to its lower right
  [1_250, ...TOP_LEFT], [SHOTS[0].select[0], ...TOP_LEFT],
  [SHOTS[0].select[1], ...BOTTOM_RIGHT], [SHOTS[0].capture + 10, ...BOTTOM_RIGHT],
  // Up to + beside the production tab: the preview opens
  [3_050, 'plus-one'], [3_200, 'plus-one'],
  // Screenshot 2, the same stroke on the preview
  [3_950, ...TOP_LEFT], [SHOTS[1].select[0], ...TOP_LEFT],
  [SHOTS[1].select[1], ...BOTTOM_RIGHT], [SHOTS[1].capture + 10, ...BOTTOM_RIGHT],
  [5_700, 'plus-two'], [5_850, 'plus-two'],
  // Into the empty description, an I-beam; it stays while the copies leave it
  [6_700, 'pr-field', .6, .5], [7_550, 'pr-field', .6, .5],
  // Then aside, and rests through the payoff
  [8_450, 'pr-rest'], [11_950, 'pr-rest'],
  [12_800, 'close-github'], [12_900, 'close-github'],
  [13_500, 'close-preview'], [13_600, 'close-preview'],
  // Back down to its resting place on production: the opening frame
  [14_450, 'home'], [17_000, 'home'],
];

/** Segments that start at these times are steady strokes (the marquees). */
const STEADY = new Set<number>([SHOTS[0].select[0], SHOTS[1].select[0]]);

export const progress = (time: number, start: number, end: number) => Math.max(0, Math.min(1, (time - start) / (end - start)));
export const smooth = (value: number) => value * value * (3 - 2 * value);
/** Travel: cubic-bezier(0.45, 0, 0.1, 1), stageshow's travel curve, sampled. */
export function travel(t: number) {
  // Solve the bezier's x for t, then read its y (a few Newton steps are plenty)
  const x1 = .45, x2 = .1, y1 = 0, y2 = 1;
  const bx = (s: number) => 3 * x1 * s * (1 - s) ** 2 + 3 * x2 * s * s * (1 - s) + s ** 3;
  const by = (s: number) => 3 * y1 * s * (1 - s) ** 2 + 3 * y2 * s * s * (1 - s) + s ** 3;
  let s = t;
  for (let i = 0; i < 6; i++) {
    const d = (bx(s + 1e-4) - bx(s)) / 1e-4 || 1;
    s = Math.max(0, Math.min(1, s - (bx(s) - t) / d));
  }
  return by(s);
}
/** Arrival from nowhere: a quick start and a long, soft settle. */
export const settle = (t: number) => 1 - (1 - t) ** 3;
/** A placed flight: eases in and out with no step in speed at either end
 * (quintic smootherstep), so a copy leaves the hand and lands without a jolt. */
export const glide = (t: number) => t * t * t * (t * (t * 6 - 15) + 10);

function point(stop: Stop, targets: Targets): Point {
  const rect = targets[stop[1]];
  if (!rect) return { x: -200, y: -200 };
  return { x: rect.x + rect.width * (stop[2] ?? .5) + (stop[4] ?? 0), y: rect.y + rect.height * (stop[3] ?? .5) + (stop[5] ?? 0) };
}

export function peerPosition(_who: 'agent', time: number, targets: Targets): Point {
  for (let i = 1; i < ROUTE.length; i++) {
    if (time > ROUTE[i][0]) continue;
    const from = point(ROUTE[i - 1], targets);
    const to = point(ROUTE[i], targets);
    const span = ROUTE[i][0] - ROUTE[i - 1][0];
    const raw = progress(time, ROUTE[i - 1][0], ROUTE[i][0]);
    // The drags are steady strokes; every other move travels
    const dragging = STEADY.has(ROUTE[i - 1][0]);
    const t = dragging ? smooth(raw) : travel(raw);
    const still = from.x === to.x && from.y === to.y;
    // A gentle arc on longer trips only
    const arc = still || dragging || span < 450 ? 0 : Math.sin(t * Math.PI) * Math.min(10, Math.hypot(to.x - from.x, to.y - from.y) * .04);
    return { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t - arc };
  }
  return point(ROUTE[ROUTE.length - 1], targets);
}

const near = (time: number, at: number) => Math.abs(time - at) < 80;

/** Every screen is derived from the same playhead, including direct seeks. */
export function heroFrame(time: number) {
  const previewOpen = time >= T.preview && time < T.closePreview + 50;
  const githubOpen = time >= T.github && time < T.closeGithub + 50;
  const phase =
    time < SHOTS[0].select[0] ? 'production'
        : time < T.preview ? 'before'
          : time < SHOTS[1].select[0] ? 'preview'
            : time < T.github ? 'after'
              : time < T.paste[1][1] ? 'github'
                : githubOpen ? 'pasted'
                  : 'closing';
  const active = githubOpen ? 'github' as const : previewOpen ? 'preview' as const : 'production' as const;
  // The capture in progress: the first until the preview opens, then the second
  const shot = SHOTS[time < T.preview ? 0 : 1];
  const peak = shot.capture + FLASH_PEAK;
  const peaks = SHOTS.map(s => s.capture + FLASH_PEAK);
  /** Each copy: its lift from the page to the hand (0 to 1), whether it is
   * drawn in flight, and its paste flight into its slot (0 to 1). */
  const copies = SHOTS.map((_, i) => ({
    lift: settle(progress(time, peaks[i], peaks[i] + LIFT)),
    visible: time >= peaks[i] && time < T.paste[i][1],
    paste: glide(progress(time, T.paste[i][0], T.paste[i][1])),
    /** Landed: the slot's own image takes over. */
    attached: time >= T.paste[i][1] && githubOpen,
    /** The caption above each slot appears on the paste's press, before its image lands. */
    caption: time >= T.pastePress && githubOpen,
  }));
  return {
    phase,
    /** What the agent runs at each beat. The cursor labels are wordless marks,
     * so these are the story's script rather than on-screen text. */
    command:
      time < 600 ? `open ${PRODUCTION_URL}`
        : time < 2_600 ? 'screenshot @hero'
          : time < 3_750 ? `tab new ${PREVIEW_URL}`
            : time < 5_300 ? 'screenshot @hero'
              : time < 6_300 ? `tab new ${PR_URL}`
                : time < T.pastePress - 100 ? 'click @description'
                  : time < 11_900 ? 'press Meta+V'
                    : time < 13_900 ? 'tab close'
                      : `open ${PRODUCTION_URL}`,
    /** Tabs: production always; the preview and the pull request while open. */
    previewTab: previewOpen,
    githubTab: githubOpen,
    /** From the preview's opening until the hand leaves the strip, tabs keep
     * their shared width (compact narrows them), so the × the hand aims for
     * never moves; then production widens again, as a browser's tabs do. */
    manyTabs: time >= T.preview && time < 13_900,
    active,
    url: active === 'github' ? PR_URL : active === 'preview' ? PREVIEW_URL : PRODUCTION_URL,
    /** The marquee grows with the drag, turns blue on capture, holds, then fades. */
    marqueeSelecting: time >= shot.select[0] && time < shot.select[1],
    marqueeFromEnd: shot.fromEnd,
    marquee: time >= shot.select[0] && time < peak + BLUE_HOLD + 350,
    captured: time >= shot.capture,
    marqueeAlpha: 1 - progress(time, peak + BLUE_HOLD, peak + BLUE_HOLD + 350),
    /** The camera flash over the box: up to .85 at its peak, out by 450ms (the card's keyframes). */
    flash: time < shot.capture ? 0 : time < peak ? progress(time, shot.capture, peak) * .85 : .85 * (1 - progress(time, peak, shot.capture + 450)),
    copies,
    /** The empty description's placeholder and caret, until the paste begins. */
    fieldFocused: time >= T.fieldPress && time < T.pastePress && githubOpen,
    fieldEmpty: !(time >= T.pastePress && githubOpen),
    /** The agent's cursor: an I-beam over the description, the arrow elsewhere. */
    agentKind: time >= 6_450 && time < 7_550 ? 'ibeam' as const : 'arrow' as const,
    agentPress: SHOTS.some(s => time >= s.select[0] && time <= s.select[1])
      || [T.previewPress, T.githubPress, T.fieldPress, T.closeGithub, T.closePreview].some(at => near(time, at)),
  };
}
export type HeroFrame = ReturnType<typeof heroFrame>;
