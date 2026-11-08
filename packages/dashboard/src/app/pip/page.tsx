"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAtom, useAtomValue, useSetAtom } from "jotai/react";
import { activePortAtom, activeSessionNameAtom, sessionsAtom, useSessionsSync } from "@/store/sessions";
import {
  browserConnectedAtom,
  currentFrameAtom,
  frameMetadataAtom,
  pointerAtom,
  sendInputAtom,
  streamConnectedAtom,
  streamEventsAtom,
  useStreamSync,
} from "@/store/stream";
import { getSessionStreamUrl, initializeDashboardAccessToken } from "@/lib/dashboard-routes";
import { cdpButton, cdpModifiers, KEY_INFO, printableKeyCode } from "@/lib/input";
import type { ActivityEvent, CommandMessage } from "@/types";

const CHROME_VISIBLE_MS = 2000;
const PILL_HOLD_MS = 2500;
const DOT_MS = 1200;
const IDLE_AFTER_MS = 30_000;
const FADE_AFTER_MS = 60_000;
const DRIVE_TIMEOUT_MS = 10_000;
const AGENT_ACTING_MS = 2000;
const OPTION_HOLD_MS = 300;
const TOOLTIP_AFTER_MS = 1000;
const FOLLOW_HYSTERESIS_MS = 3000;
const CLOSE_DISCONNECTED_MS = 60_000;
const CLOSE_NO_SESSION_MS = 30_000;
const PILL_MAX_CHARS = 50;
const BAR_HEIGHT = 20;
const TEXT_MAX_CHARS = 12;

type RefMap = Record<string, { role?: string; name?: string }>;
type PipMessage =
  | { type: "aspect"; value: number }
  | { type: "interact"; on: boolean }
  | { type: "idle"; idle: boolean }
  | { type: "bar"; visible: boolean }
  | { type: "close" };

declare global {
  interface Window {
    webkit?: { messageHandlers?: { pip?: { postMessage(message: PipMessage): void } } };
  }
}

/** Messages for the native PiP host; a no-op in a regular browser tab. */
function postToHost(message: PipMessage) {
  window.webkit?.messageHandlers?.pip?.postMessage(message);
}

/** Latest ref names from snapshot results, so `@e3` reads as `button "Sign Up"`. */
function latestRefs(events: ActivityEvent[]): RefMap {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i];
    if (event.type !== "result" || event.action !== "snapshot") continue;
    const refs = (event.data as { refs?: RefMap } | null)?.refs;
    if (refs) return refs;
  }
  return {};
}

function latestCommand(events: ActivityEvent[]): CommandMessage | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i];
    if (event.type === "command") return event;
  }
  return null;
}

function describeTarget(value: string, refs: RefMap): string {
  const ref = refs[value.replace(/^@/, "")];
  if (!value.startsWith("@") || !ref) return value;
  return ref.name ? `${ref.role ?? "element"} "${ref.name}"` : (ref.role ?? value);
}

function middleEllipsis(value: string, max: number): string {
  if (value.length <= max) return value;
  const keep = max - 1;
  return `${value.slice(0, Math.ceil(keep / 2))}…${value.slice(value.length - Math.floor(keep / 2))}`;
}

function describeAction(action: string, params: Record<string, unknown>, refs: RefMap): string {
  const target = params.selector ?? params.ref ?? params.url ?? params.key;
  const text = typeof params.text === "string" ? params.text : typeof params.value === "string" ? params.value : null;
  const parts = [action];
  if (typeof target === "string" && target.length > 0) parts.push(describeTarget(target, refs));
  if (text) parts.push(`"${text.length > TEXT_MAX_CHARS ? `${text.slice(0, TEXT_MAX_CHARS)}…` : text}"`);
  return middleEllipsis(parts.join(" "), PILL_MAX_CHARS);
}

function formatIdle(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  return minutes < 1 ? "idle" : `idle · ${minutes}m`;
}

/**
 * Follows the session whose agent acted most recently.
 */
