"use client";

import {
  useRef,
  useEffect,
  useState,
  useCallback,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useChat } from "@ai-sdk/react";
import { DefaultChatTransport } from "ai";
import { Streamdown } from "streamdown";
import Link from "next/link";
import { Button } from "@vercel/geistdocs/components/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

const STORAGE_KEY = "docs-chat-messages";
const transport = new DefaultChatTransport({ api: "/api/docs-chat" });

const DESKTOP_DEFAULT_WIDTH = 400;
const DESKTOP_MIN_WIDTH = 300;
const DESKTOP_MAX_WIDTH = 700;
const DESKTOP_QUERY = "(min-width: 1280px)";

function subscribeToDesktop(onStoreChange: () => void) {
  const media = window.matchMedia(DESKTOP_QUERY);
  media.addEventListener("change", onStoreChange);
  return () => media.removeEventListener("change", onStoreChange);
}

function getDesktopSnapshot() {
  return window.matchMedia(DESKTOP_QUERY).matches;
}

function getServerDesktopSnapshot(): boolean | null {
  return null;
}

function setCookie(name: string, value: string) {
  document.cookie = `${name}=${encodeURIComponent(value)};path=/;max-age=${60 * 60 * 24 * 365};samesite=lax`;
}

const TOOL_LABELS: Record<
  string,
  { label: string; pastLabel: string; argKey?: string }
> = {
  readFile: { label: "Reading", pastLabel: "Read", argKey: "path" },
  bash: { label: "Running", pastLabel: "Ran", argKey: "command" },
};

function isToolPart(part: { type: string }): part is {
  type: string;
  toolCallId: string;
  toolName?: string;
  state: string;
  input?: Record<string, unknown>;
  output?: unknown;
  errorText?: string;
} {
  return part.type.startsWith("tool-") || part.type === "dynamic-tool";
}

function getToolName(part: { type: string; toolName?: string }): string {
  if (part.type === "dynamic-tool") return part.toolName ?? "tool";
  return part.type.replace(/^tool-/, "");
}

