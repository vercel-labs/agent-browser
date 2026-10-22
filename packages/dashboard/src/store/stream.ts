"use client";

import { atom } from "jotai";
import { useCallback, useEffect, useRef } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import type {
  ActivityEvent,
  ConsoleEntry,
  EventSubscription,
  StreamMessage,
  TabInfo,
} from "@/types";
import { getSessionStreamUrl } from "@/lib/dashboard-routes";
import { execCommand, sessionArgs } from "@/lib/exec";
import { isActiveStatus, parseEventsList } from "@/lib/event-subscriptions";
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
export const eventSubscriptionsAtom = atom<EventSubscription[]>([]);
export const eventsNudgeAtom = atom<Record<string, number>>({});
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

export const watchingCountAtom = atom((get) =>
  get(eventSubscriptionsAtom).filter((s) => isActiveStatus(s.status)).length,
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
  const setSubscriptions = useSetAtom(eventSubscriptionsAtom);
  const setNudges = useSetAtom(eventsNudgeAtom);
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
      setSubscriptions([]);
      setNudges({});
      setTabs([]);
      setEngine("");
    }
  }, [port, setConnected, setBrowserConnected, setScreencasting, setRecording, setVpWidth, setVpHeight, setFrame, setEvents, setConsoleLogs, setSubscriptions, setNudges, setTabs, setEngine]);

  const connect = useCallback(() => {
    if (port <= 0) return;
    if (wsRef.current?.readyState === WebSocket.OPEN) return;

    const ws = new WebSocket(getSessionStreamUrl(port));
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

        case "events_nudge": {
          setNudges((prev) => {
            const current = prev[msg.subscriptionId];
            if (current != null && current >= msg.head) return prev;
            return { ...prev, [msg.subscriptionId]: msg.head };
          });
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
  }, [port, setWsRef, setConnected, setBrowserConnected, setScreencasting, setRecording, setVpWidth, setVpHeight, setFrame, setEvents, setConsoleLogs, setSubscriptions, setNudges, setTabs, setEngine, setTabCache, setEngineCache]);

  useEffect(() => {
    connect();
    return () => {
      if (reconnectTimerRef.current) clearTimeout(reconnectTimerRef.current);
      wsRef.current?.close();
      setWsRef(null);
    };
  }, [connect, setWsRef]);
}

const EVENTS_SYNC_INTERVAL_MS = 30_000;

export function useEventHydration(session: string) {
  const connected = useAtomValue(streamConnectedAtom);
  const nudges = useAtomValue(eventsNudgeAtom);
  const setSubscriptions = useSetAtom(eventSubscriptionsAtom);
  const setNudges = useSetAtom(eventsNudgeAtom);
  const busyRef = useRef(false);
  const pendingRef = useRef(false);
  const requestRef = useRef(0);
  const sessionRef = useRef(session);
  sessionRef.current = session;

  const hydrate = useCallback(async () => {
    const current = sessionRef.current;
    if (!current) return;
    if (busyRef.current) {
      pendingRef.current = true;
      return;
    }
    busyRef.current = true;
    const request = ++requestRef.current;
    try {
      const result = await execCommand(
        sessionArgs(current, "events", "list", "--owner", `chat:${current}`),
      );
      if (!result.success || requestRef.current !== request || sessionRef.current !== current) return;
      const subscriptions = parseEventsList(result.stdout);
      if (subscriptions) setSubscriptions(subscriptions);
    } finally {
      busyRef.current = false;
      if (pendingRef.current) {
        pendingRef.current = false;
        void hydrate();
      }
    }
  }, [setSubscriptions]);

  useEffect(() => {
    requestRef.current++;
    setSubscriptions([]);
    setNudges({});
  }, [session, setSubscriptions, setNudges]);

  useEffect(() => {
    if (connected) void hydrate();
  }, [connected, nudges, hydrate, session]);

  useEffect(() => {
    if (!connected) return;
    const timer = setInterval(hydrate, EVENTS_SYNC_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [connected, hydrate]);
}
