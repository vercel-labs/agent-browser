"use client";

import { useEffect, useRef } from "react";
import { useAtomValue } from "jotai/react";
import type { useChat } from "@ai-sdk/react";
import { chatEnabledAtom } from "@/store/chat";
import { eventSubscriptionsAtom } from "@/store/stream";
import { buildWakeText, WAKE_MIN_INTERVAL_MS, type WakePayload } from "@/lib/event-wake";
import { unreadCount } from "@/lib/event-subscriptions";

type UseEventWakeOptions = Pick<ReturnType<typeof useChat>, "status" | "sendMessage"> & { session: string };

const WAKE_TIMES_KEY = "agent-browser:event-wake-times";

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

export function useEventWake({ session, status, sendMessage }: UseEventWakeOptions) {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const chatEnabled = useAtomValue(chatEnabledAtom);
  const sentKeys = useRef(new Set<string>());
  const lastSentAt = useRef<Map<string, number> | null>(null);

  useEffect(() => {
    if (!chatEnabled || !session || status !== "ready") return;
    const times = (lastSentAt.current ??= readWakeTimes());
    const now = Date.now();
    for (const sub of subscriptions) {
      const key = `${sub.id}:${sub.head}`;
      const last = times.get(sub.id);
      if (
        sub.owner !== `chat:${session}` ||
        unreadCount(sub) <= 0 ||
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
      void sendMessage(
        {
          text: buildWakeText(sub),
          metadata: { wake: { ...wake, count: unreadCount(sub) } },
        },
        { body: { wake } },
      ).catch(() => {
        sentKeys.current.delete(key);
        times.delete(sub.id);
        writeWakeTimes(times);
      });
      return;
    }
  }, [subscriptions, status, chatEnabled, session, sendMessage]);
}
