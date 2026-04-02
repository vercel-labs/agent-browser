"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import type { ConsoleEntry } from "@/types";
import { consoleLogsAtom, clearConsoleLogsAtom } from "@/store/stream";
import { activeSessionNameAtom } from "@/store/sessions";
import { execCommand, sessionArgs } from "@/lib/exec";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { CornerDownLeft, Loader2, Trash2 } from "lucide-react";

type FilterLevel = "all" | "errors" | "warnings" | "info" | "log";

const FILTER_MATCH: Record<FilterLevel, (e: ConsoleEntry) => boolean> = {
  all: () => true,
  errors: (e) => e.type === "page_error" || (e.type === "console" && e.level === "error"),
  warnings: (e) => e.type === "console" && (e.level === "warn" || e.level === "warning"),
  info: (e) => e.type === "console" && e.level === "info",
  log: (e) => e.type === "console" && (e.level === "log" || e.level === "debug"),
};

const LEVEL_COLORS: Record<string, string> = {
  error: "text-destructive",
  page_error: "text-destructive",
  warn: "text-warning",
  warning: "text-warning",
  info: "text-blue-400",
  log: "text-muted-foreground",
  debug: "text-muted-foreground/60",
};

function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function entryLevel(e: ConsoleEntry): string {
  return e.type === "page_error" ? "error" : e.level;
}

function entryText(e: ConsoleEntry): string {
  if (e.type === "page_error") {
    let text = e.text;
    if (e.line != null) {
      text += ` (${e.line}`;
      if (e.column != null) text += `:${e.column}`;
      text += ")";
    }
    return text;
  }
  return e.text;
}

interface EvalEntry {
  id: number;
  expression: string;
  result?: string;
  error?: string;
  pending: boolean;
  timestamp: number;
}

let evalIdCounter = 0;

