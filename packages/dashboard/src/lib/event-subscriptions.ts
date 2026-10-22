import type { EventRule, EventSubscription } from "@/types";

const ACTIVE_STATUSES = new Set(["armed", "active", "watching", "waiting"]);

export function isActiveStatus(status: string): boolean {
  return ACTIVE_STATUSES.has(status);
}

export function unreadCount(sub: EventSubscription): number {
  return Math.max(0, sub.head - sub.consumed);
}

export function formatRule(rule: EventRule): string {
  if (typeof rule === "string") return rule;
  return Object.entries(rule)
    .map(([key, value]) => `${key}=${typeof value === "string" ? value : JSON.stringify(value)}`)
    .join(" ") || "unknown rule";
}

export function parseEventsList(stdout: string): EventSubscription[] | null {
  try {
    const parsed = JSON.parse(stdout) as { data?: { subscriptions?: unknown } };
    const subscriptions = parsed.data?.subscriptions;
    return Array.isArray(subscriptions) ? subscriptions as EventSubscription[] : null;
  } catch {
    return null;
  }
}
