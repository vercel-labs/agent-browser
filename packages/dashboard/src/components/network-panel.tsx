"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useAtomValue } from "jotai/react";
import { activeSessionNameAtom } from "@/store/sessions";
import { execCommand, sessionArgs } from "@/lib/exec";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Circle, Loader2, RefreshCw, Square, Trash2 } from "lucide-react";

interface NetworkRequest {
  url: string;
  method: string;
  status?: number;
  resourceType: string;
  requestId: string;
  mimeType?: string;
  timestamp: number;
}

type TypeFilter = "all" | "xhr" | "doc" | "css" | "js" | "img" | "font" | "other";

const TYPE_FILTERS: { key: TypeFilter; label: string; cliType?: string }[] = [
  { key: "all", label: "All" },
  { key: "xhr", label: "XHR", cliType: "xhr,fetch" },
  { key: "doc", label: "Doc", cliType: "document" },
  { key: "css", label: "CSS", cliType: "stylesheet" },
  { key: "js", label: "JS", cliType: "script" },
  { key: "img", label: "Img", cliType: "image" },
  { key: "font", label: "Font", cliType: "font" },
  { key: "other", label: "Other", cliType: "other,websocket,media,manifest,texttrack,eventsource,signedexchange,ping,cspviolationreport,preflight" },
];

const STATUS_COLOR: Record<string, string> = {
  "2": "text-emerald-500",
  "3": "text-blue-400",
  "4": "text-warning",
  "5": "text-destructive",
};

function statusColor(status?: number): string {
  if (status == null) return "text-muted-foreground";
  const prefix = String(status)[0];
  return STATUS_COLOR[prefix] ?? "text-muted-foreground";
}

function truncateUrl(url: string, max: number): string {
  try {
    const u = new URL(url);
    const path = u.pathname + u.search;
    return path.length > max ? path.slice(0, max) + "..." : path;
  } catch {
    return url.length > max ? url.slice(0, max) + "..." : url;
  }
}

function urlHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return "";
  }
}

