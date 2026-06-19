"use client";

import { useRef, useEffect, useState, useCallback, useMemo } from "react";
import { useAtomValue } from "jotai/react";
import { useChat } from "@ai-sdk/react";
import { DefaultChatTransport } from "ai";
import { Streamdown } from "streamdown";
import { getChatApiUrl, chatModelAtom, availableModelsAtom } from "@/store/chat";
import { activeSessionNameAtom } from "@/store/sessions";
import { ModelSelector } from "@/components/model-selector";
import { shikiTheme } from "@/lib/shiki-theme";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import { ArrowUp, Square, Trash2, ChevronRight, ImagePlus, X, Loader, Copy, Check, Download } from "lucide-react";

type ExtraProps = { node?: unknown };
type MdImgProps = React.ImgHTMLAttributes<HTMLImageElement> & ExtraProps;
type MdHeadingProps = React.HTMLAttributes<HTMLHeadingElement> & ExtraProps;
type MdAnchorProps = React.AnchorHTMLAttributes<HTMLAnchorElement> & ExtraProps;
type MdPreProps = React.HTMLAttributes<HTMLPreElement> & ExtraProps;
type MdCodeProps = React.HTMLAttributes<HTMLElement> & ExtraProps;

const chatComponents = {
  img: ({ node: _node, src, alt, ...props }: MdImgProps) => {
    if (typeof src === "string" && src.startsWith("data:image/")) {
      return <img src={src} alt={alt} className="rounded-md border border-border max-w-full my-1" {...props} />;
    }
    return null;
  },
  h1: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  h2: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  h3: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  h4: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  h5: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  h6: ({ node: _node, ...props }: MdHeadingProps) => <p className="font-bold" {...props} />,
  a: ({ node: _node, href, children, ...props }: MdAnchorProps) => (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="text-primary underline underline-offset-2"
      {...props}
    >
      {children}
    </a>
  ),
  pre: ({ node: _node, ...props }: MdPreProps) => (
    <pre
      className="text-[11px] bg-background border border-border rounded-md p-2 my-1.5 whitespace-pre-wrap break-all"
      {...props}
    />
  ),
  code: ({ className, children, node: _node, ...props }: MdCodeProps) => {
    if (className?.includes("language-")) {
      return <code className={className} {...props}>{children}</code>;
    }
    return (
      <span
        className="text-[11px] bg-secondary/60 px-1 py-0.5 rounded text-foreground font-mono break-all"
        {...props}
      >
        {children}
      </span>
    );
  },
};

const STORAGE_PREFIX = "dashboard-chat-";
const IMAGE_DATA_URL_RE = /data:image\/[^;]+;base64,[A-Za-z0-9+/=]+/g;

function stripImagesForStorage(messages: unknown[]): unknown[] {
  const json = JSON.stringify(messages);
  return JSON.parse(json.replace(IMAGE_DATA_URL_RE, "[image stripped]"));
}

const SUGGESTIONS = [
  "Go to vercel.com",
  "Take a screenshot",
  "What's on the page?",
  "Click the first link",
];

interface ToolInvocationPart {
  type: string;
  toolCallId: string;
  state: string;
  input?: Record<string, unknown>;
  output?: unknown;
}

function isToolPart(part: { type: string }): part is ToolInvocationPart {
  return part.type.startsWith("tool-");
}

function truncateOutput(text: string, maxLines = 30): string {
  const lines = text.split("\n");
  if (lines.length <= maxLines) return text;
  return lines.slice(0, maxLines).join("\n") + `\n... (${lines.length - maxLines} more lines)`;
}

function parseOutputObject(raw: unknown): Record<string, unknown> | null {
  if (typeof raw === "string") {
    try {
      const parsed = JSON.parse(raw);
      if (typeof parsed === "object" && parsed !== null) return parsed;
    } catch { /* not JSON */ }
    return null;
  }
  if (typeof raw === "object" && raw !== null) return raw as Record<string, unknown>;
  return null;
}

