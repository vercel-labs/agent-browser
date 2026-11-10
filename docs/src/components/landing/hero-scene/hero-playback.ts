"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { clock, type Act, type Script } from "../../stageshow/vendor/kit";

export const HERO_DURATION = 17_000;
export const HERO_POSTER = 11_500;

/** `loop`: how many times the playhead has wrapped, so scenes can carry state
 * across loops (the desktop's wallpaper moves through a series). */
export type HeroClock = { time: number; loop: number };

export const HERO_INITIAL: HeroClock = { time: 0, loop: 0 };
export const HERO_STILL: HeroClock = { time: HERO_POSTER, loop: 0 };

type ActivePlayback = {
  act: Act<HeroClock>;
  wake: () => void;
};

export function useHeroPlayback(reduced: boolean) {
  const [time, setTime] = useState(reduced ? HERO_POSTER : 0);
  const [playing, setPlaying] = useState(!reduced);
  const transport = useRef({
    time: reduced ? HERO_POSTER : 0,
    loop: 0,
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

        const next = state.time + Math.max(0, now - previous);
        state.loop += Math.floor(next / HERO_DURATION);
        state.time = next % HERO_DURATION;
        previous = now;
        act.put({ time: state.time, loop: state.loop });
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
      act.put({ time: state.time, loop: state.loop });
      active.wake();
    });
  }, []);

  const seek = useCallback((ms: number) => {
    const state = transport.current;
    state.time = Math.min(HERO_DURATION, Math.max(0, Number.isFinite(ms) ? ms : 0));
    state.playing = false;
    state.active?.wake();
    state.active?.act.put({ time: state.time, loop: state.loop });
    setTime(state.time);
    setPlaying(false);
  }, []);

  // Development only: `?hero-t=<ms>` or `window.__heroSeek(ms)` pauses the
  // story on a beat, for reviewing frames. Stripped from production builds.
  useEffect(() => {
    if (process.env.NODE_ENV === "production") return;
    const at = new URLSearchParams(window.location.search).get("hero-t");
    if (at !== null) seek(Number(at));
    (window as unknown as { __heroSeek?: (ms: number) => void }).__heroSeek = seek;
  }, [seek]);

  useEffect(() => {
    const state = transport.current;
    if (state.reduced === reduced) return;
    state.reduced = reduced;
    if (reduced) seek(HERO_POSTER);
  }, [reduced, seek]);

  return { script, time, playing };
}
