"use client";

import { GeistdocsProvider } from "@vercel/geistdocs/layout";
import type { ReactNode } from "react";
import { config } from "@/lib/geistdocs/config";

export function DocsProvider({ children }: { children: ReactNode }) {
  return (
    <GeistdocsProvider
      config={config}
      lang="en"
      theme={{
        defaultTheme: "system",
        enableSystem: true,
        disableTransitionOnChange: true,
      }}
    >
      {children}
    </GeistdocsProvider>
  );
}
