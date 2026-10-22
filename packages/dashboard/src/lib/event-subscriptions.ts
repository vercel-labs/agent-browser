import type { EventRule, EventSubscription } from "@/types";

const ACTIVE_STATUSES = new Set(["armed", "active", "watching", "waiting"]);

/** True while a subscription can still store events. */
export function isActiveStatus(status: string): boolean {
  return ACTIVE_STATUSES.has(status);
}

/** Number of stored events the owner has not acknowledged yet. */
export function unreadCount(sub: EventSubscription): number {
  return Math.max(0, sub.head - sub.consumed);
}

/** Renders a subscription rule as one line, whichever form the daemon sends. */
export function formatRule(rule: EventRule): string {
  if (typeof rule === "string") return rule;
  return Object.entries(rule)
    .map(([key, value]) => `${key}=${typeof value === "string" ? value : JSON.stringify(value)}`)
    .join(" ") || "unknown rule";
}

/** Parses `events list --json` stdout, returning null when it is not a list. */
export function parseEventsList(stdout: string): EventSubscription[] | null {
  try {
    const parsed = JSON.parse(stdout) as { data?: { subscriptions?: unknown } };
    const subscriptions = parsed.data?.subscriptions;
    return Array.isArray(subscriptions)
      ? (subscriptions as EventSubscription[])
      : null;
  } catch {
    return null;
  }
}
