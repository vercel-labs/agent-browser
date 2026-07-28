"use client";

import { atom } from "jotai";
import type { ExecResult } from "@/lib/exec";

/** Transient error shown when a dashboard action (new tab, back, ...) fails. */
export const actionErrorAtom = atom<string | null>(null);

let dismissTimer: ReturnType<typeof setTimeout> | null = null;

export const reportActionErrorAtom = atom(null, (_get, set, message: string) => {
  set(actionErrorAtom, message);
  if (dismissTimer) clearTimeout(dismissTimer);
  dismissTimer = setTimeout(() => set(actionErrorAtom, null), 8000);
});

export function execErrorText(result: ExecResult): string {
  if (result.stderr) return result.stderr;
  if (result.stdout) {
    try {
      const json = JSON.parse(result.stdout);
      if (json.error) return json.error;
    } catch {
      // stdout wasn't JSON
    }
  }
  // Server-side failures (bad request, exec timeout) come back as an HTTP
  // error body with a top-level "error" and no stdout/stderr.
  if (result.error) return result.error;
  return "";
}
