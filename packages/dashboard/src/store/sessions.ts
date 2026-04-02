"use client";

import { atom } from "jotai";
import { useCallback, useEffect, useRef } from "react";
import { useAtomCallback } from "jotai/utils";
import type { SessionInfo } from "@/types";
import { type ExecResult, execCommand, killSession, sessionArgs } from "@/lib/exec";
import { tabCacheAtom, engineCacheAtom } from "@/store/tabs";
import { streamTabsAtom, streamEngineAtom } from "@/store/stream";

function getPort(): number {
  if (typeof window === "undefined") return 9223;
  const params = new URLSearchParams(window.location.search);
  const p = params.get("port");
  return p ? parseInt(p, 10) || 9223 : 9223;
}

const DASHBOARD_PORT = 4848;

function getSessionsUrl(): string {
  if (typeof window !== "undefined") {
    const origin = window.location.origin;
    if (origin.includes(`:${DASHBOARD_PORT}`)) {
      return "/api/sessions";
    }
  }
  return `http://localhost:${DASHBOARD_PORT}/api/sessions`;
}

// ---------------------------------------------------------------------------
// Primitive atoms
// ---------------------------------------------------------------------------

export const activePortAtom = atom(getPort());

export const polledSessionsAtom = atom<SessionInfo[]>([]);

export const pendingSessionsAtom = atom<{ session: string; engine: string; provider?: string }[]>(
  [],
);

export const closingSessionsAtom = atom<Set<string>>(new Set<string>());

// ---------------------------------------------------------------------------
// Derived atoms
// ---------------------------------------------------------------------------

export const sessionsAtom = atom((get) => {
  const polled = get(polledSessionsAtom);
  const pending = get(pendingSessionsAtom);
  const closing = get(closingSessionsAtom);

  const polledNames = new Set(polled.map((s) => s.session));
  const pendingEntries = pending
    .filter((p) => !polledNames.has(p.session))
    .map((p) => ({
      session: p.session,
      port: 0,
      engine: p.engine,
      provider: p.provider,
      pending: true as const,
    }));
  const merged = polled.map((s) =>
    closing.has(s.session) ? { ...s, closing: true as const } : s,
  );
  return [...merged, ...pendingEntries];
});

export const activeSessionInfoAtom = atom((get) => {
  const sessions = get(sessionsAtom);
  const port = get(activePortAtom);
  return sessions.find((s) => s.port === port);
});

export const activeSessionNameAtom = atom(
  (get) => get(activeSessionInfoAtom)?.session ?? "",
);

export const activeExtensionsAtom = atom((get) => {
  const info = get(activeSessionInfoAtom);
  return (
    (info && "extensions" in info ? info.extensions : undefined) ?? []
  );
});

// ---------------------------------------------------------------------------
// Action atoms
// ---------------------------------------------------------------------------

export const createSessionAtom = atom(
  null,
  async (
    _get,
    set,
    { name, engine, provider }: { name: string; engine: string; provider?: string },
  ): Promise<string | null> => {
    set(pendingSessionsAtom, (prev) => [...prev, { session: name, engine, provider }]);
    const args = ["--session", name];
    if (provider) {
      args.push("--provider", provider);
    } else {
      args.push("--engine", engine);
    }
    args.push("open", "https://agent-browser.dev");
    const result = await execCommand(args);
    if (!result.success) {
      set(pendingSessionsAtom, (prev) => prev.filter((p) => p.session !== name));
      killSession(name);
      return parseExecError(result) || "Failed to create session";
    }
    return null;
  },
);

function parseExecError(result: ExecResult): string {
  if (result.stderr) return result.stderr;
  if (result.stdout) {
    try {
      const json = JSON.parse(result.stdout);
      if (json.error) return json.error;
    } catch {
      // stdout wasn't JSON
    }
  }
  return "";
}

export const closeSessionAtom = atom(null, (get, set, port: number) => {
  const sessions = get(sessionsAtom);
  const s = sessions.find((x) => x.port === port)?.session;
  if (s) {
    set(closingSessionsAtom, (prev) => new Set(prev).add(s));
    execCommand(sessionArgs(s, "close"));
  }
});