export function NetworkPanel() {
  const sessionName = useAtomValue(activeSessionNameAtom);

  const [requests, setRequests] = useState<NetworkRequest[]>([]);
  const [loading, setLoading] = useState(false);
  const [typeFilter, setTypeFilter] = useState<TypeFilter>("all");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [detail, setDetail] = useState<Record<string, unknown> | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [harRecording, setHarRecording] = useState(false);
  const [harDialogOpen, setHarDialogOpen] = useState(false);
  const [harPath, setHarPath] = useState("capture.har");
  const harInputRef = useRef<HTMLInputElement>(null);
  const lastSessionRef = useRef(sessionName);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const doFetch = useCallback(async (showSpinner: boolean) => {
    if (!sessionName) return;
    if (showSpinner) setLoading(true);
    try {
      const args = sessionArgs(sessionName, "network", "requests");
      if (typeFilter !== "all") {
        const filter = TYPE_FILTERS.find((f) => f.key === typeFilter);
        if (filter?.cliType) {
          args.push("--type", filter.cliType);
        }
      }
      const res = await execCommand(args);
      if (res.success && res.stdout) {
        try {
          const parsed = JSON.parse(res.stdout);
          const data = parsed.data ?? parsed;
          setRequests(data.requests ?? []);
        } catch {
          setRequests([]);
        }
      }
    } finally {
      if (showSpinner) setLoading(false);
    }
  }, [sessionName, typeFilter]);

  const fetchRequests = useCallback(() => doFetch(true), [doFetch]);

  useEffect(() => {
    if (sessionName && sessionName !== lastSessionRef.current) {
      lastSessionRef.current = sessionName;
      setRequests([]);
      setExpanded(null);
      setDetail(null);
      setHarRecording(false);
    }
    doFetch(true);
  }, [sessionName, typeFilter, doFetch]);

  useEffect(() => {
    if (!sessionName) return;
    pollRef.current = setInterval(() => {
      if (document.visibilityState === "visible") doFetch(false);
    }, 5000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [sessionName, doFetch]);

  useEffect(() => {
    if (harDialogOpen) {
      requestAnimationFrame(() => harInputRef.current?.select());
    }
  }, [harDialogOpen]);

  const handleClear = useCallback(async () => {
    if (!sessionName) return;
    await execCommand(sessionArgs(sessionName, "network", "requests", "--clear"));
    setRequests([]);
    setExpanded(null);
    setDetail(null);
  }, [sessionName]);

  const handleExpand = useCallback(async (requestId: string) => {
    if (expanded === requestId) {
      setExpanded(null);
      setDetail(null);
      return;
    }
    setExpanded(requestId);
    setDetail(null);
    setDetailLoading(true);
    try {
      const res = await execCommand(sessionArgs(sessionName, "network", "request", requestId));
      if (res.success && res.stdout) {
        try {
          const parsed = JSON.parse(res.stdout);
          setDetail(parsed.data ?? parsed);
        } catch {
          setDetail(null);
        }
      }
    } finally {
      setDetailLoading(false);
    }
  }, [expanded, sessionName]);

  const handleHarStart = useCallback(async () => {
    if (!sessionName) return;
    await execCommand(sessionArgs(sessionName, "network", "har", "start"));
    setHarRecording(true);
  }, [sessionName]);

  const handleHarStop = useCallback(async () => {
    if (!sessionName) return;
    const path = harPath.trim() || "capture.har";
    setHarDialogOpen(false);
    await execCommand(sessionArgs(sessionName, "network", "har", "stop", path));
    setHarRecording(false);
  }, [sessionName, harPath]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-1.5 px-3 py-2">
        {TYPE_FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            onClick={() => {
              setTypeFilter(f.key);
              setExpanded(null);
              setDetail(null);
            }}
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] transition-colors",
              typeFilter === f.key
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {f.label}
          </button>
        ))}

        <div className="ml-auto flex items-center gap-1">
          <button
            type="button"
            onClick={harRecording ? () => setHarDialogOpen(true) : handleHarStart}
            disabled={!sessionName}
            className={cn(
              "flex size-5 items-center justify-center rounded transition-colors disabled:opacity-40",
              harRecording
                ? "text-destructive hover:bg-destructive/10"
                : "text-muted-foreground hover:text-foreground",
            )}
            title={harRecording ? "Stop HAR recording" : "Start HAR recording"}
          >
            {harRecording ? (
              <Square className="size-2.5 fill-current" />
            ) : (
              <Circle className="size-3" />
            )}
          </button>
          {harRecording && (
            <Badge variant="secondary" className="h-3.5 px-1 text-[9px] text-destructive">
              HAR
            </Badge>
          )}

          <button
            type="button"
            onClick={handleClear}
            disabled={!sessionName}
            className="flex size-5 items-center justify-center rounded text-muted-foreground transition-colors hover:text-foreground disabled:opacity-40"
            title="Clear requests"
          >
            <Trash2 className="size-3" />
          </button>
          <button
            type="button"
            onClick={fetchRequests}
            disabled={loading || !sessionName}
            className="flex size-5 items-center justify-center rounded text-muted-foreground transition-colors hover:text-foreground disabled:opacity-40"
            title="Refresh"
          >
            {loading ? (
              <Loader2 className="size-3 animate-spin" />
            ) : (
              <RefreshCw className="size-3" />
            )}
          </button>
        </div>
      </div>
      <Separator />

      <div className="min-h-0 flex-1 overflow-y-auto font-mono">
        {!sessionName ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            No active session
          </div>
        ) : requests.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            {loading ? (
              <Loader2 className="mx-auto size-4 animate-spin text-muted-foreground" />
            ) : (
              "No requests captured"
            )}
          </div>
        ) : (
          requests.map((r) => {
            const isExpanded = expanded === r.requestId;
            return (
              <div key={r.requestId} className="border-b border-border/50">
                <button
                  type="button"
                  onClick={() => handleExpand(r.requestId)}
                  className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[11px] hover:bg-muted/50"
                >
                  <span className={cn("w-7 shrink-0 text-right tabular-nums", statusColor(r.status))}>
                    {r.status ?? "..."}
                  </span>
                  <span className="w-8 shrink-0 text-muted-foreground">{r.method}</span>
                  <span className="min-w-0 flex-1 truncate text-foreground" title={r.url}>
                    {truncateUrl(r.url, 80)}
                  </span>
                  <span className="shrink-0 text-[10px] text-muted-foreground/60">
                    {r.resourceType}
                  </span>
                </button>
                {isExpanded && (
                  <div className="space-y-1.5 bg-muted/30 px-3 py-2 text-[10px]">
                    {detailLoading ? (
                      <div className="flex items-center gap-2 py-2">
                        <Loader2 className="size-3 animate-spin text-muted-foreground" />
                      </div>
                    ) : detail ? (
                      <RequestDetail detail={detail} url={r.url} />
                    ) : (
                      <div className="text-muted-foreground">
                        URL: {r.url}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>

      <Dialog open={harDialogOpen} onOpenChange={setHarDialogOpen}>
        <DialogContent className="max-w-xs">
          <DialogHeader>
            <DialogTitle>Save HAR</DialogTitle>
          </DialogHeader>
          <input
            ref={harInputRef}
            type="text"
            value={harPath}
            onChange={(e) => setHarPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                handleHarStop();
              }
            }}
            placeholder="capture.har"
            className="h-9 w-full rounded-md border border-input bg-transparent px-3 font-mono text-sm outline-none focus:ring-1 focus:ring-ring"
          />
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setHarDialogOpen(false)}>
              Cancel
            </Button>
            <Button size="sm" onClick={handleHarStop} disabled={!harPath.trim()}>
              Save
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function RequestDetail({ detail, url }: { detail: Record<string, unknown>; url: string }) {
  const host = urlHost(url);
  const headers = detail.headers as Record<string, string> | undefined;
  const responseHeaders = detail.responseHeaders as Record<string, string> | undefined;
  const body = detail.body as string | undefined;
  const postData = detail.postData as string | undefined;

  return (
    <>
      <DetailRow label="URL" value={url} wrap />
      {host && <DetailRow label="Host" value={host} />}
      {detail.method && <DetailRow label="Method" value={String(detail.method)} />}
      {detail.status != null && <DetailRow label="Status" value={String(detail.status)} />}
      {detail.mimeType && <DetailRow label="Type" value={String(detail.mimeType)} />}

      {headers && Object.keys(headers).length > 0 && (
        <HeadersSection title="Request Headers" headers={headers} />
      )}
      {postData && (
        <div className="mt-1">
          <span className="text-muted-foreground">Request Body</span>
          <pre className="mt-0.5 max-h-32 overflow-auto whitespace-pre-wrap break-all text-foreground">
            {formatBody(postData)}
          </pre>
        </div>
      )}
      {responseHeaders && Object.keys(responseHeaders).length > 0 && (
        <HeadersSection title="Response Headers" headers={responseHeaders} />
      )}
      {body && (
        <div className="mt-1">
          <span className="text-muted-foreground">Response Body</span>
          <pre className="mt-0.5 max-h-48 overflow-auto whitespace-pre-wrap break-all text-foreground">
            {formatBody(body)}
          </pre>
        </div>
      )}
    </>
  );
}

function HeadersSection({ title, headers }: { title: string; headers: Record<string, string> }) {
  return (
    <div className="mt-1">
      <span className="text-muted-foreground">{title}</span>
      <div className="mt-0.5 space-y-px">
        {Object.entries(headers).map(([k, v]) => (
          <div key={k} className="flex gap-2">
            <span className="shrink-0 text-muted-foreground">{k}:</span>
            <span className="min-w-0 flex-1 break-all text-foreground">{String(v)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function DetailRow({ label, value, wrap }: { label: string; value: string; wrap?: boolean }) {
  return (
    <div className="flex gap-2">
      <span className="w-12 shrink-0 text-muted-foreground">{label}</span>
      <span className={cn("min-w-0 flex-1 text-foreground", wrap ? "break-all whitespace-pre-wrap" : "truncate")}>
        {value}
      </span>
    </div>
  );
}

function formatBody(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}
