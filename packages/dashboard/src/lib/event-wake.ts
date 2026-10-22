import type { EventSubscription } from "@/types";
import { unreadCount } from "@/lib/event-subscriptions";

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

export function stripWakePrefix(text: string): string {
  let result = text.trimStart();
  while (result.startsWith(EVENT_MESSAGE_PREFIX)) {
    result = result.slice(EVENT_MESSAGE_PREFIX.length).trimStart();
  }
  return result;
}

export function buildWakeText(sub: EventSubscription): string {
  const count = unreadCount(sub);
  const events = count === 1 ? "unread event" : "unread events";
  const tab = sub.tab ? ` Tab ${sub.tab.id}.` : "";
  return `${EVENT_MESSAGE_PREFIX} Subscription ${sub.id} has ${count} ${events}.${tab} Run agent-browser events read ${sub.id} to see them.`;
}
