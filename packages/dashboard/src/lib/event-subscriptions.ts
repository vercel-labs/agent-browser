import type { EventActivity, EventMessage, Subscription } from "@/types";

export const MAX_SUBSCRIPTIONS = 50;

function fromMessage(msg: EventMessage): Subscription {
  const state = msg.data.observation.state;
  const initialState =
    msg.name === "element.visible"
      ? "hidden"
      : msg.name === "element.hidden"
        ? "visible"
        : state;
  return {
    id: msg.subscriptionId,
    selector: msg.selector,
    tabId: msg.data.tabId,
    status: "watching",
    initialState,
    startedAt: msg.timestamp,
    lastEventAt: msg.timestamp,
    events: 0,
  };
}

function apply(sub: Subscription, msg: EventMessage): Subscription {
  const { observation } = msg.data;
  const next: Subscription = { ...sub, lastEventAt: msg.timestamp };
  if (observation.label) next.label = observation.label;

  switch (msg.name) {
    case "element.visible":
    case "element.hidden":
      next.status = "fired";
      next.events = sub.events + 1;
      if (next.firedAt == null) next.firedAt = msg.timestamp;
      break;
    case "subscription.ended": {
      const reason = observation.reason;
      next.reason = reason;
      const keepFired = sub.firedAt != null && reason === "matched";
      next.status = keepFired ? "fired" : "ended";
      break;
    }
    case "subscription.ready":
      break;
  }
  return next;
}

/**
 * Folds one event message into the subscription list (newest first, capped at 50).
 * A message for a subscription the dashboard never saw start is adopted as new.
 */
export function reduceSubscriptions(
  subs: Subscription[],
  msg: EventMessage,
): Subscription[] {
  const idx = subs.findIndex((s) => s.id === msg.subscriptionId);
  if (idx < 0) {
    const created = apply(fromMessage(msg), msg);
    return [created, ...subs].slice(0, MAX_SUBSCRIPTIONS);
  }
  return subs.map((s, i) => (i === idx ? apply(s, msg) : s));
}

/** Builds the compact activity feed row for an event message. */
export function toActivityEvent(msg: EventMessage): EventActivity {
  const id = msg.eventId
    ? msg.eventId
    : `${msg.subscriptionId}:${msg.name}:${msg.timestamp}`;
  return {
    type: "event",
    id,
    timestamp: msg.timestamp,
    subscriptionId: msg.subscriptionId,
    selector: msg.selector,
    name: msg.name,
    label: msg.data.observation.label,
    reason: msg.data.observation.reason,
  };
}
