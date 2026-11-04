import type { Point, Rect } from '../../stageshow/vendor/kit';

// The browser story, one 17s loop (it opens and ends on the empty window, the hand on +):
//   0.5–4.4s   a logged-out mood board: Log in, a small modal, email, password, submit
//   4.4–6.9s   signed in: drag a box around a block of the board, flash, the copy
//              flies to sit beside the cursor
//   6.9–7.9s   carrying it, open a new tab: mail
//   7.9–11.9s  compose, fill To, upload the capture into the body, send
//   12–14.3s   the message sent, a new row in the list: the payoff holds
//   14.3–15.5s the cursor closes Mail, then the Mood board: closing the last tab
//              ends the session, so the board comes back logged out. The close is
//              the reset's cause.
//   15.5–17s   the empty window holds 1.5s before the next loop's board appears
// The cursor goes straight from one action to the next; it never returns to a
// parking spot between them. Travel takes time in proportion to distance, and
// nothing is hurried: moves run 450–900ms, presses and typing get their beat.

export type Peer = 'agent' | 'claude' | 'openai' | 'gemini';
export type Targets = Record<string, Rect>;
/** A route stop: a measured target, a point within it (fractions), and an optional pixel offset. */
type Stop = readonly [time: number, target: string, x?: number, y?: number, dx?: number, dy?: number];

/** The marquee's gap around the board, and its corner radius (tile radius + gap). */
export const BOARD_PAD = 6;

const ROUTE: readonly Stop[] = [
  // The loop opens on the empty window: the hand on +, which it clicks to open the board's tab
  // It waits just below-right of + (so the button reads), then moves onto it to click
  [0, 'plus-home', 1.5, 1.7], [120, 'plus-home', 1.5, 1.7], [330, 'plus-home'], [400, 'plus-home'],
  [1_300, 'login', .3, .6], [1_400, 'login', .3, .6],
  // Filling a field: the I-beam lands at the value's start, then follows it to
  // its end as the gray bar appears (the bar is 104 of the field's 178, from 10)
  [1_950, 'email', .06, .5], [2_050, 'email', .06, .5],
  [2_450, 'email', .64, .5], [2_750, 'email', .64, .5],
  [3_300, 'password', .06, .5], [3_380, 'password', .06, .5],
  [3_700, 'password', .42, .5], [3_800, 'password', .42, .5],
  [4_300, 'submit', .8, .6], [4_400, 'submit', .8, .6],
  [5_150, 'board', 0, 0, -BOARD_PAD, -BOARD_PAD], [5_200, 'board', 0, 0, -BOARD_PAD, -BOARD_PAD],
  // The drag: one steady motion, the box following the hand
  [6_200, 'board', 1, 1, BOARD_PAD, BOARD_PAD],
  // Heads for + as the capture fires, so the copy (beneath the label) flies to
  // a rising hand and never hangs off the foot of the view
  [6_260, 'board', 1, 1, BOARD_PAD, BOARD_PAD],
  [7_800, 'tab-plus'], [7_900, 'tab-plus'],
  [8_650, 'compose', .5, .5], [8_750, 'compose', .5, .5],
  // The To value: 104 of the line's 208, from its start
  [9_300, 'to', .02, .5], [9_400, 'to', .02, .5],
  [9_800, 'to', .5, .5], [10_150, 'to', .5, .5],
  // The paste starts with the press itself
  [10_650, 'attach'], [11_100, 'attach'],
  [11_800, 'send', .7, .6], [11_900, 'send', .7, .6],
  [12_500, 'mail-rest'], [14_300, 'mail-rest'],
  [14_850, 'close-mail'], [14_950, 'close-mail'],
  [15_350, 'close-pin'], [15_450, 'close-pin'],
  // Then left again, onto +, where it waits out the pause and the next loop begins
  [16_050, 'plus-home', 1.5, 1.7], [17_000, 'plus-home', 1.5, 1.7],
];

/** Segments that start at these times are steady strokes (the board drag, the I-beam along each value). */
const STEADY = new Set([5_200, 2_050, 3_380, 9_400]);

