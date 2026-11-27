"use client";

import { atom } from "jotai";
import { useEffect } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import type { ActivityEvent } from "@/types";
import { streamEventsAtom } from "@/store/stream";
import { activePortAtom, activeSessionNameAtom } from "@/store/sessions";

const PERSIST_KEY = "ab-persist-activity";
const MAX_PERSISTED = 500;

function activityStorageKey(session: string) {
  return `ab-activity-${session}`;
}

function loadPersistedEvents(session: string): ActivityEvent[] {
  try {
    const raw = localStorage.getItem(activityStorageKey(session));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function savePersistedEvents(session: string, events: ActivityEvent[]) {
  try {
    const capped = events.slice(-MAX_PERSISTED);
    localStorage.setItem(activityStorageKey(session), JSON.stringify(capped));
  } catch {
    // Storage full or unavailable
  }
}

function clearPersistedEvents(session: string) {
  try {
    localStorage.removeItem(activityStorageKey(session));
  } catch {
    // Ignore
  }
}

// ---------------------------------------------------------------------------
// Primitive atoms
// ---------------------------------------------------------------------------

export const persistActivityAtom = atom(
  typeof window !== "undefined"
    ? localStorage.getItem(PERSIST_KEY) === "true"
    : false,
);

export const restoredEventsAtom = atom<ActivityEvent[]>([]);
const restoredSessionAtom = atom<{ session: string; port: number } | null>(null);

// ---------------------------------------------------------------------------
// Derived atoms
// ---------------------------------------------------------------------------

export const combinedEventsAtom = atom((get) => {
  const persist = get(persistActivityAtom);
  const restored = get(restoredEventsAtom);
  const streamEvents = get(streamEventsAtom);
  const restoredSession = get(restoredSessionAtom);
  const matchesSession = restoredSession?.session === get(activeSessionNameAtom) &&
    restoredSession?.port === get(activePortAtom);

  if (persist && matchesSession && restored.length > 0) {
    return [...restored, ...streamEvents].slice(-MAX_PERSISTED);
  }
  return streamEvents;
});

// ---------------------------------------------------------------------------
// Action atoms
// ---------------------------------------------------------------------------

export const togglePersistAtom = atom(null, (get, set) => {
  const next = !get(persistActivityAtom);
  set(persistActivityAtom, next);
  localStorage.setItem(PERSIST_KEY, String(next));

  if (!next) {
    const session = get(activeSessionNameAtom);
    if (session) clearPersistedEvents(session);
    set(restoredEventsAtom, []);
  }
});

export const clearActivityAtom = atom(null, (get, set) => {
  set(streamEventsAtom, []);
  set(restoredEventsAtom, []);
  const session = get(activeSessionNameAtom);
  if (session) clearPersistedEvents(session);
});

// ---------------------------------------------------------------------------
// Sync hook -- call once to keep localStorage in sync with atoms
// ---------------------------------------------------------------------------

export function useActivitySync() {
  const persist = useAtomValue(persistActivityAtom);
  const session = useAtomValue(activeSessionNameAtom);
  const port = useAtomValue(activePortAtom);
  const restoredSession = useAtomValue(restoredSessionAtom);
  const combinedEvents = useAtomValue(combinedEventsAtom);
  const setRestored = useSetAtom(restoredEventsAtom);
  const setRestoredSession = useSetAtom(restoredSessionAtom);

  // A daemon restart can keep the session name while changing its stream port.
  useEffect(() => {
    if (persist && session) {
      setRestored(loadPersistedEvents(session));
      setRestoredSession({ session, port });
    } else {
      setRestored([]);
      setRestoredSession(null);
    }
  }, [persist, session, port, setRestored, setRestoredSession]);

  // Do not save a render's old history under a newly selected session.
  useEffect(() => {
    if (
      persist && session &&
      restoredSession?.session === session && restoredSession.port === port &&
      combinedEvents.length > 0
    ) {
      savePersistedEvents(session, combinedEvents);
    }
  }, [persist, session, port, restoredSession, combinedEvents]);
}