function ToolCallDisplay({
  part,
}: {
  part: {
    type: string;
    toolCallId: string;
    toolName?: string;
    state: string;
    input?: Record<string, unknown>;
    output?: unknown;
    errorText?: string;
  };
}) {
  const toolName = getToolName(part);
  const config = TOOL_LABELS[toolName] ?? {
    label: toolName,
    pastLabel: toolName,
  };
  const isDone = part.state === "output-available";
  const isError = part.state === "output-error";
  const isRunning = !isDone && !isError;
  const displayLabel = isRunning ? config.label : config.pastLabel;

  const args = (part.input ?? {}) as Record<string, unknown>;
  const argValue = config.argKey ? args[config.argKey] : undefined;
  const argPreview =
    argValue != null
      ? String(argValue)
          .replace(/^\/workspace\//, "/")
          .replace(/\.md$/, "")
          .replace(/\/index$/, "") || "/"
      : "";

  // Link to the docs page if it's a readFile path
  const docsLink =
    toolName === "readFile" && argPreview.startsWith("/") ? argPreview : null;

  const argEl = argPreview ? (
    docsLink ? (
      <Link href={docsLink} className="truncate underline underline-offset-2">
        {argPreview}
      </Link>
    ) : (
      <span className="truncate">{argPreview}</span>
    )
  ) : null;

  return (
    <div className="text-xs py-0.5 min-w-0">
      {isRunning ? (
        <span className="inline-flex items-center gap-1 font-mono text-muted-foreground animate-tool-shimmer min-w-0 max-w-full">
          <span className="shrink-0">{displayLabel}</span>
          {argEl}
        </span>
      ) : (
        <span className="inline-flex items-center gap-1 font-mono text-muted-foreground/60 min-w-0 max-w-full">
          <span className="shrink-0">{displayLabel}</span>
          {argEl}
          {isError && <span className="text-destructive">failed</span>}
        </span>
      )}
    </div>
  );
}

const SUGGESTIONS = [
  "What is agent-browser?",
  "How do I install it?",
  "What commands are available?",
  "How do snapshots work?",
  "How do I use CDP mode?",
];

export function DocsChat({
  defaultOpen = false,
  defaultWidth = DESKTOP_DEFAULT_WIDTH,
}: {
  defaultOpen?: boolean;
  defaultWidth?: number;
}) {
  const [openOverride, setOpen] = useState<boolean | null>(null);
  const [input, setInput] = useState("");
  const isDesktop = useSyncExternalStore(
    subscribeToDesktop,
    getDesktopSnapshot,
    getServerDesktopSnapshot,
  );
  const hasMounted = isDesktop !== null;
  const open = openOverride ?? (defaultOpen && (isDesktop ?? true));
  const [desktopWidth, setDesktopWidth] = useState(() =>
    Number.isFinite(defaultWidth) && defaultWidth > 0
      ? Math.min(DESKTOP_MAX_WIDTH, Math.max(DESKTOP_MIN_WIDTH, defaultWidth))
      : DESKTOP_DEFAULT_WIDTH,
  );
  const messagesScrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const launcherRef = useRef<HTMLDivElement>(null);
  const restoredRef = useRef(false);
  const isDraggingRef = useRef(false);

  const { messages, sendMessage, status, setMessages, error } = useChat({
    transport,
    onError: () => setOpen(true),
  });

  const isLoading = status === "streaming" || status === "submitted";
  const showMessages = messages.length > 0 || !!error || isLoading;

  // Persist open state to cookie (only after mount to avoid overwriting on mobile)
  useEffect(() => {
    if (hasMounted) {
      setCookie("docs-chat-open", String(open));
    }
  }, [open, hasMounted]);

  useEffect(() => {
    const launcher = launcherRef.current;
    if (!hasMounted || open || !launcher) return;
    const footer = document.querySelector("footer");
    let frame = 0;
    const update = () => {
      frame = 0;
      const rect = document
        .querySelector("footer fieldset")
        ?.getBoundingClientRect();
      const overlap =
        rect && rect.width > 0 && rect.bottom > 0
          ? Math.max(0, window.innerHeight - rect.top)
          : 0;
      launcher.style.setProperty("--chat-launcher-bottom", `${24 + overlap}px`);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    const resize = new ResizeObserver(schedule);
    resize.observe(document.body);
    const mutation = new MutationObserver(schedule);
    if (footer) {
      resize.observe(footer);
      mutation.observe(footer, { childList: true, subtree: true });
    }
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    update();
    return () => {
      cancelAnimationFrame(frame);
      resize.disconnect();
      mutation.disconnect();
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, [hasMounted, open]);

  useEffect(() => {
    const body = document.body;
    if (isDesktop && open) {
      body.style.paddingRight = `${desktopWidth}px`;
      if (!isDraggingRef.current) {
        body.style.transition = "padding-right 150ms ease";
      }
    } else if (isDesktop) {
      body.style.paddingRight = "0px";
      body.style.transition = "padding-right 150ms ease";
    }
    return () => {
      body.style.paddingRight = "0px";
      body.style.transition = "";
    };
  }, [isDesktop, open, desktopWidth]);

  // Resize handle drag
  const handleResizePointerDown = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => {
      e.preventDefault();
      isDraggingRef.current = true;
      document.documentElement.style.transition = "none";
      const startX = e.clientX;
      const startWidth = desktopWidth;

      const onPointerMove = (ev: globalThis.PointerEvent) => {
        const delta = startX - ev.clientX;
        const newWidth = Math.min(
          DESKTOP_MAX_WIDTH,
          Math.max(DESKTOP_MIN_WIDTH, startWidth + delta),
        );
        setDesktopWidth(newWidth);
      };

      const onPointerUp = () => {
        isDraggingRef.current = false;
        document.documentElement.style.transition = "";
        document.removeEventListener("pointermove", onPointerMove);
        document.removeEventListener("pointerup", onPointerUp);
      };

      document.addEventListener("pointermove", onPointerMove);
      document.addEventListener("pointerup", onPointerUp);
    },
    [desktopWidth],
  );

  // Persist width to cookie
  useEffect(() => {
    setCookie("docs-chat-width", String(desktopWidth));
  }, [desktopWidth]);

  // Restore messages from sessionStorage on mount
  useEffect(() => {
    if (restoredRef.current) return;
    restoredRef.current = true;
    try {
      const stored = sessionStorage.getItem(STORAGE_KEY);
      if (stored) {
        const parsed = JSON.parse(stored);
        if (Array.isArray(parsed) && parsed.length > 0) {
          setMessages(parsed);
        }
      }
    } catch {
      // ignore parse errors
    }
  }, [setMessages]);

  // Save completed messages to sessionStorage
  useEffect(() => {
    if (!restoredRef.current) return;
    if (isLoading) return;
    if (messages.length === 0) {
      sessionStorage.removeItem(STORAGE_KEY);
      return;
    }
    try {
      sessionStorage.setItem(STORAGE_KEY, JSON.stringify(messages));
    } catch {
      // ignore quota errors
    }
  }, [messages, isLoading]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "i" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen(!open);
      }
      if (e.key === "Escape" && open) {
        setOpen(false);
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open]);

  // Auto-focus input when opened
  useEffect(() => {
    if (open) {
      const timer = setTimeout(() => inputRef.current?.focus(), 200);
      return () => clearTimeout(timer);
    }
  }, [open]);

  // Scroll to bottom when messages change or error occurs
  useEffect(() => {
    const el = messagesScrollRef.current;
    if (!el) return;
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
  }, [messages, error]);

  const handleSubmit = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault();
      if (!input.trim() || isLoading) return;
      sendMessage({ text: input });
      setInput("");
    },
    [input, isLoading, sendMessage],
  );

  const handleClear = useCallback(() => {
    setMessages([]);
    sessionStorage.removeItem(STORAGE_KEY);
  }, [setMessages]);

  const hasVisibleContent = (
    parts: (typeof messages)[number]["parts"],
  ): boolean => {
    return parts.some(
      (p) => (p.type === "text" && p.text.length > 0) || isToolPart(p),
    );
  };

  // Shared chat panel content used by both desktop and mobile
  const chatPanel = (
    <>
      <div className="flex items-center justify-between gap-2 border-b border-border shrink-0 pl-[max(1rem,env(safe-area-inset-left))] pr-[max(1rem,env(safe-area-inset-right))] pt-[max(0.75rem,env(safe-area-inset-top))] pb-3">
        <span className="text-sm font-medium">agent-browser Docs</span>
        <div className="flex items-center gap-3">
          {showMessages && (
            <Button
              onClick={handleClear}
              size="small"
              variant="tertiary"
              aria-label="Clear conversation"
            >
              Clear
            </Button>
          )}
          <Button
            onClick={() => {
              setOpen(false);
              requestAnimationFrame(() =>
                launcherRef.current?.querySelector("button")?.focus(),
              );
            }}
            size="small"
            variant="tertiary"
            svgOnly
            aria-label="Close panel"
          >
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </Button>
        </div>
      </div>

      {showMessages ? (
        <div
          ref={messagesScrollRef}
          className="flex flex-col flex-1 min-h-0 gap-4 py-4 pl-[max(1rem,env(safe-area-inset-left))] pr-[max(1rem,env(safe-area-inset-right))] overflow-y-auto"
        >
          {messages.map((message) => {
            if (!hasVisibleContent(message.parts)) return null;
            return (
              <div key={message.id}>
                {message.role === "user" ? (
                  <div className="text-sm text-muted-foreground whitespace-pre-wrap leading-relaxed">
                    {message.parts
                      .filter(
                        (p): p is Extract<typeof p, { type: "text" }> =>
                          p.type === "text",
                      )
                      .map((p) => p.text)
                      .join("")}
                  </div>
                ) : (
                  <div className="flex flex-col gap-2">
                    {message.parts.map((part, i) => {
                      if (part.type === "text" && part.text) {
                        return (
                          <div
                            key={i}
                            className="docs-chat-content text-sm text-foreground leading-relaxed"
                          >
                            <Streamdown>{part.text}</Streamdown>
                          </div>
                        );
                      }
                      if (isToolPart(part)) {
                        return (
                          <ToolCallDisplay key={part.toolCallId} part={part} />
                        );
                      }
                      return null;
                    })}
                  </div>
                )}
              </div>
            );
          })}
          {error && (
            <div className="text-sm text-destructive/80 bg-destructive/10 rounded-md px-3 py-2">
              {(() => {
                try {
                  const parsed = JSON.parse(error.message);
                  return parsed.message || parsed.error || error.message;
                } catch {
                  return (
                    error.message || "Something went wrong. Please try again."
                  );
                }
              })()}
            </div>
          )}
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex flex-col">
          <div className="flex flex-wrap gap-2 p-4">
            {SUGGESTIONS.map((s) => (
              <Button
                key={s}
                typeName="button"
                size="small"
                variant="secondary"
                onClick={() => {
                  sendMessage({ text: s });
                }}
              >
                {s}
              </Button>
            ))}
          </div>
        </div>
      )}

      <form
        onSubmit={handleSubmit}
        className="flex items-end gap-2 pl-[max(1rem,env(safe-area-inset-left))] pr-[max(1rem,env(safe-area-inset-right))] pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] border-t border-border shrink-0"
      >
        <textarea
          ref={inputRef}
          value={input}
          onChange={(e) => {
            setInput(e.target.value);
            e.target.style.height = "auto";
            e.target.style.height = `${e.target.scrollHeight}px`;
          }}
          rows={1}
          enterKeyHint="send"
          placeholder="Ask a question..."
          aria-label="Ask a question about agent-browser"
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              handleSubmit(e);
            }
          }}
          className="flex-1 min-w-0 bg-transparent text-base sm:text-sm text-foreground outline-none focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring disabled:opacity-50 resize-none max-h-32 leading-relaxed placeholder:text-muted-foreground"
        />
        <Button
          typeName="submit"
          size="small"
          shape="circle"
          svgOnly
          disabled={isLoading || !input.trim()}
          className="shrink-0"
          aria-label="Send message"
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <line x1="12" y1="19" x2="12" y2="5" />
            <polyline points="5 12 12 5 19 12" />
          </svg>
        </Button>
      </form>
    </>
  );

  return (
    <>
      {!open && (
        <div
          ref={launcherRef}
          data-docs-chat-launcher
          className="fixed bottom-[calc(1rem+env(safe-area-inset-bottom))] left-1/2 z-30 -translate-x-1/2 min-[640px]:right-[max(1.5rem,env(safe-area-inset-right))] min-[640px]:bottom-[max(var(--chat-launcher-bottom,24px),env(safe-area-inset-bottom))] min-[640px]:left-auto min-[640px]:translate-x-0"
        >
          <Button
            onClick={() => setOpen(true)}
            size="medium"
            className="h-10 shadow-lg min-[640px]:h-9"
            aria-label="Ask AI"
            aria-expanded={open}
            aria-controls={
              isDesktop
                ? "agent-browser-chat-desktop"
                : "agent-browser-chat-mobile"
            }
            aria-keyshortcuts="Meta+I Control+I"
          >
            Ask AI
            <kbd className="ml-2 hidden items-center gap-0.5 font-mono text-xs opacity-60 min-[640px]:inline-flex">
              <span>&#8984;</span>I
            </kbd>
          </Button>
        </div>
      )}

      <aside
        id="agent-browser-chat-desktop"
        aria-label="agent-browser documentation assistant"
        inert={!open || !isDesktop}
        className={cn(
          "hidden xl:flex fixed top-0 right-0 bottom-0 z-40 border-l border-border bg-background transition-transform duration-150 ease-in-out motion-reduce:transition-none",
          open ? "translate-x-0" : "translate-x-full",
        )}
        style={{ width: desktopWidth }}
        aria-hidden={!open || !isDesktop}
      >
        {/* Resize handle */}
        <div
          onPointerDown={handleResizePointerDown}
          className="absolute top-0 bottom-0 left-0 w-1.5 cursor-col-resize hover:bg-ring/30 active:bg-ring/50 transition-colors z-10"
        />
        <div className="flex flex-col flex-1 min-w-0">{chatPanel}</div>
      </aside>

      {hasMounted && !isDesktop && (
        <Sheet open={open} onOpenChange={setOpen}>
          <SheetContent
            side="right"
            showCloseButton={false}
            id="agent-browser-chat-mobile"
            aria-describedby={undefined}
            onCloseAutoFocus={(event) => {
              event.preventDefault();
              launcherRef.current?.querySelector("button")?.focus();
            }}
            className="inset-0! w-full! h-dvh! max-w-none! border-l-0! p-0! flex flex-col gap-0"
          >
            <SheetTitle className="sr-only">
              agent-browser documentation assistant
            </SheetTitle>
            {chatPanel}
          </SheetContent>
        </Sheet>
      )}
    </>
  );
}