export function ConsolePanel() {
  const entries = useAtomValue(consoleLogsAtom);
  const clearConsoleLogs = useSetAtom(clearConsoleLogsAtom);
  const sessionName = useAtomValue(activeSessionNameAtom);

  const [filter, setFilter] = useState<FilterLevel>("all");
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);
  const [evalInput, setEvalInput] = useState("");
  const [evalEntries, setEvalEntries] = useState<EvalEntry[]>([]);
  const [evaluating, setEvaluating] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const filtered = entries.filter(FILTER_MATCH[filter]);

  const errorCount = entries.filter(FILTER_MATCH.errors).length;
  const warnCount = entries.filter(FILTER_MATCH.warnings).length;

  useEffect(() => {
    if (autoScrollRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [filtered.length, evalEntries.length]);

  const handleScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    autoScrollRef.current = atBottom;
  };

  const handleEval = useCallback(async () => {
    const expr = evalInput.trim();
    if (!expr || !sessionName || evaluating) return;

    const id = ++evalIdCounter;
    const entry: EvalEntry = { id, expression: expr, pending: true, timestamp: Date.now() };
    setEvalEntries((prev) => [...prev, entry]);
    setEvalInput("");
    setEvaluating(true);

    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }

    try {
      const res = await execCommand(sessionArgs(sessionName, "eval", expr));
      setEvalEntries((prev) =>
        prev.map((e) =>
          e.id === id
            ? { ...e, pending: false, result: res.stdout.trim(), error: res.stderr.trim() || undefined }
            : e,
        ),
      );
    } catch {
      setEvalEntries((prev) =>
        prev.map((e) =>
          e.id === id ? { ...e, pending: false, error: "Failed to execute" } : e,
        ),
      );
    } finally {
      setEvaluating(false);
    }
  }, [evalInput, sessionName, evaluating]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleEval();
      }
    },
    [handleEval],
  );

  const filters: { key: FilterLevel; label: string; count?: number }[] = [
    { key: "all", label: "All" },
    { key: "errors", label: "Errors", count: errorCount },
    { key: "warnings", label: "Warnings", count: warnCount },
    { key: "info", label: "Info" },
    { key: "log", label: "Log" },
  ];

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-1.5 px-3 py-2">
        {filters.map((f) => (
          <button
            key={f.key}
            type="button"
            onClick={() => setFilter(f.key)}
            className={cn(
              "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] transition-colors",
              filter === f.key
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {f.label}
            {f.count != null && f.count > 0 && (
              <Badge
                variant="secondary"
                className={cn(
                  "h-3.5 min-w-4 px-1 text-[9px] tabular-nums",
                  f.key === "errors" && "bg-destructive/20 text-destructive",
                  f.key === "warnings" && "bg-warning/20 text-warning",
                )}
              >
                {f.count}
              </Badge>
            )}
          </button>
        ))}
        <button
          type="button"
          onClick={() => clearConsoleLogs()}
          className="ml-auto flex size-5 items-center justify-center rounded text-muted-foreground transition-colors hover:text-foreground"
          title="Clear console"
        >
          <Trash2 className="size-3" />
        </button>
      </div>
      <Separator />

      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="min-h-0 flex-1 overflow-y-auto font-mono"
      >
        {filtered.length === 0 && evalEntries.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            No console output
          </div>
        ) : (
          <>
            {filtered.map((entry, i) => {
              const level = entryLevel(entry);
              const color = LEVEL_COLORS[level] ?? "text-muted-foreground";
              return (
                <div
                  key={`c-${i}`}
                  className={cn(
                    "flex items-start gap-2 border-b border-border/50 px-3 py-1 text-[11px]",
                    level === "error" || entry.type === "page_error"
                      ? "bg-destructive/5"
                      : level === "warn" || level === "warning"
                        ? "bg-warning/5"
                        : "",
                  )}
                >
                  <span className="shrink-0 text-muted-foreground/60">
                    {formatTime(entry.timestamp)}
                  </span>
                  <span className={cn("shrink-0 w-10 uppercase", color)}>
                    {entry.type === "page_error" ? "error" : entry.level}
                  </span>
                  <span className={cn("min-w-0 flex-1 break-all whitespace-pre-wrap", color)}>
                    {entryText(entry)}
                  </span>
                </div>
              );
            })}
            {evalEntries.map((entry) => (
              <div key={`e-${entry.id}`} className="border-b border-border/50 text-[11px]">
                <div className="flex items-start gap-2 bg-muted/30 px-3 py-1">
                  <span className="shrink-0 text-muted-foreground/60">
                    {formatTime(entry.timestamp)}
                  </span>
                  <span className="shrink-0 w-10 text-violet-400">&gt;</span>
                  <span className="min-w-0 flex-1 whitespace-pre-wrap break-all text-violet-400">
                    {entry.expression}
                  </span>
                </div>
                {entry.pending ? (
                  <div className="flex items-center gap-2 px-3 py-1">
                    <Loader2 className="size-3 animate-spin text-muted-foreground" />
                  </div>
                ) : entry.error ? (
                  <div className="bg-destructive/5 px-3 py-1 pl-[76px]">
                    <span className="whitespace-pre-wrap break-all text-destructive">
                      {entry.error}
                    </span>
                  </div>
                ) : null}
                {entry.result && (
                  <div className="px-3 py-1 pl-[76px]">
                    <span className="whitespace-pre-wrap break-all text-emerald-400">
                      {entry.result}
                    </span>
                  </div>
                )}
              </div>
            ))}
          </>
        )}
        <div ref={bottomRef} />
      </div>

      <Separator />
      <div className="shrink-0 flex items-end gap-1.5 px-3 py-2">
        <textarea
          ref={textareaRef}
          value={evalInput}
          onChange={(e) => {
            setEvalInput(e.target.value);
            e.target.style.height = "auto";
            e.target.style.height = `${Math.min(e.target.scrollHeight, 120)}px`;
          }}
          onKeyDown={handleKeyDown}
          placeholder={sessionName ? "Evaluate JavaScript..." : "No active session"}
          disabled={!sessionName}
          rows={1}
          className={cn(
            "min-h-[28px] max-h-[120px] flex-1 resize-none rounded border border-border bg-background px-2 py-1.5 font-mono text-[11px] text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-ring",
            !sessionName && "opacity-50",
          )}
        />
        <button
          type="button"
          onClick={handleEval}
          disabled={!sessionName || !evalInput.trim() || evaluating}
          className="flex size-7 shrink-0 items-center justify-center rounded border border-border text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none"
          title="Run (Enter)"
        >
          {evaluating ? (
            <Loader2 className="size-3 animate-spin" />
          ) : (
            <CornerDownLeft className="size-3" />
          )}
        </button>
      </div>
    </div>
  );
}
