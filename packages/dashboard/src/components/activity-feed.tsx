"use client";

import { useEffect, useRef, useState } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import type { ActivityEvent } from "@/types";
import { combinedEventsAtom, persistActivityAtom, togglePersistAtom, clearActivityAtom } from "@/store/activity";
import { Bookmark, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { JsonSyntax } from "@/components/json-syntax";

function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function highlightRefs(text: string): React.ReactNode {
  const parts = text.split(/(@e\d+)/g);
  return parts.map((part, i) =>
    part.match(/^@e\d+$/) ? (
      <span key={i} className="font-mono font-semibold text-accent-foreground">
        {part}
      </span>
    ) : (
      part
    ),
  );
}

function CommandEntry({
  event,
}: {
  event: ActivityEvent & { type: "command" };
}) {
  const [expanded, setExpanded] = useState(false);

  const label = event.action;
  const hasParams =
    event.params &&
    Object.keys(event.params).filter((k) => k !== "action" && k !== "id")
      .length > 0;

  return (
    <Collapsible open={expanded} onOpenChange={setExpanded}>
      <div className="py-1.5 px-3">
        <CollapsibleTrigger className="flex w-full items-center gap-2 text-left text-xs">
          <span className="shrink-0 font-mono text-muted-foreground">
            {formatTime(event.timestamp)}
          </span>
          <span className="truncate font-mono font-semibold">
            {highlightRefs(label)}
          </span>
          {hasParams && (
            <span className="ml-auto shrink-0 text-muted-foreground">
              {expanded ? "-" : "+"}
            </span>
          )}
        </CollapsibleTrigger>
        <CollapsibleContent>
          {hasParams && (
            <pre className="mt-1 max-h-32 overflow-x-auto overflow-y-auto text-[10px]">
              <JsonSyntax
                value={Object.fromEntries(
                  Object.entries(event.params).filter(
                    ([k]) => k !== "action" && k !== "id",
                  ),
                )}
              />
            </pre>
          )}
        </CollapsibleContent>
      </div>
      <Separator />
    </Collapsible>
  );
}

function ResultEntry({
  event,
}: {
  event: ActivityEvent & { type: "result" };
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <Collapsible open={expanded} onOpenChange={setExpanded}>
      <div className="py-1.5 px-3">
        <CollapsibleTrigger className="flex w-full items-center gap-2 text-left text-xs">
          <span className="shrink-0 font-mono text-muted-foreground">
            {formatTime(event.timestamp)}
          </span>
          <span className="truncate font-mono">
            {event.action}
            <span className="ml-1 text-muted-foreground">
              {event.duration_ms}ms
            </span>
          </span>
          <span className="ml-auto shrink-0 text-muted-foreground">
            {expanded ? "-" : "+"}
          </span>
        </CollapsibleTrigger>
        <CollapsibleContent>
          {event.data != null && (
            <pre className="mt-1 max-h-48 overflow-x-auto overflow-y-auto text-[10px]">
              {typeof event.data === "string"
                ? event.data
                : <JsonSyntax value={event.data} />}
            </pre>
          )}
        </CollapsibleContent>
      </div>
      <Separator />
    </Collapsible>
  );
}

const LEVEL_STYLES: Record<string, string> = {
  error: "text-destructive",
  warn: "text-warning",
  warning: "text-warning",
  info: "text-accent-foreground",
  log: "text-muted-foreground",
};

function ConsoleEntry({
  event,
}: {
  event: ActivityEvent & { type: "console" };
}) {
  return (
    <>
      <div className="flex items-start gap-2 py-1.5 px-3 text-xs">
        <span className="shrink-0 font-mono text-muted-foreground">
          {formatTime(event.timestamp)}
        </span>
        <Badge
          variant="outline"
          className={cn(
            "h-4 px-1 text-[10px]",
            LEVEL_STYLES[event.level] ?? "text-muted-foreground",
          )}
        >
          {event.level}
        </Badge>
        <span className="truncate font-mono">{event.text}</span>
      </div>
      <Separator />
    </>
  );
}

export function ActivityFeed() {
  const events = useAtomValue(combinedEventsAtom);
  const persist = useAtomValue(persistActivityAtom);
  const togglePersist = useSetAtom(togglePersistAtom);
  const clearActivity = useSetAtom(clearActivityAtom);

  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);

  useEffect(() => {
    if (autoScrollRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [events.length]);

  const handleScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    autoScrollRef.current = atBottom;
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <span className="text-xs text-muted-foreground">Activity</span>
        <Badge variant="secondary" className="ml-auto h-4 px-1.5 text-[10px]">
          {events.length}
        </Badge>
        <button
          type="button"
          onClick={() => togglePersist()}
          className={cn(
            "flex h-5 items-center gap-1 rounded border px-1.5 text-[10px] transition-colors",
            persist
              ? "border-accent-foreground/30 bg-accent text-accent-foreground"
              : "border-border text-muted-foreground hover:text-foreground",
          )}
          title={persist ? "Activity is persisted across reloads. Click to disable." : "Persist activity across reloads"}
        >
          <Bookmark className="size-3" />
          {persist ? "On" : "Off"}
        </button>
        <button
          type="button"
          onClick={() => clearActivity()}
          className="flex h-5 items-center gap-1 rounded border border-border px-1.5 text-[10px] text-muted-foreground transition-colors hover:text-foreground"
          title="Clear activity"
        >
          <Trash2 className="size-3" />
        </button>
      </div>
      <Separator />

      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="min-h-0 flex-1 overflow-y-auto"
      >
        {events.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            Waiting for events...
          </div>
        ) : (
          events.map((event, i) => {
            const key = event.type === "console"
              ? `console-${event.timestamp}-${i}`
              : `${event.type}-${event.id}`;
            switch (event.type) {
              case "command":
                return <CommandEntry key={key} event={event} />;
              case "result":
                return <ResultEntry key={key} event={event} />;
              case "console":
                return <ConsoleEntry key={key} event={event} />;
            }
          })
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
