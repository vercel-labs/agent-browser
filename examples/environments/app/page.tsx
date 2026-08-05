"use client";

import { useState, useEffect, useRef, useSyncExternalStore } from "react";
import { getEnvStatus } from "./actions/browse";
import type { EnvStatus } from "./actions/browse";
import {
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
} from "@/components/ui/resizable";
import { ALLOWED_URLS } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Loader2, Monitor, CircleX, Sun, Moon, Check } from "lucide-react";

const MOBILE_QUERY = "(max-width: 767px)";
const subscribe = (cb: () => void) => {
  const mql = window.matchMedia(MOBILE_QUERY);
  mql.addEventListener("change", cb);
  return () => mql.removeEventListener("change", cb);
};
const getMobileSnapshot = () => window.matchMedia(MOBILE_QUERY).matches;
const getServerSnapshot = () => false;

function useIsMobile() {
  return useSyncExternalStore(subscribe, getMobileSnapshot, getServerSnapshot);
}

function useTheme() {
  const [theme, setThemeState] = useState<"light" | "dark">("light");

  useEffect(() => {
    const stored = localStorage.getItem("theme");
    const initial =
      stored === "dark" ||
      (!stored && window.matchMedia("(prefers-color-scheme: dark)").matches)
        ? "dark"
        : "light";
    setThemeState(initial);
    document.documentElement.classList.toggle("dark", initial === "dark");
  }, []);

  const toggle = () => {
    const next = theme === "dark" ? "light" : "dark";
    setThemeState(next);
    document.documentElement.classList.toggle("dark", next === "dark");
    localStorage.setItem("theme", next);
  };

  return { theme, toggle };
}

type Action = "screenshot" | "snapshot";

type StepInfo = {
  step: string;
  status: "running" | "done" | "error";
  elapsed?: number;
};

type BrowseResult = {
  ok: boolean;
  screenshot?: string;
  snapshot?: string;
  title?: string;
  error?: string;
};

function formatError(raw: string): string {
  let cleaned = raw.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
  const match = cleaned.match(/(?:error|Error)[:\s]*(.{1,200})/);
  if (match) cleaned = match[1].trim();
  if (cleaned.length > 300) cleaned = cleaned.slice(0, 300) + "...";
  return cleaned || raw.slice(0, 300);
}

function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string }[];
}) {
  return (
    <div className="inline-flex rounded-lg border border-input bg-muted p-0.5 w-full">
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          onClick={() => onChange(opt.value)}
          className={`
            flex-1 px-3 py-1.5 text-[13px] font-medium rounded-md transition-all cursor-pointer
            ${
              value === opt.value
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            }
          `}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

function StepIndicator({ step }: { step: StepInfo }) {
  return (
    <div className="flex items-center gap-2.5 py-1">
      <div className="size-4 flex items-center justify-center shrink-0">
        {step.status === "running" ? (
          <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
        ) : step.status === "done" ? (
          <Check className="size-3.5 text-emerald-500" />
        ) : (
          <CircleX className="size-3.5 text-destructive" />
        )}
      </div>
      <span
        className={`text-[13px] ${
          step.status === "running"
            ? "text-foreground"
            : step.status === "done"
              ? "text-muted-foreground"
              : "text-destructive"
        }`}
      >
        {step.step}
      </span>
      {step.elapsed != null && step.status !== "running" && (
        <span className="text-[11px] text-muted-foreground/60 tabular-nums ml-auto">
          {(step.elapsed / 1000).toFixed(1)}s
        </span>
      )}
    </div>
  );
}

function ErrorDisplay({ error }: { error: string }) {
  const isHtml = /<[a-z][\s\S]*>/i.test(error);
  const message = isHtml ? formatError(error) : error;
  const showRaw = isHtml && error.length > 100;

  return (
    <div className="w-full max-w-2xl space-y-0">
      <Alert variant="destructive">
        <CircleX className="size-4" />
        <AlertTitle>Request failed</AlertTitle>
        <AlertDescription>{message}</AlertDescription>
      </Alert>
      {showRaw && (
        <details className="border border-t-0 border-border rounded-b-lg overflow-hidden">
          <summary className="px-4 py-2 text-[11px] font-medium text-muted-foreground cursor-pointer hover:bg-muted transition-colors">
            Show raw response
          </summary>
          <pre className="px-4 py-3 text-[11px] leading-relaxed text-muted-foreground font-mono overflow-auto max-h-[200px] bg-muted/50">
            {error}
          </pre>
        </details>
      )}
    </div>
  );
}

async function streamBrowse(
  url: string,
  action: Action,
  onStep: (step: StepInfo) => void,
): Promise<BrowseResult> {
  const res = await fetch("/api/browse", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url, action }),
  });

  if (!res.ok) {
    const body = await res.json().catch(() => null);
    return { ok: false, error: body?.error || `HTTP ${res.status}` };
  }

  const reader = res.body?.getReader();
  if (!reader) {
    return { ok: false, error: "No response stream" };
  }

  const decoder = new TextDecoder();
  let buffer = "";
  let result: BrowseResult = { ok: false, error: "No result received" };

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    buffer += decoder.decode(value, { stream: true });

    const parts = buffer.split("\n\n");
    buffer = parts.pop() || "";

    for (const part of parts) {
      const eventMatch = part.match(/^event: (\w+)\ndata: ([\s\S]+)$/);
      if (!eventMatch) continue;

      const [, event, data] = eventMatch;
      try {
        const parsed = JSON.parse(data);
        if (event === "step") {
          onStep(parsed as StepInfo);
        } else if (event === "result") {
          result = parsed as BrowseResult;
        }
      } catch {
        // skip malformed events
      }
    }
  }

  return result;
}

