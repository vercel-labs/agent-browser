"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { clock, type Act, type Script } from "../../stageshow/vendor/kit";

export const HERO_DURATION = 17_000;
export const HERO_POSTER = 13_000;

export type HeroClock = { time: number };

export const HERO_INITIAL: HeroClock = { time: 0 };
export const HERO_STILL: HeroClock = { time: HERO_POSTER };

type ActivePlayback = {
  act: Act<HeroClock>;
  wake: () => void;
};

export function useHeroPlayback(reduced: boolean) {
  const [time, setTime] = useState(reduced ? HERO_POSTER : 0);
  const [playing, setPlaying] = useState(!reduced);
  const transport = useRef({
    time: reduced ? HERO_POSTER : 0,
    playing: !reduced,
    reduced,
    active: null as ActivePlayback | null,
  });

  // This script remains alive while paused. Only Stage's abort signal ends
  // a run, so neither pausing nor the loop seam can reset its scene state.
  const script: Script<HeroClock> = useCallback((act) => {
    return new Promise<void>((resolve) => {
      if (act.signal.aborted) {
        resolve();
        return;
      }

      const state = transport.current;
      let frame: number | null = null;
      let previous = clock.now();

      const tick = (now: number) => {
        frame = null;
        if (act.signal.aborted || !state.playing) return;

        state.time = (state.time + Math.max(0, now - previous)) % HERO_DURATION;
        previous = now;
        act.put({ time: state.time });
        frame = clock.raf(tick);
      };

      const active: ActivePlayback = {
        act,
        wake() {
          // Resume from the stored playhead, excluding paused/offscreen time.
          previous = clock.now();
          if (!state.playing) {
            if (frame !== null) clock.cancelRaf(frame);
            frame = null;
          } else if (frame === null && !act.signal.aborted) {
            frame = clock.raf(tick);
          }
        },
      };

      const stop = () => {
        if (frame !== null) clock.cancelRaf(frame);
        frame = null;
        act.signal.removeEventListener("abort", stop);
        if (state.active === active) state.active = null;
        resolve();
      };

      state.active = active;
      act.signal.addEventListener("abort", stop, { once: true });
      act.put({ time: state.time });
      active.wake();
    });
  }, []);

  const seek = useCallback((ms: number) => {
    const state = transport.current;
    state.time = Math.min(HERO_DURATION, Math.max(0, Number.isFinite(ms) ? ms : 0));
    state.playing = false;
    state.active?.wake();
    state.active?.act.put({ time: state.time });
    setTime(state.time);
    setPlaying(false);
  }, []);

  useEffect(() => {
    const state = transport.current;
    if (state.reduced === reduced) return;
    state.reduced = reduced;
    if (reduced) seek(HERO_POSTER);
  }, [reduced, seek]);

  return { script, time, playing };
}
