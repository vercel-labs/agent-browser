"use client";

import { atom } from "jotai";
import { useCallback, useEffect, useRef } from "react";
import { useSetAtom } from "jotai/react";
import type {
  ActivityEvent,
  ConsoleEntry,
  StreamMessage,
  TabInfo,
} from "@/types";
import { activePortAtom } from "@/store/sessions";
import { tabCacheAtom, engineCacheAtom } from "@/store/tabs";

const MAX_EVENTS = 500;

// ---------------------------------------------------------------------------
// Primitive atoms
// ---------------------------------------------------------------------------

export const streamConnectedAtom = atom(false);
export const browserConnectedAtom = atom(false);
export const screencastingAtom = atom(false);
export const recordingAtom = atom(false);
export const viewportWidthAtom = atom(1280);
export const viewportHeightAtom = atom(720);
export const currentFrameAtom = atom<string | null>(null);
export const streamEventsAtom = atom<ActivityEvent[]>([]);
export const consoleLogsAtom = atom<ConsoleEntry[]>([]);
export const streamTabsAtom = atom<TabInfo[]>([]);
export const streamEngineAtom = atom("");
export const wsRefAtom = atom<WebSocket | null>(null);

// ---------------------------------------------------------------------------
// Derived atoms
// ---------------------------------------------------------------------------

export const activeUrlAtom = atom(
  (get) => get(streamTabsAtom).find((t) => t.active)?.url ?? "",
);

export const hasConsoleErrorsAtom = atom((get) =>
  get(consoleLogsAtom).some(
    (e) =>
      e.type === "page_error" ||
      (e.type === "console" && e.level === "error"),
  ),
);

// ---------------------------------------------------------------------------
// Action atoms
// ---------------------------------------------------------------------------

export const sendInputAtom = atom(
  null,
  (get, _set, msg: Record<string, unknown>) => {
    const ws = get(wsRefAtom);
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  },
);

export const clearEventsAtom = atom(null, (_get, set) => {
  set(streamEventsAtom, []);
});

export const clearConsoleLogsAtom = atom(null, (_get, set) => {
  set(consoleLogsAtom, []);
});

// ---------------------------------------------------------------------------
// Sync hook
// ---------------------------------------------------------------------------

export function useStreamSync(port: number) {
  const setConnected = useSetAtom(streamConnectedAtom);
  const setBrowserConnected = useSetAtom(browserConnectedAtom);
  const setScreencasting = useSetAtom(screencastingAtom);
  const setRecording = useSetAtom(recordingAtom);
  const setVpWidth = useSetAtom(viewportWidthAtom);
  const setVpHeight = useSetAtom(viewportHeightAtom);
  const setFrame = useSetAtom(currentFrameAtom);
  const setEvents = useSetAtom(streamEventsAtom);
  const setConsoleLogs = useSetAtom(consoleLogsAtom);
  const setTabs = useSetAtom(streamTabsAtom);
  const setEngine = useSetAtom(streamEngineAtom);
  const setWsRef = useSetAtom(wsRefAtom);
  const setTabCache = useSetAtom(tabCacheAtom);
  const setEngineCache = useSetAtom(engineCacheAtom);

  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);
  const eventsRef = useRef<ActivityEvent[]>([]);
  const consoleRef = useRef<ConsoleEntry[]>([]);
  const portRef = useRef(port);

  // Reset all stream state when port changes
  useEffect(() => {
    if (portRef.current !== port) {
      portRef.current = port;
      eventsRef.current = [];
      consoleRef.current = [];
      setConnected(false);
      setBrowserConnected(false);
      setScreencasting(false);
      setRecording(false);
      setVpWidth(1280);
      setVpHeight(720);
      setFrame(null);
      setEvents([]);
      setConsoleLogs([]);
      setTabs([]);
      setEngine("");
    }
  }, [port, setConnected, setBrowserConnected, setScreencasting, setRecording, setVpWidth, setVpHeight, setFrame, setEvents, setConsoleLogs, setTabs, setEngine]);

  const connect = useCallback(() => {
    if (wsRef.current?.readyState === WebSocket.OPEN) return;

    const ws = new WebSocket(`ws://localhost:${port}`);
    wsRef.current = ws;
    setWsRef(ws);

    ws.onopen = () => {
      retryCountRef.current = 0;
      setConnected(true);
    };

    ws.onclose = () => {
      setConnected(false);
      const delay = Math.min(2000 * 2 ** retryCountRef.current, 30000);
      retryCountRef.current++;
      reconnectTimerRef.current = setTimeout(connect, delay);
    };

    ws.onerror = () => {
      ws.close();
    };

    ws.onmessage = (event) => {
      let msg: StreamMessage;
      try {
        msg = JSON.parse(event.data);
      } catch {
        return;
      }

      switch (msg.type) {
        case "frame":
          setFrame(msg.data);
          break;

        case "status":
          setBrowserConnected(msg.connected);
          setScreencasting(msg.screencasting);
          if (msg.recording != null) setRecording(msg.recording);
          setVpWidth(msg.viewportWidth);
          setVpHeight(msg.viewportHeight);
          if (msg.engine) {
            setEngine(msg.engine);
            setEngineCache((prev) => ({ ...prev, [port]: msg.engine! }));
          }
          break;

        case "command": {
          const updated = [...eventsRef.current, msg].slice(-MAX_EVENTS);
          eventsRef.current = updated;
          setEvents(updated);
          break;
        }

        case "console": {
          const conUpdated = [...consoleRef.current, msg].slice(-MAX_EVENTS);
          consoleRef.current = conUpdated;
          setConsoleLogs(conUpdated);
          break;
        }

        case "page_error": {
          const conUpdated = [...consoleRef.current, msg].slice(-MAX_EVENTS);
          consoleRef.current = conUpdated;
          setConsoleLogs(conUpdated);
          break;
        }

        case "result": {
          const cmdIdx = eventsRef.current.findIndex(
            (e) => e.type === "command" && e.id === msg.id,
          );
          const base =
            cmdIdx >= 0
              ? [
                  ...eventsRef.current.slice(0, cmdIdx),
                  ...eventsRef.current.slice(cmdIdx + 1),
                ]
              : eventsRef.current;
          const updated = [...base, msg].slice(-MAX_EVENTS);
          eventsRef.current = updated;
          setEvents(updated);
          break;
        }

        case "tabs":
          setTabs(msg.tabs);
          setTabCache((prev) => ({ ...prev, [port]: msg.tabs }));
          break;

        case "url":
          setTabs((prev) =>
            prev.map((t) => (t.active ? { ...t, url: msg.url } : t)),
          );
          break;

        case "error":
          break;
      }
    };
  }, [port, setWsRef, setConnected, setBrowserConnected, setScreencasting, setRecording, setVpWidth, setVpHeight, setFrame, setEvents, setConsoleLogs, setTabs, setEngine, setTabCache, setEngineCache]);

  useEffect(() => {
    connect();
    return () => {
      if (reconnectTimerRef.current) clearTimeout(reconnectTimerRef.current);
      wsRef.current?.close();
      setWsRef(null);
    };
  }, [connect, setWsRef]);
}
