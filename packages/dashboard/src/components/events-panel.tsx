"use client";

import { useEffect, useState } from "react";
import { useAtomValue } from "jotai/react";
import type { Subscription } from "@/types";
import { eventSubscriptionsAtom, watchingCountAtom } from "@/store/stream";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Check, Minus } from "lucide-react";

function formatDuration(ms: number, precise: boolean): string {
  const clamped = Math.max(0, ms);
  if (clamped < 60_000) {
    return precise
      ? `${(clamped / 1000).toFixed(1)}s`
      : `${Math.floor(clamped / 1000)}s`;
  }
  const minutes = Math.floor(clamped / 60_000);
  const seconds = Math.floor((clamped % 60_000) / 1000);
  return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
}

function formatReason(reason: string): string {
  return reason.replace(/_/g, " ");
}

function shortId(id: string): string {
  return id.replace(/^ev_/, "").slice(0, 8);
}

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

function StatusIndicator({ status }: { status: Subscription["status"] }) {
  if (status === "watching") {
    return (
      <span className="relative flex size-2 shrink-0">
        <span className="absolute inline-flex size-full animate-ping rounded-full bg-success opacity-60" />
        <span className="relative inline-flex size-2 rounded-full bg-success" />
      </span>
    );
  }
  if (status === "fired") {
    return <Check className="size-3 shrink-0 text-success" strokeWidth={3} />;
  }
  return <Minus className="size-3 shrink-0 text-muted-foreground" />;
}

function SubscriptionRow({
  sub,
  now,
}: {
  sub: Subscription;
  now: number;
}) {
  const fired = sub.status === "fired";
  const detail = [
    fired && sub.firedAt != null
      ? `${formatDuration(sub.firedAt - sub.startedAt, true)} after arming`
      : null,
    sub.events > 1 ? `${sub.events} events` : null,
    fired && sub.reason ? `Ended: ${formatReason(sub.reason)}` : null,
  ].filter(Boolean);

  return (
    <div
      className={cn(
        "mx-2 my-1.5 rounded-md border px-2.5 py-2 text-[11px]",
        fired
          ? "border-success/40 bg-success/5"
          : sub.status === "ended"
            ? "border-border/60 text-muted-foreground"
            : "border-border",
      )}
    >
      <div className="flex items-center gap-2">
        <StatusIndicator status={sub.status} />
        <span className="min-w-0 truncate rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">
          {sub.selector}
        </span>
        <span className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">
          {shortId(sub.id)}
        </span>
      </div>
      <div className="mt-1.5 pl-4">
        {sub.status === "watching" && (
          <p className="text-muted-foreground">
            Watching, armed {formatDuration(now - sub.startedAt, false)} ago
          </p>
        )}
        {fired && (
          <p className="flex min-w-0 items-baseline gap-1.5">
            <span className="shrink-0 font-medium text-success">
              Event received
            </span>
            {sub.label && (
              <span className="truncate text-foreground">{sub.label}</span>
            )}
          </p>
        )}
        {sub.status === "ended" && (
          <p>
            Ended: {sub.reason ? formatReason(sub.reason) : "unknown"}
            {sub.events > 0 && `, ${sub.events} received`}
          </p>
        )}
        {fired && detail.length > 0 && (
          <p className="mt-0.5 text-muted-foreground">
            {detail.join(" · ")}
          </p>
        )}
        {!fired && sub.label && (
          <p className="mt-0.5 truncate text-muted-foreground">
            {sub.label}
          </p>
        )}
      </div>
    </div>
  );
}

export function EventsPanel() {
  const subscriptions = useAtomValue(eventSubscriptionsAtom);
  const watching = useAtomValue(watchingCountAtom);
  const now = useNow(watching > 0);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <span className="text-xs text-muted-foreground">Events</span>
        <Badge variant="secondary" className="ml-auto h-4 px-1.5 text-[10px]">
          {subscriptions.length}
        </Badge>
      </div>
      <Separator />
      <div className="min-h-0 flex-1 overflow-y-auto">
        {subscriptions.length === 0 ? (
          <div className="px-6 py-8 text-center text-xs text-muted-foreground">
            No event subscriptions yet. Ask the agent to watch for something on
            the page.
          </div>
        ) : (
          subscriptions.map((sub) => (
            <SubscriptionRow key={sub.id} sub={sub} now={now} />
          ))
        )}
      </div>
    </div>
  );
}
