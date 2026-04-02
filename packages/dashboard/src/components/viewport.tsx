"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import { ArrowLeft, ArrowRight, Camera, Circle, FileCode, Maximize, Moon, RotateCw, Smartphone, Square, Sun, Wifi, WifiOff } from "lucide-react";
import { cn } from "@/lib/utils";
import { execCommand, sessionArgs } from "@/lib/exec";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import {
  currentFrameAtom,
  viewportWidthAtom,
  viewportHeightAtom,
  browserConnectedAtom,
  screencastingAtom,
  recordingAtom,
  streamEngineAtom,
  activeUrlAtom,
  sendInputAtom,
} from "@/store/stream";
import { activeSessionNameAtom, activePortAtom } from "@/store/sessions";

const SCREENCAST_ENGINES = new Set(["chrome"]);

function cdpModifiers(e: React.MouseEvent | React.WheelEvent): number {
  let m = 0;
  if (e.altKey) m |= 1;
  if (e.ctrlKey) m |= 2;
  if (e.metaKey) m |= 4;
  if (e.shiftKey) m |= 8;
  return m;
}

const KEY_INFO: Record<string, { text?: string; keyCode: number }> = {
  Enter: { text: "\r", keyCode: 13 },
  Tab: { text: "\t", keyCode: 9 },
  Backspace: { text: "\b", keyCode: 8 },
  Escape: { keyCode: 27 },
  ArrowLeft: { keyCode: 37 },
  ArrowUp: { keyCode: 38 },
  ArrowRight: { keyCode: 39 },
  ArrowDown: { keyCode: 40 },
  Delete: { keyCode: 46 },
  Home: { keyCode: 36 },
  End: { keyCode: 35 },
  PageUp: { keyCode: 33 },
  PageDown: { keyCode: 34 },
};

function cdpButton(btn: number): string {
  switch (btn) {
    case 0: return "left";
    case 1: return "middle";
    case 2: return "right";
    default: return "none";
  }
}

const DIMENSION_PRESETS: { label: string; ratio?: [number, number] }[] = [
  { label: "1:1", ratio: [1, 1] },
  { label: "4:3", ratio: [4, 3] },
  { label: "16:9", ratio: [16, 9] },
  { label: "9:16", ratio: [9, 16] },
  { label: "21:9", ratio: [21, 9] },
];

const DEVICE_PRESETS = [
  { label: "iPhone 15", value: "iPhone 15" },
  { label: "iPhone 16", value: "iPhone 16" },
  { label: "iPhone 16 Pro", value: "iPhone 16 Pro" },
  { label: "iPhone 17", value: "iPhone 17" },
  { label: "iPad", value: "iPad" },
  { label: "iPad Pro", value: "iPad Pro" },
  { label: "Pixel 9", value: "Pixel 9" },
  { label: "Galaxy S25", value: "Galaxy S25" },
];

type ColorScheme = "light" | "dark" | "no-preference";

function computePresetSize(
  ratio: [number, number],
  availableWidth: number,
  availableHeight: number,
): { w: number; h: number } {
  const [rw, rh] = ratio;
  let w = availableWidth;
  let h = Math.round(w * rh / rw);
  if (h > availableHeight) {
    h = availableHeight;
    w = Math.round(h * rw / rh);
  }
  return { w: Math.max(w, 1), h: Math.max(h, 1) };
}

function normalizeUrl(input: string): string {
  const trimmed = input.trim();
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  return `https://${trimmed}`;
}

