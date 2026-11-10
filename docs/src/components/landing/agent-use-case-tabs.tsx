'use client';

// The homepage use cases as feature tabs: pills and a description on the left,
// the selected use case's scene on the right.

import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { Section, SectionHeading } from './compat';
import { UseCaseVisual, type UseCaseVisualKind } from './use-case-visual';
import { ControlledSceneSequence, SceneSequenceItem, type ScenePlan } from './use-case-scenes/scene-sequence';

export interface AgentUseCasesProps {
  heading: string;
  items: { title: string; visual: UseCaseVisualKind; description: string }[];
}

/** Keeps "agent-desktop" on one line: browsers otherwise break it at the hyphen. */
function noBreak(text: string): ReactNode {
  return text.split(/(agent-desktop)/).map((part, index) =>
    index % 2 ? <span key={index} className="whitespace-nowrap">{part}</span> : part,
  );
}

/** One key phrase per description, marked **like this**, set in ink at 450. */
function withHighlight(description: string): ReactNode {
  const [before, phrase, after] = description.split('**');
  if (phrase === undefined) return noBreak(description);
  return <>{noBreak(before)}<strong className="font-[450] text-gray-1000">{noBreak(phrase)}</strong>{noBreak(after)}</>;
}

/** Short pill labels naming what each tab lets you do. The full titles from the
 * content stay available as each panel's (visually hidden) heading. */
const LABEL: Record<UseCaseVisualKind, string> = {
  drive: 'Control input',
  snapshots: 'Reduce tokens',
  verify: 'Capture screen',
  performance: 'Measure performance',
  network: 'Mock network',
  sessions: 'Multiple agents',
};

/**
 * How each tab plays its scene and hands off to the next. Every tab opens on
 * the start of its scene's story (the setup), never on the poster or
 * mid-action, and hands off right after the payoff:
 *
 * - `cue`: the setup's mark. A scene opens there by passing `setup` (its state
 *   at that mark) to useScenePlayback; until it does, `withoutSetup` says how
 *   to play it from its poster. See ScenePlan.
 * - `lead`: ms the opening frame rests before anything moves, on top of any
 *   beat the scene itself waits after its setup mark. Every tab's first motion
 *   lands 250 to 400ms after it opens.
 * - `handoff` + `hold`: the payoff's mark, then a beat for it to register.
 *   After it the scene parks before its `reset`, so it is never cut mid-reset.
 *
 * A tab moves on at max(reading time, handoff + hold). A handoff that never
 * fires falls back to the scene's end plus HOLD_MS.
 */
const HANDOFF: Record<UseCaseVisualKind, ScenePlan & { hold: number }> = {
  // Setup: Name filled, Email and Team empty (`setup` at `email`, the reset
  // already done). Fill it in, hand off on Saved; `saved` fires 420ms after
  // Saved rolls in and the hand drifts off in the hold. Without a setup: the
  // poster rests, then the scene's reset fades the values out.
  drive: { cue: 'reset', handoff: 'saved', hold: 700, lead: 250, withoutSetup: { cue: 'reset', lead: 500 } },
  // Opens on the full count (`setup` at `pass1`, skipping the scene's own
  // opening rest): just long enough to register 48,210 before the outline
  // enters. Payoff: one paragraph and its count
  snapshots: { cue: 'pass1', handoff: 'payoff', hold: 900, lead: 250 },
  // Setup: the bare page (`setup` at `select`); after a short beat the hand
  // moves in, drags the first selection, captures both. Hand off once the second copy has landed
  // (`result` is 300ms after); the hold covers the hand's 1.4s drift plus a
  // beat at rest, and the scene parks at its reset. Without a setup: the
  // poster (first selection drawn) rests, then the captures
  verify: { cue: 'reset', handoff: 'result', hold: 1700, lead: 220, withoutSetup: { lead: 600 } },
  // Setup: the empty terminal (`setup` at `reset`). Payoff: the full result
  performance: { cue: 'reset', handoff: 'result', hold: 1000, withoutSetup: {} },
  // Setup: the panes before the pull (`setup` at `reset`, time 0). Payoff:
  // everyone home, the layout settled
  sessions: { cue: 'reset', handoff: 'payoff', hold: 700, withoutSetup: {} },
  // Setup: the empty terminal, Add to cart (`setup` at `command-0`; the scene
  // adds its own 100ms beat, so a shorter lead). Payoff: Sold out; `done` is ~1.6s after it lands, so a short hold. Without a setup it
  // plays its whole cycle from the poster (abort, unroute, clear, cart, stock)
  network: { cue: 'reset', handoff: 'done', hold: 300, lead: 180, withoutSetup: {} },
};

