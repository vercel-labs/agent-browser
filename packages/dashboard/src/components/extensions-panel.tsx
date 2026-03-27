"use client";

import { useAtomValue } from "jotai/react";
import { activeExtensionsAtom, activeSessionNameAtom } from "@/store/sessions";
import { cn } from "@/lib/utils";
import { Puzzle } from "lucide-react";
import { Separator } from "@/components/ui/separator";
import { Badge } from "@/components/ui/badge";
import { useState } from "react";

export function ExtensionsPanel() {
  const extensions = useAtomValue(activeExtensionsAtom);
  const sessionName = useAtomValue(activeSessionNameAtom);
  const [expanded, setExpanded] = useState<string | null>(null);

  if (!sessionName) {
    return (
      <div className="flex h-full flex-col">
        <Header count={0} />
        <Separator />
        <div className="py-8 text-center text-xs text-muted-foreground">
          No active session
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <Header count={extensions.length} />
      <Separator />
      <div className="min-h-0 flex-1 overflow-y-auto">
        {extensions.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            No extensions loaded
          </div>
        ) : (
          extensions.map((ext) => {
            const isExpanded = expanded === ext.path;
            return (
              <div key={ext.path} className="border-b border-border/50">
                <button
                  type="button"
                  onClick={() => setExpanded(isExpanded ? null : ext.path)}
                  className="flex w-full items-start gap-2.5 px-3 py-2 text-left text-xs hover:bg-muted/50"
                >
                  <Puzzle className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-foreground">
                        {ext.name}
                      </span>
                      {ext.version && (
                        <Badge
                          variant="secondary"
                          className="h-4 px-1.5 text-[10px] tabular-nums"
                        >
                          v{ext.version}
                        </Badge>
                      )}
                    </div>
                    {ext.description && !isExpanded && (
                      <p className="mt-0.5 truncate text-[11px] text-muted-foreground">
                        {ext.description}
                      </p>
                    )}
                  </div>
                </button>
                {isExpanded && (
                  <div className="space-y-1 bg-muted/30 px-3 py-2 text-[11px]">
                    {ext.description && (
                      <div>
                        <span className="text-muted-foreground">Description: </span>
                        <span className="text-foreground">{ext.description}</span>
                      </div>
                    )}
                    <div>
                      <span className="text-muted-foreground">Path: </span>
                      <span className={cn("break-all font-mono text-foreground")}>
                        {ext.path}
                      </span>
                    </div>
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function Header({ count }: { count: number }) {
  return (
    <div className="flex shrink-0 items-center gap-1.5 px-3 py-2">
      <span className="text-[10px] text-muted-foreground">
        Chrome Extensions
      </span>
      {count > 0 && (
        <Badge
          variant="secondary"
          className="ml-auto h-4 px-1.5 text-[10px] tabular-nums"
        >
          {count}
        </Badge>
      )}
    </div>
  );
}
