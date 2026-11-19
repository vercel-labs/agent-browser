'use client';

import { useMemo, useState, useSyncExternalStore, type PointerEventHandler } from 'react';
import type { Act, Play, Script } from '../../stageshow/vendor/kit';
import { Stopped } from '../../stageshow/vendor/kit/act';
import { useSceneSequence, type ScenePlan } from './scene-sequence';

const HOVER = '(hover: hover) and (pointer: fine)';
const canHover = () => window.matchMedia(HOVER).matches;
const serverHover = () => false;
const subscribeHover = (onChange: () => void) => {
  const query = window.matchMedia(HOVER);
  query.addEventListener('change', onChange);
  return () => query.removeEventListener('change', onChange);
};

/** Act members that only read the stage; they work while skipping and parked. */
const READS = new Set<PropertyKey>(['rect', 'caret', 'center', 'signal', 'layout']);
/** Act members that return nothing; every other method returns a promise. */
const SYNC = new Set<PropertyKey>(['put', 'place']);
/** A script that never reaches its cue without time passing (a clock-driven
 * loop) would spin forever while skipped; past this many calls, play it live. */
const RUNAWAY = 20_000;
class Runaway extends Error {}
/** Where a scene can open instead of its poster when its host cues it: the
 * state the scene is in at mark `at` (a mark at or after its reset), with
 * nothing on screen changing between that state and the mark. */
export interface SceneSetup<S> {
  at: string;
  state: S;
}

/**
 * Plays `script` the way the host's plan asks (see ScenePlan). The act is
 * wrapped: before the cue every method that changes the stage resolves at once
 * without effect (reads still work, so the script's own measurements hold);
 * once the handoff has fired, the stop mark parks every later call until the
 * stage aborts the run, which ends it the way the Stage expects (Stopped).
 */
export async function directed<S>(
  a: Act<S>,
  script: Script<S>,
  plan: ScenePlan,
  report: { mark: (name: string, skipped: boolean) => void; handoff: () => void },
): Promise<void> {
  const stop = plan.stop ?? 'reset';
  let skipping = Boolean(plan.cue);
  let handed = false;
  let parked = false;
  let calls = 0;
  let hold: Promise<never> | undefined;
  const park = () => (hold ??= new Promise<never>((_, reject) => {
    const end = () => reject(new Stopped());
    if (a.signal.aborted) end();
    else a.signal.addEventListener('abort', end, { once: true });
  }));

  const act = new Proxy(a, {
    get(target, key) {
      const value: unknown = Reflect.get(target, key);
      if (key === 'mark') {
        return (name: string) => {
          if (parked) return;
          if (skipping && name === plan.cue) skipping = false;
          if (!skipping && handed && name === stop) {
            parked = true;
            report.mark(`${name} (parked)`, false);
            return;
          }
          target.mark(name);
          report.mark(name, skipping);
          if (!skipping && !handed && name === plan.handoff) {
            handed = true;
            report.handoff();
          }
        };
      }
      if (typeof value !== 'function') return value;
      if (READS.has(key)) return value.bind(target);
      return (...args: unknown[]) => {
        if (skipping) {
          if (++calls > RUNAWAY) throw new Runaway();
          return SYNC.has(key) ? undefined : Promise.resolve();
        }
        if (parked) return SYNC.has(key) ? undefined : park();
        return value.apply(target, args);
      };
    },
  });

  if (plan.lead) await a.wait(plan.lead);
  let runaway = false;
  try {
    await script(act);
  } catch (error) {
    if (!(error instanceof Runaway)) throw error;
    runaway = true;
  }
  if (skipping || runaway) {
    // The cue was never reached: play the scene from its start instead
    skipping = false;
    await script(act);
  } else if (plan.cue && !handed) {
    // The payoff sits before the cue (the poster is the setup's end): one
    // more pass from the poster reaches it, then parks at the stop
    await script(act);
  }
}

/** Scripts begin and end at their poster, without a reset at playback start.
 * Play one cycle on entering view after earlier scenes finish. A deliberate
 * hover replays that cycle once the initial sequence has completed. A host
 * with a plan (ControlledSceneSequence) can enter the cycle at a cue and is
 * told when its payoff lands; without one, playback is unchanged. */
export function useScenePlayback<S extends object>({
  script,
  poster,
  setup,
}: {
  script: Script<S>;
  poster: S;
  /** Optional: the state at a mark after the reset, for hosts that cue the
   * scene (see ScenePlan). Must be a stable (module-level) object. */
  setup?: SceneSetup<S>;
}): { play: Play; initial: S; script?: Script<S>; run: number; onPointerEnter: PointerEventHandler<HTMLElement> } {
  const hover = useSyncExternalStore(subscribeHover, canHover, serverHover);
  const { sequence, index } = useSceneSequence();
  const finish = sequence?.finish;
  const start = sequence?.start;
  const handoff = sequence?.handoff;
  const mark = sequence?.mark;
  const ready = sequence?.ready;
  // A cued scene opens on its setup when it has one (its mark becomes the
  // cue); otherwise on its poster, played as the host's `withoutSetup` says
  const hostPlan = sequence?.plan;
  const opensOnSetup = Boolean(hostPlan?.cue && setup);
  const plan = useMemo<ScenePlan | undefined>(() => {
    if (!hostPlan?.cue) return hostPlan;
    const { withoutSetup, ...rest } = hostPlan;
    if (setup) return { ...rest, cue: setup.at };
    if (!withoutSetup) return rest;
    return { ...rest, cue: withoutSetup.cue, lead: withoutSetup.lead };
  }, [hostPlan, setup]);
  const [replay, setReplay] = useState(0);
  const [completed, setCompleted] = useState(false);
  const cycle = useMemo<Script<S>>(
    () => async (a: Act<S>) => {
      // Rest on the opening frame until the host wants playback (the scene
      // well in view); an abort meanwhile ends the run as the Stage expects
      if (ready) await ready(a.signal);
      if (a.signal.aborted) throw new Stopped();
      start?.(index);
      if (plan) {
        await directed(a, script, plan, {
          mark: (name, skipped) => mark?.(index, name, skipped),
          handoff: () => handoff?.(index),
        });
      } else {
        await script(a);
      }
      setCompleted(true);
      finish?.(index);
    },
    [script, finish, start, plan, handoff, mark, ready, index],
  );
  const enabled = !sequence || sequence.active >= index;
  const onPointerEnter: PointerEventHandler<HTMLElement> = () => {
    if (!hover || !completed || (sequence && sequence.active < sequence.count) || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    setCompleted(false);
    setReplay(current => current + 1);
  };
  return { play: 'once', initial: opensOnSetup && setup ? setup.state : poster, script: enabled ? cycle : undefined, run: replay, onPointerEnter };
}