/** Fallback rest on a scene's final frame when its handoff mark never fires. */
const HOLD_MS = 700;

/** How much of the scene must be in view before its tab starts playing. */
const READY_RATIO = 0.6;

/** Reading time for a description: a base beat plus ~260 words per minute
 * (marketing copy is skimmed, and the pills already name the tab), clamped so
 * short copy still gets a moment and long copy never stalls. With the current
 * copy (16 to 19 words) it lands at 4.7 to 5.4s: a floor under the shortest
 * scenes that rarely leaves a payoff waiting. */
const READ_BASE_MS = 1000;
const READ_WPM = 260;
const READ_MIN_MS = 3000;
const READ_MAX_MS = 9000;
function readingTime(description: string): number {
  const words = description.replace(/\*\*/g, '').trim().split(/\s+/).filter(Boolean).length;
  const ms = READ_BASE_MS + (words / READ_WPM) * 60_000;
  return Math.round(Math.min(READ_MAX_MS, Math.max(READ_MIN_MS, ms)));
}

/** Every pill keeps a 1px ring (box-shadow, so it takes no space and nothing
 * moves between states). Inactive: transparent, gray-400 ring, gray text; hover
 * steps the ring to gray-500 and the text to ink. Active: Geist's secondary
 * Button (white fill, gray-400 ring, ink text) with no hover change; clicking
 * it restarts its scene. Every pill keeps the pointer cursor, so it never
 * flips as the selection moves. Keyboard focus swaps the ring for Geist's
 * focus ring. */
const PILL = 'h-9 cursor-pointer rounded-full px-3.5 text-[14px] leading-5 font-medium whitespace-nowrap outline-none transition-[color,background-color,box-shadow] duration-150 ease-[ease] focus-visible:shadow-[var(--ds-focus-ring)] motion-reduce:transition-none';
const PILL_ON = 'bg-background-100 text-gray-1000 shadow-[0_0_0_1px_var(--ds-gray-400)]';
const PILL_OFF = 'bg-transparent text-gray-900 shadow-[0_0_0_1px_var(--ds-gray-400)] hover:text-gray-1000 hover:shadow-[0_0_0_1px_var(--ds-gray-500)]';

/** Phone and tablet: pills, description, scene, so a tapped pill's text and
 * scene land directly below it. Desktop: text column left, scene right; the
 * pills top-align with the scene and the description slot follows them with a
 * normal gap, every description top-aligned in it. */
const LAYOUT = [
  'mt-10 grid grid-cols-1 @lg:mt-20',
  "[grid-template-areas:'pills'_'desc'_'visual']",
  '@lg:grid-cols-[minmax(0,1fr)_minmax(0,460px)] @lg:grid-rows-[auto_1fr] @lg:gap-x-20',
  "@lg:[grid-template-areas:'pills_visual'_'desc_visual']",
].join(' ');

const reducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;

