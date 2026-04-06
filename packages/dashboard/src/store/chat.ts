"use client";

import { atom } from "jotai";
import { useEffect } from "react";
import { useAtomCallback } from "jotai/utils";
import { useCallback } from "react";

const DAEMON_URL = process.env.NEXT_PUBLIC_DAEMON_URL || "";

function daemonBase(): string {
  if (typeof window === "undefined" || !DAEMON_URL) return "";
  try {
    const daemon = new URL(DAEMON_URL);
    if (window.location.host === daemon.host) return "";
    return DAEMON_URL;
  } catch {
    return "";
  }
}

function getChatStatusUrl(): string {
  return `${daemonBase()}/api/chat/status`;
}

export function getChatApiUrl(): string {
  return `${daemonBase()}/api/chat`;
}

export function getModelsApiUrl(): string {
  return `${daemonBase()}/api/models`;
}

export interface ModelInfo {
  id: string;
  name?: string;
  owned_by?: string;
  context_window?: number;
}

export const chatEnabledAtom = atom(false);
export const chatModelAtom = atom<string | undefined>(undefined);
export const availableModelsAtom = atom<ModelInfo[]>([]);

export function useChatStatusSync() {
  const fetchStatus = useAtomCallback(
    useCallback(async (_get, set) => {
      try {
        const resp = await fetch(getChatStatusUrl());
        if (resp.ok) {
          const data = await resp.json();
          set(chatEnabledAtom, !!data.enabled);
          if (data.model) set(chatModelAtom, data.model);
        }
      } catch {
        set(chatEnabledAtom, false);
      }
      try {
        const resp = await fetch(getModelsApiUrl());
        if (resp.ok) {
          const data = await resp.json();
          if (Array.isArray(data?.data)) {
            const models: ModelInfo[] = data.data.map((m: Record<string, unknown>) => ({
              id: m.id as string,
              name: (m.name as string) || undefined,
              owned_by: (m.owned_by as string) || undefined,
              context_window: typeof m.context_window === "number" ? m.context_window : undefined,
            }));
            models.sort((a, b) => a.id.localeCompare(b.id));
            set(availableModelsAtom, models);
          }
        }
      } catch {
        // models fetch failed, leave empty
      }
    }, []),
  );

  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);
}
