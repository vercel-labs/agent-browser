'use client';

// Two objects at card fidelity: the request in a chat input, right-aligned,
// and a terminal panel, left-aligned and running off the foot. Both keep a
// contained width, so the alignment reads as a conversation. The agent
// measures, profiles a reload, finds the cause, fixes it and measures again.
// Lines fade in on one left edge; once they fill the panel, the column
// scrolls up by whole lines and earlier lines run up under a gradient scrim
// at the panel's top and out of its edge, as a terminal scrolls. Two text styles, blue on the
// finding and the result.
// Output follows agent-browser's real formats (cli/src/output.rs), abridged.
import { Field, Stage, type Act } from '../../stageshow/vendor/kit';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import '../shared/vercel.css';
import './performance-scene.css';

const REQUEST = 'Measure and improve perf plz';

/** cmd: a command, bright. text: output or the agent's words, gray. key: the
 * finding and the result, blue.
 *
 * One spacing rule: lines belong to steps. A tool step is a command, or a run
 * of commands, with their output; an agent step is one of its own sentences.
 * Lines inside a step sit 20px apart; every change of step adds a 12px gap.
 * The step is the source of truth, so the gaps can't drift. */
type Kind = 'cmd' | 'text' | 'key';
type Step = { by: 'tool' | 'agent'; lines: { kind: Kind; text: string }[] };
const STEPS: Step[] = [
  { by: 'tool', lines: [{ kind: 'cmd', text: '$ agent-browser vitals' }, { kind: 'text', text: 'lcp: 2410ms  cls: 0.02' }] },
  { by: 'agent', lines: [{ kind: 'text', text: 'LCP is slow. Profiling a reload.' }] },
  { by: 'tool', lines: [
    { kind: 'cmd', text: '$ agent-browser profiler start' },
    { kind: 'cmd', text: '$ agent-browser reload' },
    { kind: 'cmd', text: '$ agent-browser profiler stop trace.json' },
    { kind: 'text', text: 'Profile saved to trace.json' },
  ] },
  { by: 'agent', lines: [{ kind: 'key', text: 'Hero image blocks LCP. Preloaded it.' }] },
  { by: 'tool', lines: [{ kind: 'cmd', text: '$ agent-browser vitals' }, { kind: 'text', text: 'lcp: 980ms  cls: 0.02' }] },
  { by: 'agent', lines: [{ kind: 'key', text: 'LCP is down from 2410ms to 980ms.' }] },
];
const LINES = STEPS.flatMap((step, s) => step.lines.map((line, i) => ({ ...line, block: s > 0 && i === 0 })));

/** Line geometry (px), kept in step with performance-scene.css: 20px lines, a
 * 12px gap between blocks, and the panel's visible text height (from its top
 * padding to a gutter above the stage's foot). */
const LINE = 20;
const GAP = 12;
const VIEW = SCENE_SIZE.height - 80 - 16 - 24;
const TOPS = LINES.reduce<number[]>((tops, line, i) => [...tops, i === 0 ? 0 : tops[i - 1] + LINE + (line.block ? GAP : 0)], []);
/** The first line in view with `shown` lines out. The column scrolls by whole
 * lines, so the newest line stays in view and the top line is never cut. */
const firstFor = (shown: number) => {
  const need = shown ? TOPS[shown - 1] + LINE - VIEW : 0;
  return need <= 0 ? 0 : TOPS.findIndex((top) => top >= need);
};

/** How long each kind holds before the next line: commands run, words are
 * read. Brisk, so the result lands about 6.5s after the story opens. */
const HOLD: Record<Kind, number> = { cmd: 300, text: 340, key: 600 };
/** The scroll: the column's move (performance-scene.css), then a beat */
const SCROLL = 400;

/** `shown`: lines written. `first`: the first line in view (the scroll). */
type State = { shown: number; first: number };
const initial: State = { shown: 0, first: 0 };
const POSTER_LINES = LINES.findIndex((line) => line.kind === 'key') + 1;
const poster: State = { shown: POSTER_LINES, first: firstFor(POSTER_LINES) };
/** A cued tab opens at the story's start: the request, an empty terminal */
/** Reduced motion's still: the report written in full and scrolled as the
 * tab hands off on it, ending on the outcome (LCP down after the fix). */
const STILL: State = { shown: LINES.length, first: firstFor(LINES.length) };
const SETUP = { at: 'reset', state: initial };

/** Writes lines `from` to `to`. With `mark`, the last line is marked as it
 * appears and its hold is left to the caller. */
async function reveal(a: Act<State>, from: number, to: number, first: number, mark?: string) {
  for (let i = from; i < to; i++) {
    // Room first: the column moves up under the scrim; only then does the
    // new line fade in at its final place.
    const next = firstFor(i + 1);
    if (next !== first) {
      first = next;
      await a.set({ first });
      await a.wait(SCROLL);
    }
    await a.set({ shown: i + 1 });
    if (mark && i === to - 1) a.mark(mark);
    else await a.wait(HOLD[LINES[i].kind]);
  }
}

async function script(a: Act<State>) {
  a.mark('resume');
  await reveal(a, POSTER_LINES, LINES.length, poster.first, 'result');
  await a.wait(2200);

  a.mark('reset');
  await a.set({ shown: 0 });
  await a.wait(200);
  await a.snap(initial);
  await a.wait(100);
  await reveal(a, 0, POSTER_LINES, 0);
  await a.set(poster);
  a.mark('done');
}

export function PerformanceScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster, setup: SETUP });
  return (
    <SceneFrame
      kind="performance"
      onPointerEnter={onPointerEnter}

      caption="A chat input holds the request: Measure and improve perf plz. In a terminal below, the agent measures Web Vitals with agent-browser, profiles a reload, finds that the hero image blocks LCP, preloads it, and measures again: LCP drops from 2410ms to 980ms."
    >
      <Stage key={run} {...SCENE_SIZE} {...playback} poster={STILL} theme="light" className="look-vercel ab-performance">
        {s => <>
          <Field className="ab-perf-input" placeholder="Ask anything" value={REQUEST} />
          <div className="ab-perf-term">
            <div className="ab-perf-view" style={{ height: VIEW }}>
              <div className="ab-perf-lines" style={{ transform: `translateY(${-TOPS[s.first]}px)` }}>
                {LINES.map((line, i) => (
                  <div key={i} className="ab-perf-line" data-kind={line.kind} data-on={s.shown > i || undefined} style={{ top: TOPS[i] }}>
                    {line.text}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </>}
      </Stage>
    </SceneFrame>
  );
}