export function AgentUseCaseTabs({ heading, items }: AgentUseCasesProps) {
  const id = useId();
  const [selected, setSelected] = useState(0);
  // Bumped on every manual choice (click or key), including a click on the
  // open pill, so the chosen scene remounts and its plan starts over (cue,
  // reading time, handoff); the effect below clears the previous run's timers
  const [run, setRun] = useState(0);
  const selectedRef = useRef(selected);
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const hovering = useRef(false);
  // Keyboard focus on a pill holds the tab too, so the selection never moves
  // out from under the focused pill
  const focused = useRef(false);
  const waiting = useRef(false);
  const onScreen = useRef(false);
  const itemsRef = useRef(items);
  useEffect(() => {
    itemsRef.current = items;
  }, [items]);

  // Dwell: a tab moves on once BOTH its scene has handed off (its HANDOFF mark
  // plus that hold, or its end plus HOLD_MS) and its description's reading
  // time, counted from when it opened, has passed
  const readTimer = useRef<number | undefined>(undefined);
  const holdTimer = useRef<number | undefined>(undefined);
  /** The scene's script has ended (a scene that ends is never replayed). */
  const finished = useRef(false);
  /** The scene has handed off; `holdMs` is how long its payoff rests. */
  const handed = useRef(false);
  const holdMs = useRef(HOLD_MS);
  const readDone = useRef(false);
  const holdDone = useRef(false);
  const visual = useRef<HTMLDivElement>(null);

  const clearTimers = useCallback(() => {
    window.clearTimeout(readTimer.current);
    window.clearTimeout(holdTimer.current);
    waiting.current = false;
  }, []);

  const next = useCallback(() => {
    waiting.current = false;
    setSelected(current => (current + 1) % items.length);
  }, [items.length]);

  const maybeAdvance = useCallback(() => {
    if (!readDone.current || !holdDone.current || reducedMotion()) return;
    // Pointer over the illustration or focus on a pill: hold the tab until it leaves
    if (hovering.current || focused.current) waiting.current = true;
    else next();
  }, [next]);

  const startRead = useCallback(() => {
    window.clearTimeout(readTimer.current);
    readDone.current = false;
    readTimer.current = window.setTimeout(() => {
      readDone.current = true;
      maybeAdvance();
    }, readingTime(itemsRef.current[selectedRef.current].description));
  }, [maybeAdvance]);

  const startHold = useCallback(() => {
    window.clearTimeout(holdTimer.current);
    holdDone.current = false;
    holdTimer.current = window.setTimeout(() => {
      holdDone.current = true;
      maybeAdvance();
    }, holdMs.current);
  }, [maybeAdvance]);

  const select = useCallback((index: number) => {
    setSelected(index);
    setRun(current => current + 1);
  }, []);

  // A tab opening (by click, key or auto-advance) starts its reading clock
  useEffect(() => {
    selectedRef.current = selected;
    finished.current = false;
    handed.current = false;
    holdDone.current = false;
    visual.current?.removeAttribute('data-handoff');
    clearTimers();
    if (onScreen.current) startRead();
    return clearTimers;
  }, [selected, run, startRead, clearTimers]);

  const handOff = useCallback((ms: number) => {
    handed.current = true;
    holdMs.current = ms;
    if (onScreen.current) startHold();
  }, [startHold]);

  // The payoff has landed: rest on it for the scene's hold. The scene parks
  // before its reset, so a longer reading time never shows a reset.
  const onHandoff = useCallback((index: number) => {
    if (index !== selectedRef.current || handed.current) return;
    visual.current?.setAttribute('data-handoff', '');
    handOff(HANDOFF[itemsRef.current[index].visual].hold);
  }, [handOff]);

  // The fallback: a scene that ends without its handoff mark rests HOLD_MS
  const onFinish = useCallback((index: number) => {
    if (index !== selectedRef.current) return;
    finished.current = true;
    if (!handed.current) handOff(HOLD_MS);
  }, [handOff]);

  // The Stage restarts a scene on its own when its layout or the motion
  // preference changes: its handoff is due again, from the new run
  const onStart = useCallback((index: number) => {
    if (index !== selectedRef.current || !handed.current || finished.current) return;
    handed.current = false;
    holdDone.current = false;
    waiting.current = false;
    window.clearTimeout(holdTimer.current);
    visual.current?.removeAttribute('data-handoff');
  }, []);

  // Every mark the scene passes, on the slot, for rehearsal logs
  const onMark = useCallback((index: number, name: string, skipped: boolean) => {
    if (index === selectedRef.current) visual.current?.setAttribute('data-scene-mark', skipped ? `skip:${name}` : name);
  }, []);

  // Playback (the scene's script and the reading clock) begins only once the
  // scene is well in view, so the first tab is not spent while the section is
  // still scrolling in; the Stage holds its opening frame until then. It
  // stops only once the scene is wholly out of view or the page is hidden,
  // so a partial scroll never restarts anything.
  const readyWaiters = useRef(new Set<() => void>());
  const waitReady = useCallback((signal: AbortSignal) => new Promise<void>(resolve => {
    if (onScreen.current || signal.aborted) return resolve();
    const done = () => {
      readyWaiters.current.delete(done);
      signal.removeEventListener('abort', done);
      resolve();
    };
    readyWaiters.current.add(done);
    signal.addEventListener('abort', done, { once: true });
  }), []);

  // Out of view, drop every pending advance so a tab never moves on unseen. On
  // return the reading clock starts over; an unfinished scene (still playing,
  // or parked on its payoff) replays, since the Stage reset it, and hands off
  // again; a finished one only needs its hold.
  useEffect(() => {
    const el = visual.current;
    if (!el) return;
    let ratio = 0;
    const update = () => {
      const visible = !document.hidden && (onScreen.current ? ratio > 0 : ratio >= READY_RATIO);
      if (visible === onScreen.current) return;
      onScreen.current = visible;
      clearTimers();
      readDone.current = false;
      holdDone.current = false;
      if (!visible) {
        if (!finished.current) {
          handed.current = false;
          el.removeAttribute('data-handoff');
        }
        return;
      }
      startRead();
      if (handed.current) startHold();
      readyWaiters.current.forEach(go => go());
    };
    const io = new IntersectionObserver(([entry]) => {
      ratio = entry.isIntersecting ? entry.intersectionRatio : 0;
      update();
    }, { threshold: [0, READY_RATIO] });
    io.observe(el);
    document.addEventListener('visibilitychange', update);
    return () => {
      io.disconnect();
      document.removeEventListener('visibilitychange', update);
    };
  }, [startRead, startHold, clearTimers]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const last = items.length - 1;
    const moves: Record<string, number> = {
      ArrowRight: selected === last ? 0 : selected + 1,
      ArrowLeft: selected === 0 ? last : selected - 1,
      Home: 0,
      End: last,
    };
    const to = moves[event.key];
    if (to === undefined) return;
    event.preventDefault();
    select(to);
    tabs.current[to]?.focus();
  };

  const item = items[selected];
  return (
    <Section top={120} bottom={120} gap={false} data-element="AgentUseCaseTabs">
      <div className="mx-auto w-full max-w-[1080px]">
        <SectionHeading className="max-w-[28ch]">{heading}</SectionHeading>

        <div className={LAYOUT}>
          {/* The pills' left edge lines up with the heading and description */}
          <div
            role="tablist"
            aria-orientation="horizontal"
            aria-label={heading}
            onKeyDown={onKeyDown}
            onFocus={event => { focused.current = event.target.matches(':focus-visible'); }}
            onBlur={event => {
              if (event.currentTarget.contains(event.relatedTarget)) return;
              focused.current = false;
              if (waiting.current && !hovering.current) next();
            }}
            className="flex flex-wrap gap-2 [grid-area:pills]"
          >
            {items.map((entry, index) => {
              const on = index === selected;
              return (
                <button
                  key={entry.title}
                  ref={el => { tabs.current[index] = el; }}
                  type="button"
                  role="tab"
                  id={`${id}-tab-${index}`}
                  aria-selected={on}
                  aria-controls={`${id}-panel`}
                  tabIndex={on ? 0 : -1}
                  onClick={() => select(index)}
                  className={`${PILL} ${on ? PILL_ON : PILL_OFF}`}
                >
                  {LABEL[entry.visual]}
                </button>
              );
            })}
          </div>

          {/* One fixed slot: all six stacked in a single cell, so it always holds the
              tallest; only the active one is visible, with a very fast crossfade */}
          <div
            role="tabpanel"
            id={`${id}-panel`}
            aria-labelledby={`${id}-tab-${selected}`}
            tabIndex={0}
            className="mt-10 grid max-w-[40ch] rounded-[2px] outline-none [grid-area:desc] focus-visible:ring-2 focus-visible:ring-gray-1000 focus-visible:ring-offset-4 focus-visible:ring-offset-background-200 @lg:mt-12 @lg:self-start"
          >
            {items.map((entry, index) => {
              const on = index === selected;
              return (
                <div
                  key={entry.title}
                  aria-hidden={!on}
                  data-read-ms={readingTime(entry.description)}
                  className={`[grid-area:1/1] transition-[opacity,visibility] duration-[130ms] ease-out motion-reduce:transition-none ${on ? 'visible opacity-100' : 'invisible opacity-0'}`}
                >
                  <h3 className="sr-only">{entry.title}</h3>
                  <p className="text-[18px] leading-7 text-gray-900 text-pretty @lg:text-[20px] @lg:leading-[30px]">
                    {withHighlight(entry.description)}
                  </p>
                </div>
              );
            })}
          </div>

          <div
            ref={visual}
            className="mt-10 w-full max-w-[560px] [grid-area:visual] @lg:mt-0 @lg:max-w-none @lg:self-start"
            onPointerEnter={() => { hovering.current = true; }}
            onPointerLeave={() => {
              hovering.current = false;
              if (waiting.current && !focused.current) next();
            }}
          >
            {/* The kit's Stage mounts with its fit box capped at the 400px design
                width, measures that, and only fills the box a frame later: on
                every switch the new scene painted one frame small, then jumped.
                Lifting the cap here lets its first measurement see the full box,
                so the new scene's first frame is already at its final size. */}
            <div className="overflow-hidden rounded-[2px] border border-gray-alpha-400 bg-background-100 [&_.ss-fit]:max-w-none!">
              {/* Only the selected scene is mounted; a new key replays it from its start */}
              <ControlledSceneSequence
                active={selected}
                count={items.length}
                plan={HANDOFF[item.visual]}
                ready={waitReady}
                onStart={onStart}
                onFinish={onFinish}
                onHandoff={onHandoff}
                onMark={onMark}
              >
                <SceneSequenceItem index={selected}>
                  <UseCaseVisual key={`${selected}-${run}`} kind={item.visual} />
                </SceneSequenceItem>
              </ControlledSceneSequence>
            </div>
          </div>
        </div>
      </div>
    </Section>
  );
}
