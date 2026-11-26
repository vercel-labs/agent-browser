"use client";

import { useEffect, useRef } from "react";
import { useAtomValue } from "jotai/react";
import type { useChat } from "@ai-sdk/react";
import type { EventSubscription } from "@/types";
import { chatEnabledAtom } from "@/store/chat";
import { eventSubscriptionsAtom, unreadEventCount } from "@/store/stream";

type UseEventWakeOptions = Pick<ReturnType<typeof useChat>, "status" | "sendMessage" | "error"> & { session: string };

type ChatStatus = UseEventWakeOptions["status"];

type WakeState =
  | { phase: "idle" }
  | { phase: "dispatching"; key: string; subId: string }
  | { phase: "running"; key: string; subId: string };

interface WakeCursor {
  key: string;
  subId: string;
}

interface WakeStep {
  state: WakeState;
  retry: WakeCursor | null;
  scan: boolean;
}

const WAKE_TIMES_KEY = "agent-browser:event-wake-times";

export const EVENT_MESSAGE_PREFIX = "[agent-browser event]";
export const WAKE_MIN_INTERVAL_MS = 10_000;

export interface WakePayload {
  subscriptionId: string;
  seq: number;
  cursor: string;
}

export interface WakeMessageMetadata {
  wake: WakePayload & { count: number };
}

/** Advance the wake lifecycle for the current chat status, returning any cursor to retry. */
function advanceWake(state: WakeState, status: ChatStatus, error: Error | undefined): WakeStep {
  switch (state.phase) {
    case "idle":
      return { state, retry: null, scan: status === "ready" && error == null };
    case "dispatching":
      if (status === "submitted" || status === "streaming") {
        return { state: { phase: "running", key: state.key, subId: state.subId }, retry: null, scan: false };
      }
      if (status === "error" || error != null) {
        return { state: { phase: "idle" }, retry: { key: state.key, subId: state.subId }, scan: false };
      }
      return { state, retry: null, scan: status === "ready" };
    case "running":
      if (status === "error" || error != null) {
        return { state: { phase: "idle" }, retry: { key: state.key, subId: state.subId }, scan: false };
      }
      if (status === "ready") {
        return { state: { phase: "idle" }, retry: null, scan: true };
      }
      return { state, retry: null, scan: false };
  }
}

/** Remove the event prefix chat messages carry so the text renders on its own. */
export function stripWakePrefix(text: string): string {
  let result = text.trimStart();
  while (result.startsWith(EVENT_MESSAGE_PREFIX)) {
    result = result.slice(EVENT_MESSAGE_PREFIX.length).trimStart();
  }
  return result;
}

function buildWakeText(sub: EventSubscription): string {
  const count = unreadEventCount(sub);
  const events = count === 1 ? "unread event" : "unread events";
  const tab = sub.tab ? ` Tab ${sub.tab.id}.` : "";
  return `${EVENT_MESSAGE_PREFIX} Subscription ${sub.id} has ${count} ${events}.${tab} Run agent-browser events read ${sub.id} to see them.`;
}

function readWakeTimes(): Map<string, number> {
  try {
    const raw = sessionStorage.getItem(WAKE_TIMES_KEY);
    if (raw === null) return new Map();
    return new Map(Object.entries(JSON.parse(raw) as Record<string, number>));
  } catch {
    return new Map();
  }
}

function writeWakeTimes(times: Map<string, number>): void {
  try {
    sessionStorage.setItem(WAKE_TIMES_KEY, JSON.stringify(Object.fromEntries(times)));
  } catch {}
}

/** Wake the chat once per subscription when events arrive past its acknowledged cursor.
 *
 * `sendMessage` reports HTTP and stream failures through `status === "error"` and
 * `error` rather than rejecting, so a failed wake is only retried after the chat
 * recovers.
 */
export function useEventWake({ session, status, sendMessage, error }: UseEventWakeOptions) {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const chatEnabled = useAtomValue(chatEnabledAtom);
  const sentKeys = useRef(new Set<string>());
  const lastSentAt = useRef<Map<string, number> | null>(null);
  const wakeRef = useRef<WakeState>({ phase: "idle" });

  useEffect(() => {
    const times = (lastSentAt.current ??= readWakeTimes());

    const clearFailedWake = (target: WakeCursor) => {
      sentKeys.current.delete(target.key);
      times.delete(target.subId);
      writeWakeTimes(times);
      const current = wakeRef.current;
      if (
        (current.phase === "dispatching" || current.phase === "running") &&
        current.key === target.key &&
        current.subId === target.subId
      ) {
        wakeRef.current = { phase: "idle" };
      }
    };

    const step = advanceWake(wakeRef.current, status, error);
    wakeRef.current = step.state;
    if (step.retry) clearFailedWake(step.retry);
    if (!step.scan || !chatEnabled || !session) return;

    const now = Date.now();
    for (const sub of subscriptions) {
      const key = `${sub.id}:${sub.head}`;
      const last = times.get(sub.id);
      if (
        sub.owner !== `chat:${session}` ||
        unreadEventCount(sub) <= 0 ||
        sentKeys.current.has(key) ||
        (last != null && now - last < WAKE_MIN_INTERVAL_MS)
      ) continue;
      sentKeys.current.add(key);
      times.set(sub.id, now);
      writeWakeTimes(times);
      const wake: WakePayload = {
        subscriptionId: sub.id,
        seq: sub.head,
        cursor: key,
      };
      wakeRef.current = { phase: "dispatching", key, subId: sub.id };
      void sendMessage(
        {
          text: buildWakeText(sub),
          metadata: { wake: { ...wake, count: unreadEventCount(sub) } },
        },
        { body: { wake } },
      ).catch(() => clearFailedWake({ key, subId: sub.id }));
      return;
    }
  }, [subscriptions, status, error, chatEnabled, session, sendMessage]);
}
