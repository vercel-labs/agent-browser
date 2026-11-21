'use client';

// Two objects on the card's center line, modeled on the performance card: the
// app's Add to cart button, whole, and below it a dark terminal running off
// the foot. The agent mocks three endpoints the button reads, one after
// another, and the button answers each a beat later in a state it rarely
// shows: In cart, Try again, Sold out (the payoff and poster). On the
// homepage loop it then unroutes everything and the button comes back; the
// feature tab opens on the empty terminal (setup at `command-0`) and hands off
// on Sold out at `done`.
//
// The terminal streams as the performance card's does: commands fade in on
// one left edge (a wrapped command's two lines as one), and once they fill
// the panel earlier commands fade and the column scrolls up by whole commands.
//
// Commands follow `network route <url> [--abort | --body <json>]` and
// `network unroute [url]` (cli/src/output.rs), split with a shell
// continuation so each fits the panel. Routes match first-registered first
// (cli/src/native/actions.rs), so each new mock is one the page checks ahead
// of the last: no stock beats a failed price, which beats a full cart.
import type { ReactNode } from 'react';
import { IconCheck, IconRefreshCounterClockwise } from '@vercel/geistdocs/assets/icons';
import { Stage, type Act } from '../../stageshow/vendor/kit';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import '../shared/vercel.css';
import './network-scene.css';

/** The button's four states, each one of Geist's own Button looks: the
 * primary, secondary with a prefix icon (In cart, Try again) and disabled. */
type Button = 'add' | 'cart' | 'sold' | 'error';
const FACES: { key: Button; label: string; icon?: ReactNode }[] = [
  { key: 'add', label: 'Add to cart' },
  { key: 'cart', label: 'In cart', icon: <IconCheck size={16} /> },
  { key: 'sold', label: 'Sold out' },
  { key: 'error', label: 'Try again', icon: <IconRefreshCounterClockwise size={16} /> },
];

/** The performance terminal's styles: the command bright, its blue on the
 * quoted arguments (the glob and the JSON body), the continuation gray. */
const TOKEN = /('[^']*'|"[^"]*")|( \\$)/g;
function highlight(text: string) {
  const out: ReactNode[] = [];
  let at = 0;
  for (const m of text.matchAll(TOKEN)) {
    if (m.index > at) out.push(text.slice(at, m.index));
    const kind = m[1] ? 'str' : 'cont';
    out.push(<span key={m.index} className={`ab-net-${kind}`}>{m[0]}</span>);
    at = m.index + m[0].length;
  }
  if (at < text.length) out.push(text.slice(at));
  return out;
}

/** One step per command: its lines, and the button state the page lands in
 * after its next fetch. Lines inside a step sit 20px apart; each new step
 * adds a 12px gap, as on the performance card. */
type Step = { lines: string[]; button: Button };
const HEAD = '$ agent-browser network route \\';
const STEPS: Step[] = [
  { lines: [HEAD, '  "**/cart" --body \'{"qty":1}\''], button: 'cart' },
  { lines: [HEAD, '  "**/price" --abort'], button: 'error' },
  { lines: [HEAD, '  "**/stock" --body \'{"stock":0}\''], button: 'sold' },
  { lines: ['$ agent-browser network unroute'], button: 'add' },
];
const LINES = STEPS.flatMap((step, s) => step.lines.map((text, i) => ({ text, step: s, block: s > 0 && i === 0 })));

/** Line geometry (px), kept in step with network-scene.css: 20px lines, a
 * 12px gap between steps, the panel's top (54% down the card, so the button
 * gets the room above) and the height the newest line must end in, from its
 * 16px top padding to 12px above the stage's foot: 87px, the newest two-line
 * command and most of the one before. */
const LINE = 20;
const GAP = 12;
const TERM_TOP = 135;
const VIEW = SCENE_SIZE.height - TERM_TOP - 16 - 12;
const TOPS = LINES.reduce<number[]>((tops, line, i) => [...tops, i === 0 ? 0 : tops[i - 1] + LINE + (line.block ? GAP : 0)], []);
/** How far the column is scrolled (px) with `shown` lines out: just enough
 * that the newest line ends at the foot of the view. Earlier lines run on up
 * under the panel's top scrim and out through its top edge, never faded. */
const scrollFor = (shown: number) => Math.max(0, shown ? TOPS[shown - 1] + LINE - VIEW : 0);
/** Lines written once step `s` is complete. */
const linesThrough = (s: number) => LINES.filter((line) => line.step <= s).length;

