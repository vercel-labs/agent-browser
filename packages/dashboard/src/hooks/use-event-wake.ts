"use client";

import { useEffect, useRef } from "react";
import { useAtomValue } from "jotai/react";
import { chatEnabledAtom } from "@/store/chat";
import { eventSubscriptionsAtom } from "@/store/stream";
import {
  buildWakeText,
  shouldWake,
  type WakeMessageMetadata,
  type WakePayload,
} from "@/lib/event-wake";
import { unreadCount } from "@/lib/event-subscriptions";

interface UseEventWakeOptions {
  session: string;
  status: string;
  sendMessage: (
    message: { text: string; metadata?: WakeMessageMetadata },
    options: { body: { wake: WakePayload } },
  ) => unknown;
}

/**
 * Wakens the agent when an owned subscription has unread events and the chat is
 * idle, at most once per subscription every ten seconds.
 */
export function useEventWake({
  session,
  status,
  sendMessage,
}: UseEventWakeOptions) {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const chatEnabled = useAtomValue(chatEnabledAtom);
  const sentKeys = useRef(new Set<string>());
  const lastSentAt = useRef(new Map<string, number>());

  useEffect(() => {
    if (!chatEnabled || !session) return;
    const now = Date.now();
    for (const sub of subscriptions) {
      const key = `${sub.id}:${sub.head}`;
      const ok = shouldWake({
        sub,
        session,
        chatReady: status === "ready",
        chatEnabled,
        sent: sentKeys.current.has(key),
        lastSentAt: lastSentAt.current.get(sub.id),
        now,
      });
      if (!ok) continue;
      sentKeys.current.add(key);
      lastSentAt.current.set(sub.id, now);
      const wake: WakePayload = {
        subscriptionId: sub.id,
        seq: sub.head,
        cursor: `${sub.id}:${sub.head}`,
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