function useFollowMostActiveSession() {
  const sessions = useAtomValue(sessionsAtom);
  const [activePort, setActivePort] = useAtom(activePortAtom);
  const lastCommandAt = useRef(new Map<number, number>());
  const ahead = useRef<{ port: number; since: number } | null>(null);

  const portsKey = sessions
    .map((s) => s.port)
    .filter((port) => port > 0)
    .sort((a, b) => a - b)
    .join(",");
  const ports = useMemo(() => (portsKey ? portsKey.split(",").map(Number) : []), [portsKey]);

  useEffect(() => {
    const sockets = ports.map((port) => {
      const ws = new WebSocket(getSessionStreamUrl(port));
      // Ack pacing without acks: after the opening frame, only events arrive.
      ws.onopen = () => ws.send(JSON.stringify({ type: "config", pacing: "ack" }));
      ws.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data) as { type?: string; timestamp?: number };
          if (msg.type === "command") lastCommandAt.current.set(port, msg.timestamp ?? Date.now());
        } catch {
          // Ignore non-JSON payloads.
        }
      };
      return ws;
    });
    return () => {
      for (const ws of sockets) ws.close();
    };
  }, [ports]);

  useEffect(() => {
    const timer = setInterval(() => {
      const times = lastCommandAt.current;
      const activeAt = times.get(activePort) ?? 0;
      let leader: number | null = null;
      for (const port of ports) {
        const at = times.get(port) ?? 0;
        if (port !== activePort && at > activeAt && (leader === null || at > (times.get(leader) ?? 0))) {
          leader = port;
        }
      }
      if (leader === null) {
        ahead.current = null;
      } else if (ahead.current?.port !== leader) {
        ahead.current = { port: leader, since: Date.now() };
      } else if (Date.now() - ahead.current.since >= FOLLOW_HYSTERESIS_MS) {
        ahead.current = null;
        setActivePort(leader);
      }
    }, 500);
    return () => clearInterval(timer);
  }, [ports, activePort, setActivePort]);
}

