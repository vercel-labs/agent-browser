"use client";

import { useEffect, useRef } from "react";
import { useAtomValue } from "jotai/react";
import { chatEnabledAtom } from "@/store/chat";
import { eventSubscriptionsAtom } from "@/store/stream";
import { buildWakeText, shouldWake, type WakeContext } from "@/lib/event-wake";

interface UseEventWakeOptions {
  messages: WakeContext["messages"];
  status: string;
  sendMessage: (message: { text: string }) => unknown;
}

/**
 * Sends the agent a new message when a subscription started in this panel finishes,
 * once per subscription, and only while the chat is idle.
 */
export function useEventWake({ messages, status, sendMessage }: UseEventWakeOptions) {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const chatEnabled = useAtomValue(chatEnabledAtom);
  const preexisting = useRef<Set<string> | null>(null);
  if (preexisting.current === null) {
    preexisting.current = new Set(subscriptions.map((s) => s.id));
  }
  const tracked = useRef(new Set<string>());
  const woken = useRef(new Set<string>());

  useEffect(() => {
    for (const sub of subscriptions) {
      if (sub.status === "watching" && !preexisting.current!.has(sub.id)) {
        tracked.current.add(sub.id);
      }
    }
    for (const sub of subscriptions) {
      const ok = shouldWake({
        sub,
        tracked: tracked.current.has(sub.id),
        woken: woken.current.has(sub.id),
        chatReady: status === "ready",
        chatEnabled,
        messages,
      });
      if (!ok) continue;
      woken.current.add(sub.id);
      sendMessage({ text: buildWakeText(sub) });
      return;
    }
  }, [subscriptions, status, chatEnabled, messages, sendMessage]);
}
