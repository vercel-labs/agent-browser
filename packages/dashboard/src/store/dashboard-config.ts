"use client";

import { atom } from "jotai";
import { useSetAtom } from "jotai/react";
import { useEffect } from "react";
import { getDashboardApiPath } from "@/lib/dashboard-routes";

/** What the dashboard server lets this UI offer, from `/api/dashboard/config`. */
export interface DashboardConfig {
  engines: string[];
  providers: string[];
  chat: { enabled: boolean; model?: string };
}

/** Null until the config loads. */
export const dashboardConfigAtom = atom<DashboardConfig | null>(null);

export function useDashboardConfigSync() {
  const setConfig = useSetAtom(dashboardConfigAtom);

  useEffect(() => {
    fetch(getDashboardApiPath("/api/dashboard/config"))
      .then((resp) => (resp.ok ? resp.json() : null))
      .then((config: DashboardConfig | null) => {
        if (config) setConfig(config);
      })
      .catch(() => {
        // Without a config the UI hides chat and creates Chrome sessions.
      });
  }, [setConfig]);
}