export const killSessionAtom = atom(null, (get, set, port: number) => {
  const sessions = get(sessionsAtom);
  const s = sessions.find((x) => x.port === port)?.session;
  if (s) {
    set(closingSessionsAtom, (prev) => new Set(prev).add(s));
    killSession(s);
  }
});

export const closeAllSessionsAtom = atom(null, (get, set) => {
  const sessions = get(sessionsAtom);
  for (const s of sessions) {
    if (!s.pending && !s.closing) {
      set(closingSessionsAtom, (prev) => new Set(prev).add(s.session));
      execCommand(sessionArgs(s.session, "close"));
    }
  }
});

export const closeTabAtom = atom(
  null,
  (get, _set, { port, tabIndex }: { port: number; tabIndex: number }) => {
    const sessions = get(sessionsAtom);
    const s = sessions.find((x) => x.port === port)?.session;
    if (s) execCommand(sessionArgs(s, "tab", "close", String(tabIndex)));
  },
);

export const addTabAtom = atom(null, (get, _set, port: number) => {
  const sessions = get(sessionsAtom);
  const s = sessions.find((x) => x.port === port)?.session;
  if (s) execCommand(sessionArgs(s, "tab", "new"));
});

export const switchTabAtom = atom(
  null,
  (get, _set, { port, tabIndex }: { port: number; tabIndex: number }) => {
    const sessions = get(sessionsAtom);
    const s = sessions.find((x) => x.port === port)?.session;
    if (s) execCommand(sessionArgs(s, "tab", String(tabIndex)));
  },
);

/** Prune pending/closing once the polled list confirms them */
const reconcileSessionsAtom = atom(
  null,
  (get, set) => {
    const polled = get(polledSessionsAtom);
    const polledNames = new Set(polled.map((s) => s.session));

    set(pendingSessionsAtom, (prev) => {
      const next = prev.filter((p) => !polledNames.has(p.session));
      return next.length === prev.length ? prev : next;
    });

    set(closingSessionsAtom, (prev) => {
      const next = new Set(prev);
      for (const name of prev) {
        if (!polledNames.has(name)) next.delete(name);
      }
      return next.size === prev.size ? prev : next;
    });
  },
);

// ---------------------------------------------------------------------------
// Sync hook
// ---------------------------------------------------------------------------

export function useSessionsSync(pollInterval = 5000) {
  const failCountRef = useRef(0);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const reconcile = useAtomCallback(
    useCallback((_get, set) => {
      set(reconcileSessionsAtom);
    }, []),
  );

  const fetchSessions = useAtomCallback(
    useCallback(
      async (get, set) => {
        try {
          const resp = await fetch(getSessionsUrl());
          if (resp.ok) {
            failCountRef.current = 0;
            const data: SessionInfo[] = await resp.json();
            data.sort((a, b) => a.session.localeCompare(b.session));
            set(polledSessionsAtom, data);

            // Reconcile pending/closing
            reconcile();

            // Seed engine cache from session list
            const engineCache = get(engineCacheAtom);
            const nextEngine = { ...engineCache };
            let engineChanged = false;
            for (const s of data) {
              if (s.engine && !nextEngine[s.port]) {
                nextEngine[s.port] = s.engine;
                engineChanged = true;
              }
            }
            if (engineChanged) set(engineCacheAtom, nextEngine);

            // Auto-select first session if current port is not in list
            const activePort = get(activePortAtom);
            const sessions = get(sessionsAtom);
            if (sessions.length > 0 && !sessions.some((s) => s.port === activePort)) {
              set(activePortAtom, sessions[0].port);
            }

            // Poll tabs for all sessions
            for (const s of data) {
              try {
                const tabsResp = await fetch(
                  `http://localhost:${s.port}/api/tabs`,
                ).catch(() => null);
                if (tabsResp?.ok) {
                  const tabs = await tabsResp.json();
                  if (tabs.length > 0) {
                    set(tabCacheAtom, (prev) => ({ ...prev, [s.port]: tabs }));
                  }
                }
              } catch {
                // Session unreachable
              }
            }

            return;
          }
        } catch {
          // Server unreachable
        }
        failCountRef.current++;
        if (failCountRef.current >= 2) set(polledSessionsAtom, []);
      },
      [reconcile],
    ),
  );

  useEffect(() => {
    fetchSessions();
    timerRef.current = setInterval(fetchSessions, pollInterval);
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [fetchSessions, pollInterval]);
}