/** The performance card's timing language: the page's refetch answers 640ms
 * after a command lands; the result is read before the next command. */
const FETCH = 640;
const READ = 1300;

/** `shown`: lines written. `scroll`: how far the column is scrolled (px).
 * `button`: what the page's last fetch made of the button. `live`: the script
 * has changed the button, so its label rolls; a frame the scene opens on
 * (poster or setup) shows its face whole, with no entrance. */
type State = { shown: number; scroll: number; button: Button; live: boolean };
const initial: State = { shown: 0, scroll: 0, button: 'add', live: false };
/** The payoff on its own: the stock mock in the terminal, the Sold out it forced. */
const POSTER_STEP = 2;
const poster: State = { shown: linesThrough(POSTER_STEP), scroll: scrollFor(linesThrough(POSTER_STEP)), button: 'sold', live: false };
/** The feature tab opens here: the empty terminal over Add to cart, which is
 * still the scene's state at `command-0` (the first command needs no scroll,
 * so nothing changes between `reset` and it), and the command lands at once. */
const setup = { at: 'command-0', state: initial };

async function run(a: Act<State>, from: number, to: number, scroll: number) {
  for (let s = from; s < to; s++) {
    // One command, one arrival: a wrapped command's lines fade in together,
    // in the same frame, so its continuation never reads as a second command.
    // Room first: the column moves up, then the whole command fades in place.
    const next = scrollFor(linesThrough(s));
    if (next !== scroll) {
      scroll = next;
      await a.set({ scroll });
      await a.wait(440);
    }
    a.mark(`command-${s}`);
    // A scene opened at `command-0` (the feature tab) is still in the kit's
    // first-frame placing, which turns transitions off: one short beat so the
    // first command fades in instead of snapping.
    if (s === 0) await a.wait(100);
    await a.set({ shown: linesThrough(s) });
    await a.wait(FETCH);
    if (s === POSTER_STEP) a.mark('result');
    await a.set({ button: STEPS[s].button, live: true });
    await a.wait(READ);
  }
  return scroll;
}

async function script(a: Act<State>) {
  a.mark('resume');
  await a.wait(500);
  await run(a, POSTER_STEP + 1, STEPS.length, poster.scroll);

  // The unroute brought the button back; the terminal clears for the next
  // run. `reset` marks the cleared state exactly (the tab's setup).
  await a.set({ shown: 0 });
  await a.wait(200);
  await a.snap(initial);
  a.mark('reset');
  await a.wait(400);
  await run(a, 0, POSTER_STEP + 1, 0);
  await a.set(poster);
  a.mark('done');
}

export function NetworkScene() {
  const { onPointerEnter, run: replay, ...playback } = useScenePlayback({ script, poster, setup });
  return (
    <SceneFrame
      kind="network"
      onPointerEnter={onPointerEnter}
      caption="The app's Add to cart button sits above a terminal. The agent mocks the endpoints it reads with agent-browser network route: a cart that already holds the item turns it into In cart with a check, an aborted price request into Try again with a retry icon, and a stock body of zero into a disabled Sold out. Then network unroute removes the mocks and the button returns to Add to cart."
    >
      <Stage key={replay} {...SCENE_SIZE} {...playback} poster={poster} theme="light" className="look-vercel ab-network">
        {s => <>
          {/* Every state is its own whole button, stacked in one cell: the
              new one fades in over the old, so fills never blend through a
              muddy middle, and its label rolls in as the old one rolls out. */}
          <div className="ab-net-button" data-live={s.live || undefined}>
            {FACES.map(({ key, label, icon }) => (
              <div key={key} className="ab-net-face" data-state={key} data-on={s.button === key || undefined}>
                <span className="ab-net-roll">
                  {icon && <span className="ab-net-icon">{icon}</span>}
                  <span className="ab-net-label">{label}</span>
                </span>
              </div>
            ))}
          </div>
          <div className="ab-net-term">
            <div className="ab-net-view" style={{ height: VIEW }}>
              <div className="ab-net-lines" style={{ top: -s.scroll }}>
                {LINES.map((line, i) => (
                  <div key={i} className="ab-net-line" data-on={s.shown > i || undefined} style={{ top: TOPS[i] }}>
                    {highlight(line.text)}
                  </div>
                ))}
              </div>
            </div>
            {/* Lines run up under this and out through the panel's top edge */}
            <div className="ab-net-scrim" />
          </div>
        </>}
      </Stage>
    </SceneFrame>
  );
}