function formatOutput(raw: unknown): string | null {
  if (typeof raw === "string") {
    if (!raw.trim()) return null;
    const obj = parseOutputObject(raw);
    if (obj) {
      if (typeof obj.text === "string" && obj.image) return obj.text as string;
      const { image: _, ...rest } = obj;
      return JSON.stringify(rest, null, 2);
    }
    return raw;
  }
  if (typeof raw === "object" && raw !== null) {
    const r = raw as Record<string, unknown>;
    if (typeof r.text === "string" && r.image) return r.text as string;
    const { image: _, ...rest } = r;
    return JSON.stringify(rest, null, 2);
  }
  return null;
}

function extractImageUrl(raw: unknown): string | null {
  const obj = parseOutputObject(raw);
  if (!obj) return null;
  const img = obj.image;
  if (typeof img === "string" && img.startsWith("data:image/")) return img;
  return null;
}

function ToolCallBlock({ part, onImageLoad }: { part: ToolInvocationPart; onImageLoad?: () => void }) {
  const [expanded, setExpanded] = useState(false);
  const toolName = part.type.split("-").slice(1).join("-");
  const command = (part.input as { command?: string })?.command ?? toolName;
  const isDone = part.state === "output-available";
  const isRunning = !isDone;
  const output = isDone ? formatOutput(part.output) : null;
  const hasOutput = !!output;
  const imageUrl = isDone ? extractImageUrl(part.output) : null;
  const canExpand = hasOutput && !isRunning;

  return (
    <div className="space-y-1.5">
      <div
        className={cn(
          "rounded-md text-[10px] font-mono overflow-hidden border border-border",
          canExpand && "cursor-pointer",
        )}
        onClick={() => canExpand && setExpanded(!expanded)}
      >
        <div className={cn(
          "px-2 py-1 flex items-center gap-2",
          expanded && hasOutput ? "border-b border-border bg-secondary/30" : "bg-secondary/30",
        )}>
          {isRunning ? (
            <Loader className="size-3 shrink-0 animate-spin text-muted-foreground" />
          ) : (
            <ChevronRight
              className={cn(
                "size-3 shrink-0 text-muted-foreground transition-transform duration-200",
                expanded && "rotate-90",
              )}
            />
          )}
          <span className={cn(
            "truncate",
            isRunning ? "text-foreground/80 shimmer-text" : "text-foreground/80",
          )}>{command}</span>
        </div>
        {expanded && hasOutput && (
          <div className="max-h-[300px] overflow-y-auto">
            <pre className="px-2 py-1.5 text-foreground/80 whitespace-pre-wrap break-all leading-relaxed">
              {truncateOutput(output)}
            </pre>
          </div>
        )}
      </div>
      {imageUrl && (
        <img
          src={imageUrl}
          alt="Screenshot"
          className="rounded-md border border-border max-w-full"
          onLoad={onImageLoad}
        />
      )}
    </div>
  );
}

const DEFAULT_CONTEXT_WINDOW = 128000;

function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

function formatTokenCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return `${n}`;
}

