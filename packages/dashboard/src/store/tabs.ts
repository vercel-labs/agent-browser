import { atom } from "jotai";
import type { TabInfo } from "@/types";
import { activePortAtom } from "@/store/sessions";
import { streamTabsAtom, streamEngineAtom } from "@/store/stream";

// ---------------------------------------------------------------------------
// Primitive atoms
// ---------------------------------------------------------------------------

export const tabCacheAtom = atom<Record<number, TabInfo[]>>({});
export const engineCacheAtom = atom<Record<number, string>>({});

// ---------------------------------------------------------------------------
// Derived atoms (used by SessionTree to get tabs/engine for any port)
// ---------------------------------------------------------------------------

export const tabsForPortAtom = atom((get) => {
  const activePort = get(activePortAtom);
  const streamTabs = get(streamTabsAtom);
  const cache = get(tabCacheAtom);

  return (port: number): TabInfo[] => {
    if (port === activePort && streamTabs.length > 0) return streamTabs;
    return cache[port] ?? [];
  };
});

export const engineForPortAtom = atom((get) => {
  const activePort = get(activePortAtom);
  const streamEngine = get(streamEngineAtom);
  const cache = get(engineCacheAtom);

  return (port: number): string => {
    if (cache[port]) return cache[port];
    if (port === activePort && streamEngine) return streamEngine;
    return "";
  };
});