export default function PipPage() {
  initializeDashboardAccessToken();
  const port = useAtomValue(activePortAtom);
  useStreamSync(port);
  useSessionsSync(2000);
  useFollowMostActiveSession();

  const frame = useAtomValue(currentFrameAtom);
  const streamConnected = useAtomValue(streamConnectedAtom);
  const browserConnected = useAtomValue(browserConnectedAtom);
  const session = useAtomValue(activeSessionNameAtom);
  const events = useAtomValue(streamEventsAtom);
  const pointer = useAtomValue(pointerAtom);
  const frameMetadata = useAtomValue(frameMetadataAtom);
  const sendInput = useSetAtom(sendInputAtom);
  const cssWidth = frameMetadata?.deviceWidth ?? 0;
  const connected = streamConnected && browserConnected;

  const mainRef = useRef<HTMLElement>(null);
  const frameAreaRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const [bitmapSize, setBitmapSize] = useState({ width: 0, height: 0 });
  const [action, setAction] = useState<string | null>(null);
  const [pillVisible, setPillVisible] = useState(false);
  const [chromeVisible, setChromeVisible] = useState(true);
  const [marker, setMarker] = useState<{ x: number; y: number; key: number; amber: boolean } | null>(null);
  const [driving, setDriving] = useState(false);
  const [agentActing, setAgentActing] = useState(false);
  const [lastActivityAt, setLastActivityAt] = useState(() => Date.now());
  const [now, setNow] = useState(() => Date.now());
  const [tooltip, setTooltip] = useState(false);
  const [hovered, setHovered] = useState(false);

  const lastCommandIdRef = useRef<string | null>(null);
  const drivingRef = useRef(false);
  drivingRef.current = driving;
  const tooltipShown = useRef(false);
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const later = useCallback((key: string, ms: number, fn: () => void) => {
    clearTimeout(timers.current.get(key));
    timers.current.set(
      key,
      setTimeout(() => {
        timers.current.delete(key);
        fn();
      }, ms),
    );
  }, []);

  const cancel = useCallback((key: string) => {
    clearTimeout(timers.current.get(key));
    timers.current.delete(key);
  }, []);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) clearTimeout(timer);
    };
  }, []);

  const refs = useMemo(() => latestRefs(events), [events]);

  const showChrome = useCallback(() => {
    setChromeVisible(true);
    later("chrome", CHROME_VISIBLE_MS, () => setChromeVisible(false));
  }, [later]);

  // Show the bar on load and flash the name when the followed session changes.
  useEffect(() => {
    showChrome();
  }, [session, showChrome]);

  useEffect(() => {
    const el = frameAreaRef.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      setSize({ width: entry.contentRect.width, height: entry.contentRect.height });
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!frame || !canvas) return;
    const bin = atob(frame);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    let cancelled = false;
    createImageBitmap(new Blob([bytes], { type: "image/jpeg" })).then((bmp) => {
      if (!cancelled) {
        const { width, height } = bmp;
        canvas.width = width;
        canvas.height = height;
        canvas.getContext("2d")?.drawImage(bmp, 0, 0);
        setBitmapSize((prev) => (prev.width === width && prev.height === height ? prev : { width, height }));
      }
      bmp.close();
    });
    return () => {
      cancelled = true;
    };
  }, [frame]);

  useEffect(() => {
    if (bitmapSize.width > 0 && bitmapSize.height > 0) {
      postToHost({ type: "aspect", value: bitmapSize.width / bitmapSize.height });
    }
  }, [bitmapSize]);

  /** Displayed image rect and the CSS-pixel to display-pixel factor. */
  const layout = useMemo(() => {
    const { width: bw, height: bh } = bitmapSize;
    if (bw <= 0 || bh <= 0 || cssWidth <= 0 || size.width <= 0) return null;
    const scale = Math.min(size.width / bw, size.height / bh);
    return {
      left: (size.width - bw * scale) / 2,
      top: (size.height - bh * scale) / 2,
      width: bw * scale,
      height: bh * scale,
      toDisplay: (bw / cssWidth) * scale,
    };
  }, [bitmapSize, cssWidth, size.width, size.height]);
  const layoutRef = useRef(layout);
  layoutRef.current = layout;

  // A command's event is replaced by its result in the store, so react to new
  // command ids only and let the timers run out on their own.
  useEffect(() => {
    const command = latestCommand(events);
    if (!command || command.id === lastCommandIdRef.current) return;
    lastCommandIdRef.current = command.id;
    setLastActivityAt(Date.now());
    setAction(describeAction(command.action, command.params, refs));
    setPillVisible(true);
    later("pill", PILL_HOLD_MS, () => setPillVisible(false));
    showChrome();
    if (drivingRef.current) {
      setAgentActing(true);
      later("acting", AGENT_ACTING_MS, () => setAgentActing(false));
    }
  }, [events, refs, later, showChrome]);

  // Only a new pointer event places the marker; later layout changes do not move it.
  useEffect(() => {
    const current = layoutRef.current;
    if (!pointer || !current) return;
    setLastActivityAt(Date.now());
    setMarker({
      x: current.left + pointer.x * current.toDisplay,
      y: current.top + pointer.y * current.toDisplay,
      key: pointer.timestamp,
      amber: drivingRef.current,
    });
    later("marker", DOT_MS, () => setMarker(null));
  }, [pointer, later]);

  // Idle clock: coarse enough for "idle · 2m" and the 60 s fade.
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(timer);
  }, []);

  const idleFor = now - lastActivityAt;
  const idle = connected && idleFor >= IDLE_AFTER_MS;
  const faded = connected && idleFor >= FADE_AFTER_MS;

  useEffect(() => {
    postToHost({ type: "idle", idle: faded });
  }, [faded]);

  useEffect(() => {
    if (port > 0 && connected) return;
    const timer = setTimeout(
      () => postToHost({ type: "close" }),
      port > 0 ? CLOSE_DISCONNECTED_MS : CLOSE_NO_SESSION_MS,
    );
    return () => clearTimeout(timer);
  }, [port, connected]);

  // ---------------------------------------------------------------------------
  // Take over
  // ---------------------------------------------------------------------------

  const stopDriving = useCallback(() => {
    if (!drivingRef.current) return;
    drivingRef.current = false;
    setDriving(false);
    setAgentActing(false);
    cancel("drive-timeout");
    postToHost({ type: "interact", on: false });
    showChrome();
  }, [cancel, showChrome]);

  const startDriving = useCallback(() => {
    if (drivingRef.current || !connected) return;
    drivingRef.current = true;
    setDriving(true);
    setTooltip(false);
    postToHost({ type: "interact", on: true });
    later("drive-timeout", DRIVE_TIMEOUT_MS, stopDriving);
  }, [connected, later, stopDriving]);

  const noteHumanInput = useCallback(() => {
    later("drive-timeout", DRIVE_TIMEOUT_MS, stopDriving);
  }, [later, stopDriving]);

  useEffect(() => {
    window.addEventListener("blur", stopDriving);
    return () => window.removeEventListener("blur", stopDriving);
  }, [stopDriving]);

  const toCss = useCallback(
    (clientX: number, clientY: number) => {
      const rect = frameAreaRef.current?.getBoundingClientRect();
      if (!rect || !layout) return null;
      const x = clientX - rect.left - layout.left;
      const y = clientY - rect.top - layout.top;
      if (x < 0 || y < 0 || x > layout.width || y > layout.height) return null;
      return { x: Math.round(x / layout.toDisplay), y: Math.round(y / layout.toDisplay) };
    },
    [layout],
  );

  const forwardMouse = useCallback(
    (e: React.MouseEvent, eventType: string) => {
      const pos = toCss(e.clientX, e.clientY);
      if (!pos) return;
      noteHumanInput();
      sendInput({
        type: "input_mouse",
        eventType,
        x: pos.x,
        y: pos.y,
        button: eventType === "mouseMoved" ? "none" : cdpButton(e.button),
        clickCount: eventType === "mousePressed" ? 1 : 0,
        modifiers: cdpModifiers(e),
      });
    },
    [toCss, noteHumanInput, sendInput],
  );

  useEffect(() => {
    if (!driving) return;
    const handler = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        if (e.type === "keydown") stopDriving();
        return;
      }
      noteHumanInput();
      const eventType = e.type === "keydown" ? "keyDown" : "keyUp";
      const info = KEY_INFO[e.key];
      const text =
        eventType === "keyDown" ? (info?.text ?? (Array.from(e.key).length === 1 ? e.key : undefined)) : undefined;
      let modifiers = 0;
      if (e.altKey) modifiers |= 1;
      if (e.ctrlKey) modifiers |= 2;
      if (e.metaKey) modifiers |= 4;
      if (e.shiftKey) modifiers |= 8;
      sendInput({
        type: "input_keyboard",
        eventType,
        key: e.key,
        code: e.code,
        text,
        windowsVirtualKeyCode: info?.keyCode ?? printableKeyCode(e.key),
        modifiers,
      });
    };
    window.addEventListener("keydown", handler, true);
    window.addEventListener("keyup", handler, true);
    return () => {
      window.removeEventListener("keydown", handler, true);
      window.removeEventListener("keyup", handler, true);
    };
  }, [driving, noteHumanInput, sendInput, stopDriving]);

  const onMouseMove = (e: React.MouseEvent) => {
    if (driving) {
      forwardMouse(e, "mouseMoved");
      return;
    }
    if (e.altKey) {
      if (!timers.current.has("option")) later("option", OPTION_HOLD_MS, startDriving);
    } else {
      cancel("option");
    }
    if (!tooltipShown.current && connected && !timers.current.has("tooltip")) {
      later("tooltip", TOOLTIP_AFTER_MS, () => {
        tooltipShown.current = true;
        setTooltip(true);
        later("tooltip-hide", 2000, () => setTooltip(false));
      });
    }
  };

  const onMouseLeave = () => {
    setHovered(false);
    cancel("tooltip");
    cancel("option");
  };

  const barVisible = chromeVisible || hovered || driving || idle || !connected;
  useEffect(() => {
    postToHost({ type: "bar", visible: barVisible });
  }, [barVisible]);

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  let dotClass = "bg-emerald-400";
  let label = session || "agent-browser";
  if (driving) {
    dotClass = agentActing ? "bg-amber-400" : "bg-sky-400";
    label = agentActing ? "Agent acting" : "You are driving · Esc to hand back";
  } else if (port > 0 && !connected) {
    dotClass = "bg-white/30";
    label = "disconnected";
  } else if (idle) {
    label = `${session} · ${formatIdle(idleFor)}`;
  }

  return (
    <main
      ref={mainRef}
      className="group relative h-screen w-screen overflow-hidden bg-black text-white select-none"
      onMouseMove={onMouseMove}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={onMouseLeave}
      onMouseDown={(e) => {
        if (!driving) {
          startDriving();
          return;
        }
        forwardMouse(e, "mousePressed");
      }}
      onMouseUp={(e) => {
        if (driving) forwardMouse(e, "mouseReleased");
      }}
      onWheel={(e) => {
        if (!driving) return;
        const pos = toCss(e.clientX, e.clientY);
        if (!pos) return;
        noteHumanInput();
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
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {/* The bar pushes the frame down while visible and gives the space back when hidden. */}
      <header
        className={`pointer-events-none absolute inset-x-0 top-0 flex h-5 items-center gap-1.5 px-2 text-[10px] leading-none transition-opacity duration-200 ${barVisible ? (idle && !driving ? "opacity-60" : "opacity-100") : "opacity-0"}`}
      >
        <span className={`size-1.5 shrink-0 rounded-full ${dotClass}`} />
        <span className="truncate font-medium text-white/85">{label}</span>
      </header>

      <div
        ref={frameAreaRef}
        className="absolute inset-x-0 bottom-0 transition-[top] duration-200 ease-out"
        style={{ top: barVisible ? BAR_HEIGHT : 0 }}
      >
        <canvas
          ref={canvasRef}
          className="absolute inset-0 size-full object-contain transition-[filter] duration-300"
          style={port > 0 && !connected ? { filter: "grayscale(1) opacity(0.6)" } : undefined}
        />

        {port === 0 && (
          <div className="absolute inset-0 flex items-center justify-center text-xs text-white/60">
            No agent-browser session
          </div>
        )}

        {marker && (
          <>
            <span
              key={`ring-${marker.key}`}
              className="pointer-events-none absolute size-6 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 animate-[pip-ring_500ms_ease-out_forwards]"
              style={{ left: marker.x, top: marker.y, borderColor: marker.amber ? "#f59e0b" : "#38bdf8" }}
            />
            <span
              key={`dot-${marker.key}`}
              className="pointer-events-none absolute size-1 -translate-x-1/2 -translate-y-1/2 rounded-full"
              style={{ left: marker.x, top: marker.y, background: marker.amber ? "#f59e0b" : "#38bdf8" }}
            />
          </>
        )}

        {tooltip && !driving && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
            <span className="rounded-full bg-black/75 px-2.5 py-1 text-[11px] text-white/90 ring-1 ring-white/15">
              Click to take over
            </span>
          </div>
        )}

        {action && (
          <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
            <span
              className="max-w-[85%] truncate rounded-[20px] bg-black/80 px-2.5 py-1.5 font-mono text-[11px] leading-none text-white shadow-lg ring-1 ring-white/15 transition-opacity"
              style={{ opacity: pillVisible ? 1 : 0, transitionDuration: pillVisible ? "100ms" : "200ms" }}
            >
              {action}
            </span>
          </div>
        )}
      </div>

      {driving && <div className="pointer-events-none absolute inset-0 rounded-[10px] ring-2 ring-inset ring-sky-400" />}

      <style>{"@keyframes pip-ring { from { opacity: 0.9; } to { opacity: 0; scale: 1.6; } }"}</style>
    </main>
  );
}
