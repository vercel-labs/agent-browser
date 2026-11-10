'use client';

import { createContext, useCallback, useContext, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';

/**
 * How a host wants the active scene played, by mark name. Every scene script
 * starts and ends on its poster, so it can be run as a loop and entered at any
 * of its marks.
 *
 * - `cue`: where the host's playback begins. Beats before it are skipped
 *   without painting (the Stage still shows the poster), so a scene whose
 *   poster sits on its payoff can open on its setup instead. Use only a mark
 *   after which the script resets its own state, usually `reset`. Omitted, or
 *   never reached: from the script's start.
 * - `handoff`: the mark where the payoff has landed. Reported once per
 *   showing through `onHandoff`; the script loops at most once more to reach
 *   it after the cue. Never reached: the host falls back to `onFinish`.
 * - `stop`: once the handoff has fired, the scene pauses at this mark (default
 *   `reset`) and waits there for the host to move on, so it is never caught
 *   mid-reset.
 * - `lead`: ms the opening frame rests before the script starts, so the
 *   viewer sees where the scene begins before anything moves.
 *
 * The Stage always paints its opening state before a script can run, so a
 * cue alone cannot hide a poster that differs from the setup. A scene opens
 * on its setup by passing `setup` to useScenePlayback: the state it is in at a
 * named mark (that mark becomes the cue). Until a scene has one, the host's
 * `withoutSetup` says how to play it from its poster instead (its own cue and
 * lead; omitted, the plan's cue is taken as clean from the poster).
 */
export interface ScenePlan {
  cue?: string;
  handoff: string;
  stop?: string;
  lead?: number;
  withoutSetup?: { cue?: string; lead?: number };
}

interface Sequence {
  active: number;
  count: number;
  finish: (index: number) => void;
  /** Optional: told when a scene's cycle begins (again after re-entering view). */
  start?: (index: number) => void;
  /** Optional: how the active scene is played; see ScenePlan. */
  plan?: ScenePlan;
  /** Optional: told when the active scene reaches its plan's handoff mark. */
  handoff?: (index: number) => void;
  /** Optional: told of every mark the scene passes; `skipped` during the cue. */
  mark?: (index: number, name: string, skipped: boolean) => void;
  /** Optional: resolves once the host wants playback to begin (the scene well
   * in view); the Stage holds its opening frame until then. */
  ready?: (signal: AbortSignal) => Promise<void>;
}

const SequenceContext = createContext<Sequence | null>(null);
const IndexContext = createContext(-1);

/** Stage handles visibility; the sequence only grants each scene its turn. */
export function SceneSequence({ count, className, children }: {
  count: number; className: string; children: ReactNode;
}) {
  const [active, setActive] = useState(0);
  const finish = useCallback((index: number) => {
    setActive(current => current === index ? current + 1 : current);
  }, []);
  const value = useMemo(() => ({ active, count, finish }), [active, count, finish]);
  return <SequenceContext.Provider value={value}><ul className={className}>{children}</ul></SequenceContext.Provider>;
}

/** A sequence whose turn the host sets, told when each scene starts and finishes.
 * Callbacks are held in refs so a new handler never restarts a playing scene. */
export function ControlledSceneSequence({ active, count, plan, ready, onStart, onFinish, onHandoff, onMark, children }: {
  active: number;
  count: number;
  plan?: ScenePlan;
  /** Stable (held as given): resolves when playback may begin. */
  ready?: (signal: AbortSignal) => Promise<void>;
  onStart?: (index: number) => void;
  onFinish: (index: number) => void;
  onHandoff?: (index: number) => void;
  onMark?: (index: number, name: string, skipped: boolean) => void;
  children: ReactNode;
}) {
  const handlers = useRef({ onStart, onFinish, onHandoff, onMark });
  useLayoutEffect(() => {
    handlers.current = { onStart, onFinish, onHandoff, onMark };
  }, [onStart, onFinish, onHandoff, onMark]);
  const start = useCallback((index: number) => handlers.current.onStart?.(index), []);
  const finish = useCallback((index: number) => handlers.current.onFinish(index), []);
  const handoff = useCallback((index: number) => handlers.current.onHandoff?.(index), []);
  const mark = useCallback((index: number, name: string, skipped: boolean) => handlers.current.onMark?.(index, name, skipped), []);
  // Keyed by its fields, so a host passing a fresh object each render never
  // restarts the scene
  const { cue, handoff: handoffMark, stop, lead, withoutSetup } = plan ?? {};
  const hasFallback = Boolean(withoutSetup);
  const fallbackCue = withoutSetup?.cue;
  const fallbackLead = withoutSetup?.lead;
  const stablePlan = useMemo<ScenePlan | undefined>(
    () => (handoffMark
      ? { cue, handoff: handoffMark, stop, lead, withoutSetup: hasFallback ? { cue: fallbackCue, lead: fallbackLead } : undefined }
      : undefined),
    [cue, handoffMark, stop, lead, hasFallback, fallbackCue, fallbackLead],
  );
  const value = useMemo(
    () => ({ active, count, finish, start, plan: stablePlan, handoff, mark, ready }),
    [active, count, finish, start, stablePlan, handoff, mark, ready],
  );
  return <SequenceContext.Provider value={value}>{children}</SequenceContext.Provider>;
}

export function SceneSequenceItem({ index, children }: { index: number; children: ReactNode }) {
  return <IndexContext.Provider value={index}>{children}</IndexContext.Provider>;
}

export function useSceneSequence() {
  const sequence = useContext(SequenceContext);
  const index = useContext(IndexContext);
  return { sequence, index };
}
