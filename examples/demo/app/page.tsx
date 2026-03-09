"use client";

import { useState, useEffect, useSyncExternalStore } from "react";
import { takeScreenshot, takeSnapshot, getEnvStatus } from "./actions/browse";
import type {
  ScreenshotResult,
  SnapshotResult,
  Mode,
  EnvStatus,
} from "./actions/browse";
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
import { Loader2, Monitor, TriangleAlert, CircleX } from "lucide-react";

const MOBILE_QUERY = "(max-width: 767px)";
const subscribe = (cb: () => void) => {
  const mql = window.matchMedia(MOBILE_QUERY);
  mql.addEventListener("change", cb);
  return () => mql.removeEventListener("change", cb);
};
const getSnapshot = () => window.matchMedia(MOBILE_QUERY).matches;
const getServerSnapshot = () => false;

function useIsMobile() {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}

type Action = "screenshot" | "snapshot";

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

function EnvBadge({
  label,
  value,
  status,
}: {
  label: string;
  value?: string;
  status: "ok" | "warn" | "missing";
}) {
  const variant =
    status === "ok"
      ? "outline"
      : status === "warn"
        ? "secondary"
        : "destructive";
  const icon =
    status === "ok" ? "\u2713" : status === "warn" ? "\u26A0" : "\u2717";

  return (
    <Badge variant={variant} className="gap-1 font-mono text-[10px]">
      <span>{icon}</span>
      {label}
      {value && <span className="opacity-60">{value}</span>}
    </Badge>
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

function ModeCard({
  selected,
  onSelect,
  title,
  description,
  badges,
}: {
  selected: boolean;
  onSelect: () => void;
  title: string;
  description: string;
  badges?: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={`
        w-full text-left rounded-lg border p-3 transition-all cursor-pointer
        ${
          selected
            ? "border-ring bg-background ring-1 ring-ring/20"
            : "border-input bg-background hover:border-ring/50"
        }
      `}
    >
      <div className="flex items-center gap-2 mb-1.5">
        <div
          className={`
            size-3.5 rounded-full border-2 flex items-center justify-center shrink-0 transition-colors
            ${selected ? "border-foreground" : "border-muted-foreground/30"}
          `}
        >
          {selected && (
            <div className="size-1.5 rounded-full bg-foreground" />
          )}
        </div>
        <span className="text-[13px] font-semibold">{title}</span>
      </div>
      <p className="text-[12px] text-muted-foreground leading-relaxed pl-[22px] mb-2">
        {description}
      </p>
      {badges && (
        <div className="flex flex-wrap gap-1.5 pl-[22px]">{badges}</div>
      )}
    </button>
  );
}

export default function Home() {
  const isMobile = useIsMobile();
  const [url, setUrl] = useState<string>(ALLOWED_URLS[0]);
  const [loading, setLoading] = useState(false);
  const [action, setAction] = useState<Action>("screenshot");
  const [mode, setMode] = useState<Mode>("serverless");
  const [screenshotResult, setScreenshotResult] =
    useState<ScreenshotResult | null>(null);
  const [snapshotResult, setSnapshotResult] =
    useState<SnapshotResult | null>(null);
  const [envStatus, setEnvStatus] = useState<EnvStatus | null>(null);

  useEffect(() => {
    getEnvStatus().then(setEnvStatus);
  }, []);

  function clearResults() {
    setScreenshotResult(null);
    setSnapshotResult(null);
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setLoading(true);
    setScreenshotResult(null);
    setSnapshotResult(null);

    try {
      if (action === "screenshot") {
        const result = await takeScreenshot(url, mode);
        setScreenshotResult(result);
      } else {
        const result = await takeSnapshot(url, mode);
        setSnapshotResult(result);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (action === "screenshot") {
        setScreenshotResult({ ok: false, error: message });
      } else {
        setSnapshotResult({ ok: false, error: message });
      }
    } finally {
      setLoading(false);
    }
  }

  const hasResult = screenshotResult || snapshotResult;

  const envWarning =
    envStatus &&
    mode === "serverless" &&
    !envStatus.serverless.isVercel &&
    !envStatus.serverless.hasChromiumPath
      ? "Running locally without CHROMIUM_PATH. The app will try to use your system Chrome. Set CHROMIUM_PATH if Chrome is not in the default location."
      : null;

  const controlsForm = (
    <form onSubmit={handleSubmit} className="p-5 space-y-5">
      <div className="space-y-1.5">
        <Label
          htmlFor="url-select"
          className="text-[11px] text-muted-foreground uppercase tracking-wider"
        >
          URL
        </Label>
        <select
          id="url-select"
          value={url}
          onChange={(e) => {
            setUrl(e.target.value);
            clearResults();
          }}
          className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-xs transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        >
          {ALLOWED_URLS.map((u) => (
            <option key={u} value={u}>
              {u.replace("https://", "")}
            </option>
          ))}
        </select>
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

      <div className="space-y-1.5">
        <Label className="text-[11px] text-muted-foreground uppercase tracking-wider">
          Environment
        </Label>
        <div className="space-y-2">
          <ModeCard
            selected={mode === "serverless"}
            onSelect={() => {
              setMode("serverless");
              clearResults();
            }}
            title="Serverless Function"
            description="Runs @sparticuz/chromium + puppeteer-core directly in a Vercel function."
            badges={
              envStatus && (
                <EnvBadge
                  label="@sparticuz/chromium"
                  status={
                    envStatus.serverless.isVercel
                      ? "ok"
                      : envStatus.serverless.hasChromiumPath
                        ? "ok"
                        : "warn"
                  }
                  value={
                    envStatus.serverless.isVercel
                      ? "auto"
                      : envStatus.serverless.hasChromiumPath
                        ? "CHROMIUM_PATH"
                        : "system Chrome"
                  }
                />
              )
            }
          />
          <ModeCard
            selected={mode === "sandbox"}
            onSelect={() => {
              setMode("sandbox");
              clearResults();
            }}
            title="Vercel Sandbox"
            description={
              envStatus?.sandbox.hasSnapshot
                ? "Ephemeral microVM with agent-browser + Chrome pre-installed via sandbox snapshot."
                : "Ephemeral microVM with agent-browser + Chrome. No binary size limits."
            }
            badges={
              envStatus && (
                <EnvBadge
                  label="Sandbox snapshot"
                  status={
                    envStatus.sandbox.hasSnapshot ? "ok" : "warn"
                  }
                  value={
                    envStatus.sandbox.hasSnapshot
                      ? "sub-second startup"
                      : "~30s cold start"
                  }
                />
              )
            }
          />
        </div>
      </div>

      {envWarning && (
        <Alert>
          <TriangleAlert className="size-4" />
          <AlertTitle>Local development</AlertTitle>
          <AlertDescription>{envWarning}</AlertDescription>
        </Alert>
      )}

      <Button
        type="submit"
        disabled={loading}
        className="w-full"
        size="lg"
      >
        {loading && <Loader2 className="size-4 animate-spin" />}
        {loading ? "Running..." : "Run"}
      </Button>
    </form>
  );

  const resultContent = loading ? (
    <div className="min-h-[300px] md:h-full flex flex-col items-center justify-center gap-3 text-muted-foreground">
      <Loader2 className="size-6 animate-spin" />
      <p className="text-sm">Taking {action}...</p>
    </div>
  ) : hasResult ? (
    <div className="flex flex-col items-center p-6 lg:p-10">
      {screenshotResult &&
        (screenshotResult.ok ? (
          <div className="w-full max-w-3xl">
            <div className="flex items-center justify-between mb-4">
              <h2 className="text-sm font-semibold truncate mr-3">
                {screenshotResult.title}
              </h2>
              <Badge variant="outline" className="font-mono text-[11px] shrink-0">
                screenshot
              </Badge>
            </div>
            <div className="rounded-xl border border-border overflow-hidden shadow-sm">
              <img
                src={`data:image/png;base64,${screenshotResult.screenshot}`}
                alt={screenshotResult.title}
                className="w-full block"
              />
            </div>
          </div>
        ) : (
          <ErrorDisplay
            error={screenshotResult.error ?? "Unknown error"}
          />
        ))}

      {snapshotResult &&
        (snapshotResult.ok ? (
          <div className="w-full max-w-3xl">
            <div className="flex items-center justify-between mb-4">
              <h2 className="text-sm font-semibold truncate mr-3">
                {snapshotResult.title}
              </h2>
              <Badge variant="outline" className="font-mono text-[11px] shrink-0">
                snapshot
              </Badge>
            </div>
            <pre className="bg-card rounded-xl border border-border p-5 overflow-auto text-[13px] leading-relaxed font-mono max-h-[calc(100vh-12rem)]">
              {snapshotResult.snapshot}
            </pre>
          </div>
        ) : (
          <ErrorDisplay
            error={snapshotResult.error ?? "Unknown error"}
          />
        ))}
    </div>
  ) : (
    <div className="min-h-[300px] md:h-full flex flex-col items-center justify-center text-muted-foreground">
      <Monitor className="size-12 mb-4 opacity-30" strokeWidth={1} />
      <p className="text-sm font-medium mb-1">No result yet</p>
      <p className="text-[13px]">Enter a URL and click Run</p>
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
            <span className="text-muted-foreground text-sm hidden sm:inline">/</span>
            <span className="text-sm text-muted-foreground hidden sm:inline">
              Demo
            </span>
          </div>
          <a
            href="https://vercel.com/new/clone?repository-url=https%3A%2F%2Fgithub.com%2Fagent-browser%2Fagent-browser%2Ftree%2Fmain%2Fexamples%2Fdemo&env=CHROMIUM_PATH&envDescription=Optional%20path%20to%20Chromium%20binary.%20Not%20needed%20on%20Vercel.&envLink=https%3A%2F%2Fgithub.com%2Fagent-browser%2Fagent-browser%2Ftree%2Fmain%2Fexamples%2Fdemo%23environment-variables&project-name=agent-browser-demo&repository-name=agent-browser-demo"
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