function ContextMeter({ used, total }: { used: number; total: number }) {
  const ratio = Math.min(used / total, 1);
  const size = 16;
  const strokeWidth = 2;
  const r = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * r;
  const offset = circumference * (1 - ratio);
  const color =
    ratio > 0.9 ? "text-destructive" : ratio > 0.7 ? "text-yellow-500" : "text-muted-foreground/50";

  return (
    <div
      className="relative shrink-0"
      title={`${formatTokenCount(used)} / ${formatTokenCount(total)} tokens`}
    >
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`}>
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          className="text-border"
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          strokeLinecap="round"
          className={cn(color, "transition-[stroke-dashoffset] duration-300")}
          transform={`rotate(-90 ${size / 2} ${size / 2})`}
        />
      </svg>
    </div>
  );
}

const DEFAULT_MODEL = "anthropic/claude-sonnet-4.6";

function useTimeAgo(ts: number | undefined) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!ts) return;
    const id = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, [ts]);
  if (!ts) return "";
  const diff = Math.floor((Date.now() - ts) / 1000);
  if (diff < 5) return "just now";
  if (diff < 60) return `${diff}s ago`;
  const mins = Math.floor(diff / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  return `${hrs}h ago`;
}

function MessageFooter({ model, timestamp, text }: { model: string; timestamp?: number; text: string }) {
  const [copied, setCopied] = useState(false);
  const timeAgo = useTimeAgo(timestamp);
  const shortModel = model.includes("/") ? model.split("/").pop()! : model;

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [text]);

  return (
    <div className="flex items-center gap-2 pt-0.5 text-[10px] text-muted-foreground/50">
      <span>{shortModel}</span>
      {timeAgo && (
        <>
          <span>·</span>
          <span>{timeAgo}</span>
        </>
      )}
      <button
        type="button"
        onClick={handleCopy}
        className="ml-auto hover:text-muted-foreground transition-colors"
        aria-label="Copy message"
      >
        {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
      </button>
    </div>
  );
}

interface PendingImage {
  file: File;
  preview: string;
}

export function ChatPanel() {
  const [input, setInput] = useState("");
  const [errorDismissed, setErrorDismissed] = useState(false);
  const [pendingImages, setPendingImages] = useState<PendingImage[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const defaultModel = useAtomValue(chatModelAtom);
  const [selectedModel, setSelectedModel] = useState<string>(defaultModel || DEFAULT_MODEL);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const sessionName = useAtomValue(activeSessionNameAtom);
  const chatId = sessionName || "default";
  const storageKey = `${STORAGE_PREFIX}${chatId}`;
  const sessionRef = useRef(chatId);
  sessionRef.current = chatId;
  const modelRef = useRef(selectedModel);
  modelRef.current = selectedModel;
  const messageTimestamps = useRef<Record<string, number>>({});

  useEffect(() => {
    if (defaultModel) setSelectedModel(defaultModel);
  }, [defaultModel]);

  const transport = useRef(
    new DefaultChatTransport({
      api: getChatApiUrl(),
      body: () => ({
        session: sessionRef.current,
        model: modelRef.current,
      }),
    }),
  ).current;

  const { messages, sendMessage, stop, status, setMessages, error } = useChat({
    id: chatId,
    transport,
    onError: () => setErrorDismissed(false),
  });

  const visibleError = error && !errorDismissed ? error : undefined;
  const isLoading = status === "streaming" || status === "submitted";
  const hasMessages = messages.length > 0 || !!visibleError;

  useEffect(() => {
    for (const msg of messages) {
      if (msg.role === "assistant" && !messageTimestamps.current[msg.id]) {
        messageTimestamps.current[msg.id] = Date.now();
      }
    }
  }, [messages]);

  const models = useAtomValue(availableModelsAtom);
  const estimatedTokens = useMemo(() => {
    let total = 0;
    for (const msg of messages) {
      for (const part of msg.parts) {
        if (part.type === "text") total += estimateTokens(part.text);
        else if (isToolPart(part)) {
          if (part.input) total += estimateTokens(JSON.stringify(part.input));
          if (part.output) {
            const raw = typeof part.output === "string" ? part.output : JSON.stringify(part.output);
            const stripped = raw.replace(/"image"\s*:\s*"data:[^"]*"/g, '"image":"[omitted]"');
            total += estimateTokens(stripped);
          }
        }
      }
    }
    return total;
  }, [messages]);
  const contextWindow = useMemo(() => {
    const match = models.find((m) => m.id === selectedModel);
    return match?.context_window ?? DEFAULT_CONTEXT_WINDOW;
  }, [models, selectedModel]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const scrollToBottom = useCallback(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, []);

  useEffect(() => {
    scrollToBottom();
  }, [messages, visibleError, scrollToBottom]);

  // Restore messages from localStorage when chatId changes
  useEffect(() => {
    try {
      const stored = localStorage.getItem(storageKey);
      if (stored) {
        const parsed = JSON.parse(stored);
        if (Array.isArray(parsed) && parsed.length > 0) {
          setMessages(parsed);
          return;
        }
      }
    } catch {
      // ignore
    }
    setMessages([]);
  }, [chatId, storageKey, setMessages]);

  // Persist messages to localStorage (strip base64 images to save space)
  useEffect(() => {
    if (isLoading) return;
    if (messages.length === 0) {
      localStorage.removeItem(storageKey);
      return;
    }
    try {
      localStorage.setItem(storageKey, JSON.stringify(stripImagesForStorage(messages)));
    } catch {
      // ignore quota
    }
  }, [messages, isLoading, storageKey]);

  const addImages = useCallback((files: FileList | null) => {
    if (!files) return;
    const images = Array.from(files).filter((f) => f.type.startsWith("image/"));
    setPendingImages((prev) => [
      ...prev,
      ...images.map((file) => ({ file, preview: URL.createObjectURL(file) })),
    ]);
  }, []);

  const removeImage = useCallback((index: number) => {
    setPendingImages((prev) => {
      const next = [...prev];
      URL.revokeObjectURL(next[index].preview);
      next.splice(index, 1);
      return next;
    });
  }, []);

  const handleSubmit = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault();
      if ((!input.trim() && pendingImages.length === 0) || isLoading) return;
      const dt = new DataTransfer();
      for (const img of pendingImages) dt.items.add(img.file);
      const files = dt.files.length > 0 ? dt.files : undefined;
      sendMessage({ text: input, files });
      setInput("");
      setPendingImages((prev) => {
        for (const p of prev) URL.revokeObjectURL(p.preview);
        return [];
      });
    },
    [input, isLoading, sendMessage, pendingImages],
  );

  const lastCompactedId = useRef<string | null>(null);
  useEffect(() => {
    if (isLoading || messages.length === 0) return;
    const lastAssistant = [...messages].reverse().find((m) => m.role === "assistant");
    if (!lastAssistant) return;
    if (lastAssistant.id === lastCompactedId.current) return;
    const meta = (lastAssistant as any).metadata as
      | { compacted?: boolean; summary?: string; keepLastN?: number }
      | undefined;
    if (!meta?.compacted || typeof meta.keepLastN !== "number") return;

    lastCompactedId.current = lastAssistant.id;
    const keep = meta.keepLastN;
    if (keep >= messages.length) return;

    const summaryMsg = {
      id: `compaction-${Date.now()}`,
      role: "assistant" as const,
      parts: [
        {
          type: "text" as const,
          text: `*Earlier messages were summarized to stay within the context window.*`,
        },
      ],
    };

    const kept = messages.slice(messages.length - keep);
    setMessages([summaryMsg as any, ...kept]);
  }, [isLoading, messages, setMessages]);

  const handleClear = useCallback(() => {
    setMessages([]);
    setErrorDismissed(true);
    localStorage.removeItem(storageKey);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [setMessages, storageKey]);

  const handleDownload = useCallback(() => {
    const data = messages.map((msg) => ({
      id: msg.id,
      role: msg.role,
      parts: msg.parts.map((p) => {
        if (p.type === "text") return { type: "text", text: p.text };
        if (p.type === "file") return { type: "file", filename: (p as any).filename };
        if (isToolPart(p)) {
          const out = typeof p.output === "string" ? p.output : JSON.stringify(p.output);
          const stripped = out?.replace(/"image":"data:[^"]*"/g, '"image":"[stripped]"');
          return {
            type: p.type,
            toolName: (p as any).toolName,
            state: (p as any).state,
            input: (p as any).input,
            output: stripped,
          };
        }
        return { type: p.type };
      }),
    }));
    const json = JSON.stringify({ session: chatId, model: selectedModel, messages: data }, null, 2);
    const blob = new Blob([json], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `chat-${chatId}-${Date.now()}.json`;
    a.click();
    URL.revokeObjectURL(url);
  }, [messages, chatId, selectedModel]);

  const hasVisibleContent = (parts: (typeof messages)[number]["parts"]): boolean => {
    return parts.some(
      (p) => (p.type === "text" && p.text.length > 0) || p.type === "file" || isToolPart(p),
    );
  };

  return (
    <div className="flex h-full flex-col">
      {hasMessages && (
        <div className="flex items-center justify-end gap-2 px-3 py-1.5 shrink-0 border-b border-border/40">
          <button
            onClick={handleDownload}
            className="text-muted-foreground hover:text-foreground transition-colors shrink-0"
            aria-label="Download conversation"
          >
            <Download className="size-3" />
          </button>
          <button
            onClick={handleClear}
            className="text-muted-foreground hover:text-foreground transition-colors shrink-0"
            aria-label="Clear conversation"
          >
            <Trash2 className="size-3" />
          </button>
        </div>
      )}

      <ScrollArea className="flex-1 min-h-0">
        <div className="p-3 space-y-3">
          {!hasMessages && !isLoading && (
            <div className="space-y-2 pt-2">
              <p className="text-[11px] text-muted-foreground">
                Control the browser with natural language:
              </p>
              <div className="flex flex-wrap gap-1.5">
                {SUGGESTIONS.map((s) => (
                  <button
                    key={s}
                    type="button"
                    onClick={() => sendMessage({ text: s })}
                    className="text-[10px] px-2 py-1 rounded-md border bg-secondary/50 text-muted-foreground hover:text-foreground hover:bg-secondary transition-colors"
                  >
                    {s}
                  </button>
                ))}
              </div>
            </div>
          )}

          {messages.map((message) => {
            if (message.id.startsWith("compaction-")) {
              return (
                <div key={message.id} className="flex items-center gap-2 text-[10px] text-muted-foreground/60">
                  <div className="flex-1 border-t border-border/40" />
                  <span>Earlier messages summarized</span>
                  <div className="flex-1 border-t border-border/40" />
                </div>
              );
            }
            if (!hasVisibleContent(message.parts)) return null;
            return (
              <div key={message.id}>
                {message.role === "user" ? (
                  <div className="space-y-1.5">
                    {message.parts.some((p) => p.type === "file") && (
                      <div className="flex flex-wrap gap-1.5">
                        {message.parts
                          .filter((p): p is Extract<typeof p, { type: "file" }> => p.type === "file")
                          .map((p, i) => (
                            <img
                              key={i}
                              src={p.url}
                              alt={p.filename ?? "uploaded image"}
                              className="max-h-24 rounded-md border border-border object-cover"
                            />
                          ))}
                      </div>
                    )}
                    <div className="text-xs text-muted-foreground whitespace-pre-wrap leading-relaxed">
                      {message.parts
                        .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text")
                        .map((p) => p.text)
                        .join("")}
                    </div>
                  </div>
                ) : (
                  <div className="space-y-1.5">
                    {(() => {
                      type Group = { type: "tools" | "text"; items: (typeof message.parts)[number][] };
                      const groups: Group[] = [];
                      for (const part of message.parts) {
                        const groupType = isToolPart(part) ? "tools" : "text";
                        const last = groups[groups.length - 1];
                        if (last && last.type === groupType) {
                          last.items.push(part);
                        } else {
                          groups.push({ type: groupType, items: [part] });
                        }
                      }

                      return groups.map((group, gi) => {
                        if (group.type === "tools") {
                          return (
                            <div key={gi} className="space-y-0.5">
                              {group.items.map((part) => {
                                if (!isToolPart(part)) return null;
                                return <ToolCallBlock key={part.toolCallId} part={part} onImageLoad={scrollToBottom} />;
                              })}
                            </div>
                          );
                        }
                        const combinedText = group.items
                          .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text" && !!p.text)
                          .map((p) => p.text)
                          .join("");
                        if (!combinedText) return null;
                        return (
                          <div key={gi} className="text-xs text-foreground">
                            <Streamdown
                              shikiTheme={shikiTheme}
                              controls={false}
                              components={chatComponents}
                            >
                              {combinedText}
                            </Streamdown>
                          </div>
                        );
                      });
                    })()}
                    {(() => {
                      const isLast = message === messages[messages.length - 1];
                      const isComplete = !isLast || !isLoading;
                      if (!isComplete) return null;
                      const fullText = message.parts
                        .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text" && !!p.text)
                        .map((p) => p.text)
                        .join("");
                      return (
                        <MessageFooter
                          model={selectedModel}
                          timestamp={messageTimestamps.current[message.id]}
                          text={fullText}
                        />
                      );
                    })()}
                  </div>
                )}
              </div>
            );
          })}

          {isLoading && messages.length > 0 && (() => {
            const lastMsg = messages[messages.length - 1];
            const lastPart = lastMsg?.parts[lastMsg.parts.length - 1];
            const noVisibleContent = !lastMsg || !hasVisibleContent(lastMsg.parts);
            const lastIsCompletedTool = lastPart && isToolPart(lastPart) && lastPart.state === "output-available";
            if (noVisibleContent || lastIsCompletedTool) {
              return (
                <span className="text-[11px] text-muted-foreground shimmer-text">
                  Working...
                </span>
              );
            }
            return null;
          })()}

          {visibleError && (
            <div className="text-[10px] text-destructive/80 bg-destructive/10 rounded-md px-2 py-1.5">
              {(() => {
                try {
                  const parsed = JSON.parse(visibleError.message);
                  return parsed.message || parsed.error || visibleError.message;
                } catch {
                  return visibleError.message || "Something went wrong.";
                }
              })()}
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>
      </ScrollArea>

      <div className="shrink-0 border-t border-border">
        <form onSubmit={handleSubmit}>
          {pendingImages.length > 0 && (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2">
              {pendingImages.map((img, i) => (
                <div key={img.preview} className="group relative">
                  <img
                    src={img.preview}
                    alt={img.file.name}
                    className="h-14 rounded-md border border-border object-cover"
                  />
                  <button
                    type="button"
                    onClick={() => removeImage(i)}
                    className="absolute -top-1.5 -right-1.5 hidden group-hover:flex size-4 items-center justify-center rounded-full bg-background border border-border text-muted-foreground hover:text-foreground"
                  >
                    <X className="size-2.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="px-3 pt-2 pb-1.5">
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                e.target.style.height = "auto";
                e.target.style.height = `${e.target.scrollHeight}px`;
              }}
              rows={1}
              placeholder="Ask something..."
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  handleSubmit(e);
                }
              }}
              onPaste={(e) => {
                const items = e.clipboardData?.items;
                if (!items) return;
                const imageFiles: File[] = [];
                for (const item of items) {
                  if (item.type.startsWith("image/")) {
                    const file = item.getAsFile();
                    if (file) imageFiles.push(file);
                  }
                }
                if (imageFiles.length > 0) {
                  const dt = new DataTransfer();
                  for (const f of imageFiles) dt.items.add(f);
                  addImages(dt.files);
                }
              }}
              className="w-full bg-transparent text-xs text-foreground outline-none resize-none max-h-24 leading-relaxed placeholder:text-muted-foreground"
            />
          </div>
          <div className="flex items-center justify-between px-3 pb-2">
            <ModelSelector value={selectedModel} onChange={setSelectedModel} />
            <div className="flex items-center gap-2">
              {hasMessages && (
                <ContextMeter used={estimatedTokens} total={contextWindow} />
              )}
              <input
                ref={fileInputRef}
                type="file"
                accept="image/*"
                multiple
                className="hidden"
                onChange={(e) => {
                  addImages(e.target.files);
                  e.target.value = "";
                }}
              />
              <button
                type="button"
                onClick={() => fileInputRef.current?.click()}
                className="text-muted-foreground hover:text-foreground transition-colors shrink-0 p-1"
                aria-label="Attach image"
              >
                <ImagePlus className="size-3.5" />
              </button>
              {isLoading ? (
                <button
                  type="button"
                  onClick={() => stop()}
                  className="bg-primary text-primary-foreground rounded-full p-1 hover:bg-primary/90 transition-colors shrink-0"
                  aria-label="Stop"
                >
                  <Square className="size-3 fill-current" />
                </button>
              ) : (
                <button
                  type="submit"
                  disabled={!input.trim() && pendingImages.length === 0}
                  className="bg-primary text-primary-foreground rounded-full p-1 hover:bg-primary/90 transition-colors disabled:opacity-30 shrink-0"
                  aria-label="Send message"
                >
                  <ArrowUp className="size-3" />
                </button>
              )}
            </div>
          </div>
        </form>
      </div>
    </div>
  );
}
