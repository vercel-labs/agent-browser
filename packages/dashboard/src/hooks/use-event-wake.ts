"use client";

import { useEffect, useRef } from "react";
import { useAtomValue } from "jotai/react";
import type { useChat } from "@ai-sdk/react";
import { chatEnabledAtom } from "@/store/chat";
import { eventSubscriptionsAtom } from "@/store/stream";
import { buildWakeText, WAKE_MIN_INTERVAL_MS, type WakePayload } from "@/lib/event-wake";
import { unreadCount } from "@/lib/event-subscriptions";

type UseEventWakeOptions = Pick<ReturnType<typeof useChat>, "status" | "sendMessage"> & { session: string };

/**
 * Wakens the agent when an owned subscription has unread events and the chat is
 * idle, at most once per subscription every ten seconds.
 */
export function useEventWake({ session, status, sendMessage }: UseEventWakeOptions) {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const chatEnabled = useAtomValue(chatEnabledAtom);
  const sentKeys = useRef(new Set<string>());
  const lastSentAt = useRef(new Map<string, number>());

  useEffect(() => {
    if (!chatEnabled || !session || status !== "ready") return;
    const now = Date.now();
    for (const sub of subscriptions) {
      const key = `${sub.id}:${sub.head}`;
      const last = lastSentAt.current.get(sub.id);
      if (
        sub.owner !== `chat:${session}` ||
        unreadCount(sub) <= 0 ||
        sentKeys.current.has(key) ||
        (last != null && now - last < WAKE_MIN_INTERVAL_MS)
      ) continue;
      sentKeys.current.add(key);
      lastSentAt.current.set(sub.id, now);
      const wake: WakePayload = {
        subscriptionId: sub.id,
        seq: sub.head,
        cursor: key,
      };
      sendMessage(
        {
          text: buildWakeText(sub),
          metadata: { wake: { ...wake, count: unreadCount(sub) } },
        },
        { body: { wake } },
      );
      return;
    }
  }, [subscriptions, status, chatEnabled, session, sendMessage]);
}