export const progress = (time: number, start: number, end: number) => Math.max(0, Math.min(1, (time - start) / (end - start)));
export const smooth = (value: number) => value * value * (3 - 2 * value);
/** Travel: cubic-bezier(0.45, 0, 0.1, 1), stageshow's travel curve, sampled. */
function travel(t: number) {
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
    // The drag and the I-beam's run along a filled value are steady strokes;
    // every other move travels
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
  // Closed from the last tab's close, through the 1.5s pause, until + opens the board at 0.4s
  const closed = time >= 15_450 || time < 400;
  const mailOpen = time >= 7_950 && time < 14_950;
  const signedIn = time >= 4_400 && time < 15_450;
  const modal = time >= 1_450 && time < 4_400;
  const composing = time >= 8_800 && time < 11_950;
  const sent = time >= 11_950 && time < 14_950;
  const phase =
    closed ? 'closed'
      : time < 1_450 ? 'logged-out'
        : modal ? 'log-in'
          : time < 6_250 ? 'select'
            : time < 7_950 ? 'captured'
              : time < 8_800 ? 'inbox'
                : composing ? 'compose'
                  : time < 14_950 ? 'sent'
                    : 'closing';
  return {
    phase,
    command:
      time < 500 ? 'tab new'
        : time < 1_450 ? 'click @login'
          : time < 2_850 ? 'fill @email'
            : time < 3_850 ? 'fill @password'
              : time < 4_450 ? 'click @submit'
                : time < 6_950 ? 'screenshot @board'
                  : time < 8_000 ? 'tab new'
                    : time < 8_850 ? 'click @compose'
                      : time < 10_200 ? 'fill @to'
                        : time < 11_300 ? 'upload @body board.png'
                          : time < 14_300 ? 'click @send'
                            : 'tab close',
    /** The pill leaves once `click @send` is done; back for `tab close`. */
    commandShown: !(time >= 12_400 && time < 14_300),
    /** Tabs: the mood board, then mail; none while the session is closed. */
    pinTab: !closed,
    mailTab: mailOpen,
    active: mailOpen ? 'mail' : 'pin',
    /** The address bar is abstract: a placeholder line, no domain. */
    addressShown: !closed,
    signedIn,
    modal,
    /** No words: a field's gray bar appears whole as the cursor's I-beam runs along it. */
    emailFilled: time >= 2_050,
    passwordFilled: time >= 3_380,
    /** The agent's cursor is an I-beam while it fills a field, as the cards' is. */
    agentKind: (time >= 1_950 && time < 2_850) || (time >= 3_300 && time < 3_850) || (time >= 9_300 && time < 10_200) ? 'ibeam' as const : 'arrow' as const,
    emailFocused: time >= 1_950 && time < 2_850,
    passwordFocused: time >= 3_300 && time < 3_850,
    /** The marquee grows with the drag, holds, turns blue on capture, then fades. */
    selecting: time >= 5_200 && time < 6_200,
    marquee: time >= 5_200 && time < 7_000,
    captured: time >= 6_250,
    marqueeAlpha: 1 - smooth(progress(time, 6_650, 7_000)),
    /** The camera flash over the board: up fast, out over ~0.45s. */
    flash: time < 6_250 ? 0 : time < 6_310 ? progress(time, 6_250, 6_310) * .85 : .85 * (1 - settle(progress(time, 6_310, 6_750))),
    /** The copy: lifts off the board at its full size and flies to the hand. */
    copyFlight: settle(progress(time, 6_300, 6_850)),
    /** The copy is drawn until it lands in the message, where the attachment takes over. */
    carryVisible: time >= 6_300 && time < 11_100,
    /** The paste: from the hand into the message body, starting with the press. */
    pasteFlight: settle(progress(time, 10_650, 11_100)),
    attached: time >= 11_100,
    composing,
    toFilled: time >= 9_400,
    toFocused: time >= 9_300 && time < 10_200,
    sent,
    /** The sent message's row opens at the top of the list. */
    sentRow: smooth(progress(time, 12_050, 12_450)),
    agentPress: (time >= 5_200 && time <= 6_200) || [350, 1_400, 1_950, 3_300, 4_400, 7_900, 8_750, 9_300, 10_650, 11_900, 14_950, 15_450].some(at => near(time, at)),
  };
}
export type HeroFrame = ReturnType<typeof heroFrame>;
