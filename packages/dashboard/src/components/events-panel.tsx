"use client";

import { useAtomValue } from "jotai/react";
import type { EventSubscription } from "@/types";
import { eventSubscriptionsAtom } from "@/store/stream";
import { formatRule, isActiveStatus, unreadCount } from "@/lib/event-subscriptions";
import { stripWakePrefix } from "@/lib/event-wake";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Check, Minus } from "lucide-react";

function StatusIndicator({ active, status }: { active: boolean; status: string }) {
  if (active) {
    return (
      <span className="relative flex size-2 shrink-0">
        <span className="absolute inline-flex size-full animate-ping rounded-full bg-success opacity-60" />
        <span className="relative inline-flex size-2 rounded-full bg-success" />
      </span>
    );
  }
  return status === "matched"
    ? <Check className="size-3 shrink-0 text-success" strokeWidth={3} />
    : <Minus className="size-3 shrink-0 text-muted-foreground" />;
}

function SubscriptionRow({ sub }: { sub: EventSubscription }) {
  const active = isActiveStatus(sub.status);
  const unread = unreadCount(sub);

  return (
    <div
      className={cn(
        "mx-2 my-1.5 rounded-md border px-2.5 py-2 text-[11px]",
        active
          ? "border-border"
          : unread > 0
            ? "border-success/40 bg-success/5"
            : "border-border/60 text-muted-foreground",
      )}
    >
      <div className="flex items-center gap-2">
        <StatusIndicator active={active} status={sub.status} />
        <span className="min-w-0 truncate rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">
          {stripWakePrefix(formatRule(sub.rule))}
        </span>
        <span className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">{sub.id.replace(/^ev_/, "").slice(0, 8)}</span>
      </div>
      <div className="mt-1.5 space-y-0.5 pl-4">
        <p className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
          <span className={active ? "font-medium text-success" : "font-medium"}>
            {sub.status.replaceAll("_", " ")}
          </span>
          {sub.reason && <span>{sub.reason.replaceAll("_", " ")}</span>}
          {sub.tab && (
            <span className="font-mono text-muted-foreground">tab {sub.tab.id}</span>
          )}
        </p>
        <p className="text-muted-foreground">
          head {sub.head} · consumed {sub.consumed} ·{" "}
          {unread > 0 ? `${unread} unread` : "no unread"}
          {sub.dropped > 0 && ` · ${sub.dropped} dropped`}
        </p>
      </div>
    </div>
  );
}

export function EventsPanel() {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <span className="text-xs text-muted-foreground">Events</span>
        <Badge variant="secondary" className="ml-auto h-4 px-1.5 text-[10px]">{subscriptions.length}</Badge>
      </div>
      <Separator />
      <div className="min-h-0 flex-1 overflow-y-auto">
        {subscriptions.length === 0 ? (
          <div className="px-6 py-8 text-center text-xs text-muted-foreground">
            No event subscriptions yet. Ask the agent to watch for something on
            the page.
          </div>
        ) : (
          subscriptions.map((sub) => <SubscriptionRow key={sub.id} sub={sub} />)
        )}
      </div>
    </div>
  );
}
