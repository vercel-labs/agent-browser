"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useAtomValue } from "jotai/react";
import { activeSessionNameAtom } from "@/store/sessions";
import { execCommand, sessionArgs } from "@/lib/exec";
import { cn } from "@/lib/utils";
import { Separator } from "@/components/ui/separator";
import { Loader2, RefreshCw } from "lucide-react";

type StorageTab = "cookies" | "localStorage" | "sessionStorage";

interface CookieEntry {
  name: string;
  value: string;
  domain?: string;
  path?: string;
  expires?: number;
  httpOnly?: boolean;
  secure?: boolean;
  sameSite?: string;
}

interface StorageEntry {
  key: string;
  value: string;
}

const TABS: { key: StorageTab; label: string }[] = [
  { key: "cookies", label: "Cookies" },
  { key: "localStorage", label: "Local" },
  { key: "sessionStorage", label: "Session" },
];

function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max) + "..." : s;
}

function formatExpiry(expires: number | undefined): string {
  if (expires == null || expires <= 0) return "Session";
  const d = new Date(expires * 1000);
  return d.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

export function StoragePanel() {
  const sessionName = useAtomValue(activeSessionNameAtom);

  const [tab, setTab] = useState<StorageTab>("cookies");
  const [cookies, setCookies] = useState<CookieEntry[]>([]);
  const [local, setLocal] = useState<StorageEntry[]>([]);
  const [session, setSession] = useState<StorageEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);
  const lastSessionRef = useRef(sessionName);

  const fetchData = useCallback(
    async (which: StorageTab) => {
      if (!sessionName) return;
      setLoading(true);
      try {
        if (which === "cookies") {
          const res = await execCommand(sessionArgs(sessionName, "cookies"));
          if (res.success && res.stdout) {
            try {
              const parsed = JSON.parse(res.stdout);
              setCookies(parsed.cookies ?? []);
            } catch {
              setCookies([]);
            }
          }
        } else {
          const storageType = which === "localStorage" ? "local" : "session";
          const res = await execCommand(
            sessionArgs(sessionName, "storage", storageType),
          );
          if (res.success && res.stdout) {
            try {
              const parsed = JSON.parse(res.stdout);
              const entries: StorageEntry[] = [];
              if (parsed.entries && typeof parsed.entries === "object") {
                for (const [k, v] of Object.entries(parsed.entries)) {
                  entries.push({ key: k, value: String(v) });
                }
              } else if (typeof parsed === "object") {
                for (const [k, v] of Object.entries(parsed)) {
                  if (k !== "length") {
                    entries.push({ key: k, value: String(v) });
                  }
                }
              }
              if (which === "localStorage") setLocal(entries);
              else setSession(entries);
            } catch {
              if (which === "localStorage") setLocal([]);
              else setSession([]);
            }
          }
        }
      } finally {
        setLoading(false);
      }
    },
    [sessionName],
  );

  useEffect(() => {
    if (sessionName && sessionName !== lastSessionRef.current) {
      lastSessionRef.current = sessionName;
      setCookies([]);
      setLocal([]);
      setSession([]);
    }
    fetchData(tab);
  }, [tab, sessionName, fetchData]);

  const handleRefresh = () => fetchData(tab);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-1.5 px-3 py-2">
        {TABS.map((t) => (
          <button
            key={t.key}
            type="button"
            onClick={() => {
              setTab(t.key);
              setExpanded(null);
            }}
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] transition-colors",
              tab === t.key
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {t.label}
          </button>
        ))}
        <button
          type="button"
          onClick={handleRefresh}
          disabled={loading || !sessionName}
          className="ml-auto flex size-5 items-center justify-center rounded text-muted-foreground transition-colors hover:text-foreground disabled:opacity-40"
          title="Refresh"
        >
          {loading ? (
            <Loader2 className="size-3 animate-spin" />
          ) : (
            <RefreshCw className="size-3" />
          )}
        </button>
      </div>
      <Separator />

      <div className="min-h-0 flex-1 overflow-y-auto font-mono">
        {!sessionName ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            No active session
          </div>
        ) : tab === "cookies" ? (
          <CookiesView
            cookies={cookies}
            loading={loading}
            expanded={expanded}
            onExpand={setExpanded}
          />
        ) : (
          <KeyValueView
            entries={tab === "localStorage" ? local : session}
            loading={loading}
            expanded={expanded}
            onExpand={setExpanded}
          />
        )}
      </div>
    </div>
  );
}

