'use client';

import { useMemo, useState, useSyncExternalStore, type PointerEventHandler } from 'react';
import type { Act, Play, Script } from '../../stageshow/vendor/kit';
import { useSceneSequence } from './scene-sequence';

const HOVER = '(hover: hover) and (pointer: fine)';
const canHover = () => window.matchMedia(HOVER).matches;
const serverHover = () => false;
const subscribeHover = (onChange: () => void) => {
  const query = window.matchMedia(HOVER);
  query.addEventListener('change', onChange);
  return () => query.removeEventListener('change', onChange);
};

/** Scripts begin and end at their poster, without a reset at playback start.
 * Play one cycle on entering view after earlier scenes finish. A deliberate
 * hover replays that cycle once the initial sequence has completed. */
export function useScenePlayback<S extends object>({
  script,
  poster,
}: {
  script: Script<S>;
  poster: S;
}): { play: Play; initial: S; script?: Script<S>; run: number; onPointerEnter: PointerEventHandler<HTMLElement> } {
  const hover = useSyncExternalStore(subscribeHover, canHover, serverHover);
  const { sequence, index } = useSceneSequence();
  const finish = sequence?.finish;
  const [replay, setReplay] = useState(0);
  const [completed, setCompleted] = useState(false);
  const cycle = useMemo<Script<S>>(
    () => async (a: Act<S>) => {
      await script(a);
      setCompleted(true);
      finish?.(index);
    },
    [script, finish, index],
  );
  const enabled = !sequence || sequence.active >= index;
  const onPointerEnter: PointerEventHandler<HTMLElement> = () => {
    if (!hover || !completed || (sequence && sequence.active < sequence.count) || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    setCompleted(false);
    setReplay(current => current + 1);
  };
  return { play: 'once', initial: poster, script: enabled ? cycle : undefined, run: replay, onPointerEnter };
}