export function Viewport() {
  const frame = useAtomValue(currentFrameAtom);
  const viewportWidth = useAtomValue(viewportWidthAtom);
  const viewportHeight = useAtomValue(viewportHeightAtom);
  const browserConnected = useAtomValue(browserConnectedAtom);
  const screencasting = useAtomValue(screencastingAtom);
  const recording = useAtomValue(recordingAtom);
  const engine = useAtomValue(streamEngineAtom);
  const url = useAtomValue(activeUrlAtom);
  const sessionName = useAtomValue(activeSessionNameAtom);
  const streamPort = useAtomValue(activePortAtom);
  const sendInput = useSetAtom(sendInputAtom);

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasAreaRef = useRef<HTMLDivElement>(null);
  const addressRef = useRef<HTMLInputElement>(null);
  const [addressValue, setAddressValue] = useState(url);
  const [navigating, setNavigating] = useState(false);
  const [canvasArea, setCanvasArea] = useState({ width: 0, height: 0 });
  const [customDialogOpen, setCustomDialogOpen] = useState(false);
  const [customValue, setCustomValue] = useState("");
  const customInputRef = useRef<HTMLInputElement>(null);
  const [recordDialogOpen, setRecordDialogOpen] = useState(false);
  const [recordPath, setRecordPath] = useState("recording.webm");
  const recordInputRef = useRef<HTMLInputElement>(null);
  const [activeDevice, setActiveDevice] = useState<string | null>(null);
  const [colorScheme, setColorScheme] = useState<ColorScheme>("no-preference");
  const [offline, setOffline] = useState(false);

  useEffect(() => {
    if (customDialogOpen) {
      requestAnimationFrame(() => customInputRef.current?.select());
    }
  }, [customDialogOpen]);

  useEffect(() => {
    if (recordDialogOpen) {
      requestAnimationFrame(() => recordInputRef.current?.select());
    }
  }, [recordDialogOpen]);

  useEffect(() => {
    setAddressValue(url);
  }, [url]);

  useEffect(() => {
    setActiveDevice(null);
    setColorScheme("no-preference");
    setOffline(false);
  }, [sessionName]);

  useEffect(() => {
    const el = canvasAreaRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      setCanvasArea({ width: Math.floor(width), height: Math.floor(height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const runCmd = useCallback(
    (...args: string[]) => execCommand(sessionArgs(sessionName, ...args)),
    [sessionName],
  );

  const handleNavigate = useCallback(async () => {
    if (!addressValue.trim() || navigating) return;

    addressRef.current?.blur();

    const target = normalizeUrl(addressValue);
    const previousUrl = url;
    setAddressValue(target);
    setNavigating(true);
    try {
      const result = await runCmd("navigate", target);
      if (!result.success) {
        setAddressValue(previousUrl || "about:blank");
      }
    } catch {
      setAddressValue(previousUrl || "about:blank");
    } finally {
      setNavigating(false);
    }
  }, [addressValue, navigating, runCmd, url]);

  const drawFrame = useCallback((base64: string) => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const bin = atob(base64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);

    createImageBitmap(new Blob([bytes], { type: "image/jpeg" })).then((bmp) => {
      canvas.width = bmp.width;
      canvas.height = bmp.height;
      const ctx = canvas.getContext("2d");
      if (ctx) ctx.drawImage(bmp, 0, 0);
      bmp.close();
    });
  }, []);

  useEffect(() => {
    if (frame) {
      drawFrame(frame);
    }
  }, [frame, drawFrame]);

  const isFit =
    canvasArea.width > 0 &&
    viewportWidth === canvasArea.width &&
    viewportHeight === canvasArea.height;

  const handleFit = useCallback(() => {
    if (canvasArea.width <= 0) return;
    const w = canvasArea.width;
    const h = canvasArea.height;
    if (w > 0 && h > 0) {
      runCmd("set", "viewport", String(w), String(h));
    }
  }, [canvasArea, runCmd]);

  const handlePreset = useCallback(
    (ratio: [number, number]) => {
      if (canvasArea.width <= 0) return;
      const avail = { w: canvasArea.width, h: canvasArea.height };
      const { w, h } = computePresetSize(ratio, avail.w, avail.h);
      runCmd("set", "viewport", String(w), String(h));
    },
    [canvasArea, runCmd],
  );

  const submitCustomDimensions = useCallback(() => {
    setCustomDialogOpen(false);
    const match = customValue.trim().match(/^(\d+)\s*[x,\s]\s*(\d+)$/);
    if (!match) return;
    const w = parseInt(match[1], 10);
    const h = parseInt(match[2], 10);
    if (w > 0 && h > 0) {
      runCmd("set", "viewport", String(w), String(h));
    }
  }, [customValue, runCmd]);

  const handleRecordStart = useCallback(async () => {
    const path = recordPath.trim();
    if (!path) return;
    setRecordDialogOpen(false);
    await execCommand(sessionArgs(sessionName, "record", "start", path));
  }, [recordPath, sessionName]);

  const handleRecordStop = useCallback(async () => {
    await execCommand(sessionArgs(sessionName, "record", "stop"));
  }, [sessionName]);

  const handleSetDevice = useCallback(async (device: string) => {
    setActiveDevice(device);
    await execCommand(sessionArgs(sessionName, "set", "device", device));
  }, [sessionName]);

  const handleResetDevice = useCallback(async () => {
    setActiveDevice(null);
    if (canvasArea.width > 0 && canvasArea.height > 0) {
      await runCmd("set", "viewport", String(canvasArea.width), String(canvasArea.height));
    }
  }, [canvasArea.width, canvasArea.height, runCmd]);

  const handleSetColorScheme = useCallback(async (scheme: ColorScheme) => {
    setColorScheme(scheme);
    const args = sessionArgs(sessionName, "set", "media");
    if (scheme !== "no-preference") args.push(scheme);
    await execCommand(args);
  }, [sessionName]);

  const handleToggleOffline = useCallback(async () => {
    const next = !offline;
    setOffline(next);
    const args = sessionArgs(sessionName, "set", "offline");
    if (!next) args.push("off");
    await execCommand(args);
  }, [offline, sessionName]);

  const toViewport = useCallback(
    (e: React.MouseEvent): { x: number; y: number } | null => {
      const canvas = canvasRef.current;
      if (!canvas) return null;
      const rect = canvas.getBoundingClientRect();
      const scaleX = viewportWidth / rect.width;
      const scaleY = viewportHeight / rect.height;
      return {
        x: Math.round((e.clientX - rect.left) * scaleX),
        y: Math.round((e.clientY - rect.top) * scaleY),
      };
    },
    [viewportWidth, viewportHeight],
  );

  const handleMouseEvent = useCallback(
    (e: React.MouseEvent, eventType: string) => {
      const pos = toViewport(e);
      if (!pos) return;
      sendInput({
        type: "input_mouse",
        eventType,
        x: pos.x,
        y: pos.y,
        button: cdpButton(e.button),
        clickCount: eventType === "mousePressed" ? 1 : 0,
        modifiers: cdpModifiers(e),
      });
    },
    [toViewport, sendInput],
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent) => {
      const pos = toViewport(e);
      if (!pos) return;
      sendInput({
        type: "input_mouse",
        eventType: "mouseWheel",
        x: pos.x,
        y: pos.y,
        button: "none",
        clickCount: 0,
        deltaX: e.deltaX,
        deltaY: e.deltaY,
        modifiers: cdpModifiers(e),
      });
    },
    [toViewport, sendInput],
  );

  const dispatchKey = useCallback(
    (e: KeyboardEvent, eventType: string) => {
      const info = KEY_INFO[e.key];
      const text = eventType === "keyDown"
        ? (info?.text ?? (e.key.length === 1 ? e.key : undefined))
        : undefined;
      const keyCode = info?.keyCode ?? (e.key.length === 1 ? e.key.charCodeAt(0) : 0);
      let m = 0;
      if (e.altKey) m |= 1;
      if (e.ctrlKey) m |= 2;
      if (e.metaKey) m |= 4;
      if (e.shiftKey) m |= 8;
      sendInput({
        type: "input_keyboard",
        eventType,
        key: e.key,
        code: e.code,
        text,
        windowsVirtualKeyCode: keyCode,
        modifiers: m,
      });
    },
    [sendInput],
  );

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement !== canvasRef.current) return;
      e.preventDefault();
      e.stopPropagation();
      dispatchKey(e, e.type === "keydown" ? "keyDown" : "keyUp");
    };
    window.addEventListener("keydown", handler, true);
    window.addEventListener("keyup", handler, true);
    return () => {
      window.removeEventListener("keydown", handler, true);
      window.removeEventListener("keyup", handler, true);
    };
  }, [dispatchKey]);

  return (
    <div ref={containerRef} className="flex h-full flex-col">
      {browserConnected && (
        <>
          <div className="flex shrink-0 items-center gap-1.5 px-2 py-1.5">
            <TooltipProvider delayDuration={300}>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => runCmd("back")}
                    className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <ArrowLeft className="size-4" />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>Back</p></TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => runCmd("forward")}
                    className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <ArrowRight className="size-4" />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>Forward</p></TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => runCmd("reload")}
                    className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <RotateCw className="size-3.5" />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>Refresh</p></TooltipContent>
              </Tooltip>
            </TooltipProvider>
            <div className="flex min-w-0 flex-1 items-center rounded-md bg-muted px-2.5 py-1">
              <input
                ref={addressRef}
                type="text"
                value={addressValue}
                onChange={(e) => setAddressValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    handleNavigate();
                  }
                }}
                className={cn(
                  "w-full bg-transparent font-mono text-xs text-muted-foreground outline-none placeholder:text-muted-foreground/50",
                  navigating && "opacity-50",
                )}
                placeholder="Enter URL..."
                spellCheck={false}
              />
            </div>
            <TooltipProvider delayDuration={300}>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => runCmd("snapshot")}
                    className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <FileCode className="size-4" />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>Snapshot</p></TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => runCmd("screenshot")}
                    className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <Camera className="size-4" />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>Screenshot</p></TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={recording ? handleRecordStop : () => setRecordDialogOpen(true)}
                    className={cn(
                      "shrink-0 rounded p-1 transition-colors",
                      recording
                        ? "text-destructive hover:bg-destructive/10"
                        : "text-muted-foreground hover:bg-muted hover:text-foreground",
                    )}
                  >
                    {recording ? <Square className="size-3.5" /> : <Circle className="size-4" />}
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom"><p>{recording ? "Stop recording" : "Record"}</p></TooltipContent>
              </Tooltip>
            </TooltipProvider>
          </div>
          <Separator />
        </>
      )}

      <div ref={canvasAreaRef} className="flex min-h-0 flex-1 items-center justify-center">
        {frame ? (
          <canvas
            ref={canvasRef}
            tabIndex={0}
            className="max-h-full max-w-full object-contain outline-none"
            onMouseMove={(e) => handleMouseEvent(e, "mouseMoved")}
            onMouseDown={(e) => {
              canvasRef.current?.focus();
              handleMouseEvent(e, "mousePressed");
            }}
            onMouseUp={(e) => handleMouseEvent(e, "mouseReleased")}
            onWheel={handleWheel}
            onContextMenu={(e) => e.preventDefault()}
          />
        ) : (
          <div className="text-center text-sm text-muted-foreground">
            {browserConnected
              ? SCREENCAST_ENGINES.has(engine)
                ? "Waiting for frames..."
                : `Screencast not available for ${engine}`
              : "No browser connected"}
          </div>
        )}
      </div>

      <Separator />
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <div
          className={cn(
            "size-2 rounded-full",
            browserConnected ? "bg-success" : "bg-destructive",
          )}
        />
        <span className="text-xs text-muted-foreground">
          {browserConnected
            ? screencasting
              ? "Live"
              : "Connected"
            : "Disconnected"}
        </span>
        {browserConnected && (
          <span className="text-xs text-muted-foreground/60 font-mono">
            ws://localhost:{streamPort}
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          {browserConnected && (
            <>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Badge
                    variant="outline"
                    className="flex h-4 cursor-pointer items-center gap-1 px-1.5 text-[10px] hover:bg-muted"
                  >
                    {colorScheme === "dark" ? (
                      <Moon className="size-2.5" />
                    ) : (
                      <Sun className="size-2.5" />
                    )}
                    {colorScheme === "dark" ? "Dark" : colorScheme === "light" ? "Light" : "System"}
                  </Badge>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" side="top">
                  <DropdownMenuLabel className="text-xs">Color Scheme</DropdownMenuLabel>
                  <DropdownMenuItem
                    onClick={() => handleSetColorScheme("no-preference")}
                    className={cn("text-xs", colorScheme === "no-preference" && "font-semibold")}
                  >
                    System
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => handleSetColorScheme("light")}
                    className={cn("text-xs", colorScheme === "light" && "font-semibold")}
                  >
                    <Sun className="mr-1.5 size-3" />
                    Light
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => handleSetColorScheme("dark")}
                    className={cn("text-xs", colorScheme === "dark" && "font-semibold")}
                  >
                    <Moon className="mr-1.5 size-3" />
                    Dark
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>

              <Badge
                variant="outline"
                onClick={handleToggleOffline}
                className={cn(
                  "flex h-4 cursor-pointer items-center gap-1 px-1.5 text-[10px] hover:bg-muted",
                  offline && "border-destructive/50 bg-destructive/10 text-destructive hover:bg-destructive/20",
                )}
              >
                {offline ? <WifiOff className="size-2.5" /> : <Wifi className="size-2.5" />}
                {offline ? "Offline" : "Online"}
              </Badge>
            </>
          )}

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Badge
                variant="outline"
                className="flex h-4 cursor-pointer items-center gap-1 px-1.5 text-[10px] tabular-nums hover:bg-muted"
              >
                {activeDevice && <Smartphone className="size-2.5" />}
                {activeDevice ?? `${viewportWidth} x ${viewportHeight}`}
              </Badge>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" side="top">
              <DropdownMenuLabel className="text-xs">Aspect Ratio</DropdownMenuLabel>
              {DIMENSION_PRESETS.map((p) => (
                <DropdownMenuItem
                  key={p.label}
                  onClick={() => handlePreset(p.ratio!)}
                  className="text-xs"
                >
                  {p.label}
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuLabel className="text-xs">Devices</DropdownMenuLabel>
              {DEVICE_PRESETS.map((d) => (
                <DropdownMenuItem
                  key={d.value}
                  onClick={() => handleSetDevice(d.value)}
                  className={cn("text-xs", activeDevice === d.value && "font-semibold")}
                >
                  <Smartphone className="mr-1.5 size-3" />
                  {d.label}
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              {!isFit && (
                <DropdownMenuItem onClick={handleFit} className="text-xs">
                  <Maximize className="mr-1.5 size-3" />
                  Fit
                </DropdownMenuItem>
              )}
              <DropdownMenuItem
                onClick={() => {
                  setCustomValue(`${viewportWidth} x ${viewportHeight}`);
                  setCustomDialogOpen(true);
                }}
                className="text-xs"
              >
                Custom...
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      <Dialog open={customDialogOpen} onOpenChange={setCustomDialogOpen}>
        <DialogContent className="max-w-xs">
          <DialogHeader>
            <DialogTitle>Custom Dimensions</DialogTitle>
          </DialogHeader>
          <input
            ref={customInputRef}
            type="text"
            value={customValue}
            onChange={(e) => setCustomValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                submitCustomDimensions();
              }
            }}
            placeholder="1280 x 720"
            className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm outline-none focus:ring-1 focus:ring-ring"
          />
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setCustomDialogOpen(false)}>
              Cancel
            </Button>
            <Button size="sm" onClick={submitCustomDimensions}>
              Apply
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={recordDialogOpen} onOpenChange={setRecordDialogOpen}>
        <DialogContent className="max-w-xs">
          <DialogHeader>
            <DialogTitle>Start Recording</DialogTitle>
          </DialogHeader>
          <input
            ref={recordInputRef}
            type="text"
            value={recordPath}
            onChange={(e) => setRecordPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                handleRecordStart();
              }
            }}
            placeholder="recording.webm"
            className="h-9 w-full rounded-md border border-input bg-transparent px-3 font-mono text-sm outline-none focus:ring-1 focus:ring-ring"
          />
          <p className="text-xs text-muted-foreground">
            Output path for the WebM video file.
          </p>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setRecordDialogOpen(false)}>
              Cancel
            </Button>
            <Button size="sm" onClick={handleRecordStart} disabled={!recordPath.trim()}>
              Record
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