function CookiesView({
  cookies,
  loading,
  expanded,
  onExpand,
}: {
  cookies: CookieEntry[];
  loading: boolean;
  expanded: string | null;
  onExpand: (key: string | null) => void;
}) {
  if (loading && cookies.length === 0) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="size-4 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (cookies.length === 0) {
    return (
      <div className="py-8 text-center text-xs text-muted-foreground">
        No cookies
      </div>
    );
  }

  return (
    <>
      {cookies.map((c) => {
        const id = `${c.domain ?? ""}::${c.name}`;
        const isExpanded = expanded === id;
        return (
          <div key={id} className="border-b border-border/50">
            <button
              type="button"
              onClick={() => onExpand(isExpanded ? null : id)}
              className="flex w-full items-start gap-2 px-3 py-1.5 text-left text-[11px] hover:bg-muted/50"
            >
              <span className="shrink-0 font-semibold text-foreground">
                {c.name}
              </span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {truncate(c.value, 60)}
              </span>
            </button>
            {isExpanded && (
              <div className="space-y-0.5 bg-muted/30 px-3 py-1.5 text-[10px]">
                <DetailRow label="Value" value={c.value} wrap />
                {c.domain && <DetailRow label="Domain" value={c.domain} />}
                {c.path && <DetailRow label="Path" value={c.path} />}
                <DetailRow label="Expires" value={formatExpiry(c.expires)} />
                <DetailRow
                  label="Flags"
                  value={[
                    c.httpOnly && "HttpOnly",
                    c.secure && "Secure",
                    c.sameSite && `SameSite=${c.sameSite}`,
                  ]
                    .filter(Boolean)
                    .join(", ") || "None"}
                />
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

function KeyValueView({
  entries,
  loading,
  expanded,
  onExpand,
}: {
  entries: StorageEntry[];
  loading: boolean;
  expanded: string | null;
  onExpand: (key: string | null) => void;
}) {
  if (loading && entries.length === 0) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="size-4 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div className="py-8 text-center text-xs text-muted-foreground">
        No entries
      </div>
    );
  }

  return (
    <>
      {entries.map((e) => {
        const isExpanded = expanded === e.key;
        return (
          <div key={e.key} className="border-b border-border/50">
            <button
              type="button"
              onClick={() => onExpand(isExpanded ? null : e.key)}
              className="flex w-full items-start gap-2 px-3 py-1.5 text-left text-[11px] hover:bg-muted/50"
            >
              <span className="shrink-0 font-semibold text-foreground">
                {e.key}
              </span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {truncate(e.value, 80)}
              </span>
            </button>
            {isExpanded && (
              <div className="bg-muted/30 px-3 py-1.5 text-[10px]">
                <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all text-foreground">
                  {formatValue(e.value)}
                </pre>
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

function DetailRow({
  label,
  value,
  wrap,
}: {
  label: string;
  value: string;
  wrap?: boolean;
}) {
  return (
    <div className="flex gap-2">
      <span className="w-14 shrink-0 text-muted-foreground">{label}</span>
      <span
        className={cn(
          "min-w-0 flex-1 text-foreground",
          wrap ? "break-all whitespace-pre-wrap" : "truncate",
        )}
      >
        {value}
      </span>
    </div>
  );
}

function formatValue(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}