export default function Home() {
  const isMobile = useIsMobile();
  const { theme, toggle: toggleTheme } = useTheme();
  const [url, setUrl] = useState<string>(ALLOWED_URLS[0]);
  const [loading, setLoading] = useState(false);
  const [action, setAction] = useState<Action>("screenshot");
  const [result, setResult] = useState<BrowseResult | null>(null);
  const [steps, setSteps] = useState<StepInfo[]>([]);
  const [envStatus, setEnvStatus] = useState<EnvStatus | null>(null);
  const stepsEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    getEnvStatus().then(setEnvStatus);
  }, []);

  function clearResults() {
    setResult(null);
    setSteps([]);
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setLoading(true);
    setResult(null);
    setSteps([]);

    try {
      const browseResult = await streamBrowse(url, action, (step) => {
        setSteps((prev) => {
          const existing = prev.findIndex((s) => s.step === step.step);
          if (existing >= 0) {
            const updated = [...prev];
            updated[existing] = step;
            return updated;
          }
          return [...prev, step];
        });
      });
      setResult(browseResult);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setResult({ ok: false, error: message });
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    stepsEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [steps]);

  const controlsForm = (
    <form onSubmit={handleSubmit} className="p-5 space-y-5">
      <div className="space-y-1.5">
        <Label
          htmlFor="url-select"
          className="text-[11px] text-muted-foreground uppercase tracking-wider"
        >
          URL
        </Label>
        <Select
          value={url}
          onValueChange={(v) => {
            if (v) setUrl(v);
            clearResults();
          }}
        >
          <SelectTrigger id="url-select">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {ALLOWED_URLS.map((u) => (
              <SelectItem key={u} value={u}>
                {u.replace("https://", "")}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="space-y-1.5">
        <Label className="text-[11px] text-muted-foreground uppercase tracking-wider">
          Action
        </Label>
        <SegmentedControl<Action>
          value={action}
          onChange={(v) => {
            setAction(v);
            clearResults();
          }}
          options={[
            { value: "screenshot", label: "Screenshot" },
            { value: "snapshot", label: "Snapshot" },
          ]}
        />
        <p className="text-[11px] text-muted-foreground">
          {action === "screenshot"
            ? "Captures a full-page PNG image"
            : "Returns the accessibility tree"}
        </p>
      </div>

      {envStatus && !envStatus.sandbox.hasSnapshot && (
        <Alert>
          <AlertTitle className="text-[12px]">
            Sandbox snapshot not configured
          </AlertTitle>
          <AlertDescription className="text-[11px]">
            Without a sandbox snapshot, the VM installs agent-browser +
            Chromium on every request (~30s). Create one with{" "}
            <code className="text-[10px] bg-muted px-1 py-0.5 rounded">
              npx tsx scripts/create-snapshot.ts
            </code>{" "}
            and set{" "}
            <code className="text-[10px] bg-muted px-1 py-0.5 rounded">
              AGENT_BROWSER_SNAPSHOT_ID
            </code>{" "}
            for sub-second startup.
          </AlertDescription>
        </Alert>
      )}

      <Button type="submit" disabled={loading} className="w-full" size="lg">
        {loading && <Loader2 className="size-4 animate-spin" />}
        {loading ? "Running..." : "Run"}
      </Button>
    </form>
  );

  const showSteps = loading || (steps.length > 0 && !result);
  const hasResult = result && !loading;

  const resultContent = showSteps ? (
    <div className="p-6 lg:p-10">
      <div className="max-w-xl mx-auto">
        <div className="space-y-0.5">
          {steps.map((s, i) => (
            <StepIndicator key={`${s.step}-${i}`} step={s} />
          ))}
          <div ref={stepsEndRef} />
        </div>
      </div>
    </div>
  ) : hasResult ? (
    <div className="flex flex-col items-center p-6 lg:p-10">
      {result.ok && result.screenshot && (
        <div className="w-full max-w-3xl">
          <div className="flex items-center justify-between mb-4">
            <h2 className="text-sm font-semibold truncate mr-3">
              {result.title}
            </h2>
            <Badge
              variant="outline"
              className="font-mono text-[11px] shrink-0"
            >
              screenshot
            </Badge>
          </div>
          <div className="rounded-xl border border-border overflow-hidden shadow-sm">
            <img
              src={`data:image/png;base64,${result.screenshot}`}
              alt={result.title}
              className="w-full block"
            />
          </div>
          <details className="mt-4">
            <summary className="text-[11px] text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
              Show steps ({steps.length})
            </summary>
            <div className="mt-2 space-y-0.5">
              {steps.map((s, i) => (
                <StepIndicator key={`${s.step}-${i}`} step={s} />
              ))}
            </div>
          </details>
        </div>
      )}

      {result.ok && result.snapshot && (
        <div className="w-full max-w-3xl">
          <div className="flex items-center justify-between mb-4">
            <h2 className="text-sm font-semibold truncate mr-3">
              {result.title}
            </h2>
            <Badge
              variant="outline"
              className="font-mono text-[11px] shrink-0"
            >
              snapshot
            </Badge>
          </div>
          <pre className="bg-card rounded-xl border border-border p-5 overflow-auto text-[13px] leading-relaxed font-mono max-h-[calc(100vh-12rem)]">
            {result.snapshot}
          </pre>
          <details className="mt-4">
            <summary className="text-[11px] text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
              Show steps ({steps.length})
            </summary>
            <div className="mt-2 space-y-0.5">
              {steps.map((s, i) => (
                <StepIndicator key={`${s.step}-${i}`} step={s} />
              ))}
            </div>
          </details>
        </div>
      )}

      {!result.ok && (
        <div className="w-full max-w-2xl space-y-4">
          <ErrorDisplay error={result.error ?? "Unknown error"} />
          {steps.length > 0 && (
            <div className="space-y-0.5">
              {steps.map((s, i) => (
                <StepIndicator key={`${s.step}-${i}`} step={s} />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  ) : (
    <div className="min-h-[300px] md:h-full flex flex-col items-center justify-center text-muted-foreground">
      <Monitor className="size-12 mb-4 opacity-30" strokeWidth={1} />
      <p className="text-sm font-medium mb-1">No result yet</p>
      <p className="text-[13px]">Pick a URL and click Run</p>
    </div>
  );

  return (
    <div className="h-screen flex flex-col">
      <header className="border-b border-border shrink-0">
        <div className="px-4 md:px-6 h-12 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <span className="text-sm font-semibold tracking-tight">
              agent-browser
            </span>
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={toggleTheme}
              className="size-8 inline-flex items-center justify-center rounded-md border border-input bg-background text-muted-foreground hover:text-foreground transition-colors cursor-pointer"
              aria-label="Toggle theme"
            >
              {theme === "dark" ? (
                <Sun className="size-4" />
              ) : (
                <Moon className="size-4" />
              )}
            </button>
            <a
              href="https://vercel.com/new/clone?repository-url=https%3A%2F%2Fgithub.com%2Fvercel-labs%2Fagent-browser%2Ftree%2Fmain%2Fexamples%2Fenvironments&project-name=agent-browser-environments&repository-name=agent-browser-environments"
              target="_blank"
              rel="noopener noreferrer"
            >
              <img
                src="https://vercel.com/button"
                alt="Deploy with Vercel"
                className="h-8"
              />
            </a>
          </div>
        </div>
      </header>

      {isMobile ? (
        <div className="flex-1 overflow-auto">
          <div className="border-b border-border">{controlsForm}</div>
          <div className="bg-surface">{resultContent}</div>
        </div>
      ) : (
        <ResizablePanelGroup orientation="horizontal" className="flex-1">
          <ResizablePanel defaultSize="30%" minSize="20%" maxSize="50%">
            <aside className="h-full overflow-y-auto">{controlsForm}</aside>
          </ResizablePanel>

          <ResizableHandle withHandle />

          <ResizablePanel defaultSize="70%">
            <main className="h-full overflow-auto bg-surface">
              {resultContent}
            </main>
          </ResizablePanel>
        </ResizablePanelGroup>
      )}
    </div>
  );
}
