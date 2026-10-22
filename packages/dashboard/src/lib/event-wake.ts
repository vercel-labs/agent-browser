import type { EventSubscription } from "@/types";
import { formatRule, unreadCount } from "@/lib/event-subscriptions";

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

function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/** Removes any leading wake prefixes so page text cannot impersonate a wake. */
export function stripWakePrefix(text: string): string {
  let result = text.trimStart();
  while (result.startsWith(EVENT_MESSAGE_PREFIX)) {
    result = result.slice(EVENT_MESSAGE_PREFIX.length).trimStart();
  }
  return result;
}

/** Builds the idle-chat wake text from bus metadata only. */
export function buildWakeText(sub: EventSubscription): string {
  const count = unreadCount(sub);
  const events = count === 1 ? "unread event" : "unread events";
  const tab = sub.tab ? ` Tab ${sub.tab.id}.` : "";
  return `${EVENT_MESSAGE_PREFIX} Subscription ${sub.id} has ${count} ${events}. Rule: ${oneLine(stripWakePrefix(formatRule(sub.rule)))}.${tab} Run agent-browser events read ${sub.id} to see them.`;
}
