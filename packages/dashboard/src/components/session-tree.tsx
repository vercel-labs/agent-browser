"use client";

import { useCallback, useRef, useState, type SyntheticEvent } from "react";
import { useAtomValue, useSetAtom } from "jotai/react";
import type { SessionInfo, TabInfo } from "@/types";
import {
  sessionsAtom,
  activePortAtom,
  createSessionAtom,
  closeSessionAtom,
  killSessionAtom,
  closeAllSessionsAtom,
  closeTabAtom,
  addTabAtom,
  switchTabAtom,
} from "@/store/sessions";
import { tabsForPortAtom, engineForPortAtom } from "@/store/tabs";
import { ChevronRight, Loader2, Plus, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { Separator } from "@/components/ui/separator";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

const ENGINE_LOGOS: Record<string, string> = {
  chrome: "https://svgl.app/library/chrome.svg",
  firefox: "https://svgl.app/library/firefox.svg",
  safari: "https://svgl.app/library/safari.svg",
  lightpanda: "/lightpanda.svg",
};

const PROVIDER_LOGOS: Record<string, string> = {
  browserbase: "/providers/browserbase.svg",
  browserless: "/providers/browserless.svg",
  "browser-use": "/providers/browser-use.svg",
  kernel: "/providers/kernel.svg",
};

const SUPPORTED_ENGINES = ["chrome", "lightpanda"] as const;

const BROWSER_OPTIONS: { id: string; label: string; engine?: string; provider?: string }[] = [
  { id: "chrome", label: "Chrome", engine: "chrome" },
  { id: "lightpanda", label: "Lightpanda", engine: "lightpanda" },
  { id: "browserbase", label: "Browserbase", provider: "browserbase" },
  { id: "browserless", label: "Browserless", provider: "browserless" },
  { id: "browser-use", label: "Browser Use", provider: "browser-use" },
  { id: "kernel", label: "Kernel", provider: "kernel" },
];

function BrandLogo({ name, logos }: { name: string; logos: Record<string, string> }) {
  const src = logos[name];
  if (!src) {
    if (!name) {
      return <span className="size-4 shrink-0" />;
    }
    return (
      <span className="flex size-4 shrink-0 items-center justify-center rounded bg-muted text-[8px] font-bold text-muted-foreground uppercase">
        {name.charAt(0)}
      </span>
    );
  }
  return (
    <img
      src={src}
      alt={name}
      width={16}
      height={16}
      className="size-4 shrink-0"
    />
  );
}

function EngineLogo({ engine }: { engine: string }) {
  return <BrandLogo name={engine} logos={ENGINE_LOGOS} />;
}

function ProviderLogo({ provider }: { provider: string }) {
  return <BrandLogo name={provider} logos={PROVIDER_LOGOS} />;
}

function getFaviconUrl(url: string): string | null {
  try {
    const { hostname } = new URL(url);
    if (!hostname || hostname === "localhost") return null;
    return `https://www.google.com/s2/favicons?domain=${hostname}&sz=32`;
  } catch {
    return null;
  }
}

function TabFavicon({ url }: { url: string }) {
  const src = getFaviconUrl(url);
  if (!src) {
    return <span className="flex size-3.5 shrink-0 items-center justify-center rounded-sm bg-muted text-[8px] text-muted-foreground">&#9679;</span>;
  }
  const handleError = (e: SyntheticEvent<HTMLImageElement>) => {
    (e.target as HTMLImageElement).style.display = "none";
  };
  return (
    <img
      src={src}
      alt=""
      width={14}
      height={14}
      className="size-3.5 shrink-0 rounded-sm"
      onError={handleError}
    />
  );
}

function TabNode({ tab, isViewed, isSessionActive, onClose, onSwitch, onSelectSession }: { tab: TabInfo; isViewed: boolean; isSessionActive: boolean; onClose: () => void; onSwitch: () => void; onSelectSession: () => void }) {
  const handleClick = () => {
    if (!isSessionActive) {
      onSelectSession();
    }
    if (!tab.active) {
      onSwitch();
    }
  };
  const isClickable = !isViewed;
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <button
          onClick={isClickable ? handleClick : undefined}
          className={cn(
            "flex w-full min-w-0 items-center gap-1.5 py-1 pr-1 pl-7 text-left text-xs",
            isViewed
              ? "bg-card text-foreground"
              : "text-muted-foreground cursor-pointer hover:text-foreground",
          )}
        >
          <TabFavicon url={tab.url} />
          <span className="min-w-0 flex-1 truncate">
            {tab.title || tab.url || `Tab ${tab.index}`}
          </span>
          {tab.active && (
            <span className="shrink-0 rounded border border-border px-1 py-px text-[9px] leading-none text-muted-foreground">
              active
            </span>
          )}
        </button>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem onClick={onClose}>Close tab</ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

function SessionNode({
  session,
  isActive,
  tabs,
  engine,
  provider,
  expanded,
  onSelect,
  onToggle,
  onCloseTab,
  onAddTab,
  onSwitchTab,
  onClose,
  onKill,
}: {
  session: SessionInfo;
  isActive: boolean;
  tabs: TabInfo[];
  engine: string;
  provider: string;
  expanded: boolean;
  onSelect: () => void;
  onToggle: () => void;
  onCloseTab: (tabIndex: number) => void;
  onAddTab: () => void;
  onSwitchTab: (tabIndex: number) => void;
  onClose: () => void;
  onKill: () => void;
}) {
  const [confirmClose, setConfirmClose] = useState(false);
  const [confirmKill, setConfirmKill] = useState(false);

  if (session.pending || session.closing) {
    return (
      <div className="flex w-full items-center text-xs text-muted-foreground">
        <span className="flex size-6 shrink-0 items-center justify-center">
          <Loader2 className="size-3 animate-spin" />
        </span>
        <span className="flex flex-1 min-w-0 items-center gap-2 py-1.5 pr-3 pl-1">
          {(session.provider ?? provider)
            ? <ProviderLogo provider={session.provider ?? provider} />
            : <EngineLogo engine={session.engine ?? engine} />}
          <span className="truncate font-mono font-semibold">
            {session.session}
          </span>
          <span className="ml-auto text-[10px]">
            {session.closing ? "Closing..." : "Starting..."}
          </span>
        </span>
      </div>
    );
  }

  return (
    <Collapsible open={expanded} onOpenChange={() => onToggle()}>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <CollapsibleTrigger
            className={cn(
              "flex w-full items-center text-xs transition-colors",
              isActive
                ? "text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <span className="flex size-6 shrink-0 items-center justify-center">
              <ChevronRight className={cn("size-3 transition-transform", expanded && "rotate-90")} />
            </span>
            <span className="flex flex-1 min-w-0 items-center gap-2 py-1.5 pr-3 pl-1 text-left">
              {provider
                ? <ProviderLogo provider={provider} />
                : <EngineLogo engine={engine} />}
              <span className="truncate font-mono font-semibold">
                {session.session}
              </span>
              <Badge
                variant="secondary"
                className="ml-auto h-4 px-1.5 text-[10px] tabular-nums"
              >
                {tabs.length}
              </Badge>
            </span>
          </CollapsibleTrigger>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuItem onClick={() => setConfirmClose(true)}>Close session</ContextMenuItem>
          <ContextMenuItem className="text-destructive focus:text-destructive" onClick={() => setConfirmKill(true)}>Kill session</ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>

      <Dialog open={confirmClose} onOpenChange={setConfirmClose}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Close session</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            Close <span className="font-mono font-semibold text-foreground">{session.session}</span> and its browser? This action cannot be undone.
          </p>
          <DialogFooter>
            <Button variant="ghost" size="sm" onClick={() => setConfirmClose(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => {
                setConfirmClose(false);
                onClose();
              }}
            >
              Close
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={confirmKill} onOpenChange={setConfirmKill}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Kill session</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            Force-kill <span className="font-mono font-semibold text-foreground">{session.session}</span>? This immediately terminates the process without cleanup.
          </p>
          <DialogFooter>
            <Button variant="ghost" size="sm" onClick={() => setConfirmKill(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => {
                setConfirmKill(false);
                onKill();
              }}
            >
              Kill
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <CollapsibleContent>
        <div className="overflow-hidden pb-1">
          {tabs.map((tab) => (
            <TabNode key={tab.index} tab={tab} isViewed={isActive && tab.active} isSessionActive={isActive} onClose={() => onCloseTab(tab.index)} onSwitch={() => onSwitchTab(tab.index)} onSelectSession={onSelect} />
          ))}
          <button
            onClick={onAddTab}
            className="flex w-full items-center gap-1.5 py-1 pr-1 pl-7 text-xs text-muted-foreground hover:text-foreground"
          >
            <Plus className="size-3.5" />
            Add tab
          </button>
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}

export function SessionTree() {
  const sessions = useAtomValue(sessionsAtom);
  const activePort = useAtomValue(activePortAtom);
  const setActivePort = useSetAtom(activePortAtom);
  const getTabsForSession = useAtomValue(tabsForPortAtom);
  const getEngineForSession = useAtomValue(engineForPortAtom);
  const dispatchCreateSession = useSetAtom(createSessionAtom);
  const dispatchCloseSession = useSetAtom(closeSessionAtom);
  const dispatchKillSession = useSetAtom(killSessionAtom);
  const dispatchCloseAllSessions = useSetAtom(closeAllSessionsAtom);
  const dispatchCloseTab = useSetAtom(closeTabAtom);
  const dispatchAddTab = useSetAtom(addTabAtom);
  const dispatchSwitchTab = useSetAtom(switchTabAtom);

  const [expandedMap, setExpandedMap] = useState<Record<number, boolean>>({});
  const [newSessionOpen, setNewSessionOpen] = useState(false);
  const [closeAllOpen, setCloseAllOpen] = useState(false);
  const [newSessionName, setNewSessionName] = useState("");
  const [newSessionBrowser, setNewSessionBrowser] = useState("chrome");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState("");
  const nameInputRef = useRef<HTMLInputElement>(null);

  const isExpanded = useCallback(
    (port: number) => expandedMap[port] ?? true,
    [expandedMap],
  );

  const toggleExpanded = useCallback((port: number) => {
    setExpandedMap((prev) => ({ ...prev, [port]: !(prev[port] ?? true) }));
  }, []);

  const handleCreateSubmit = useCallback(async () => {
    const name = newSessionName.trim();
    if (!name || creating) return;
    setCreating(true);
    setCreateError("");
    const option = BROWSER_OPTIONS.find((o) => o.id === newSessionBrowser);
    const error = await dispatchCreateSession({
      name,
      engine: option?.engine ?? "chrome",
      provider: option?.provider,
    });
    setCreating(false);
    if (error) {
      setCreateError(error);
    } else {
      setNewSessionName("");
      setNewSessionOpen(false);
    }
  }, [newSessionName, newSessionBrowser, creating, dispatchCreateSession]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center px-3 py-2">
        <span className="text-xs text-muted-foreground">Sessions</span>
        <div className="ml-auto flex items-center gap-0.5">
          {sessions.some((s) => !s.pending) && (
            <button
              type="button"
              onClick={() => setCloseAllOpen(true)}
              className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
              title="Close all sessions"
            >
              <Trash2 className="size-3" />
            </button>
          )}
          <button
            type="button"
            onClick={() => setNewSessionOpen(true)}
            className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
            title="New session"
          >
            <Plus className="size-3.5" />
          </button>
        </div>
      </div>
      <Separator />
      <ScrollArea className="flex-1">
        <div className="w-full py-1">
          {sessions.length === 0 ? (
            <div className="py-4 text-center text-xs text-muted-foreground">
              No sessions
            </div>
          ) : (
            sessions.map((s) => (
              <SessionNode
                key={s.pending ? `pending-${s.session}` : s.port}
                session={s}
                isActive={s.port === activePort}
                tabs={getTabsForSession(s.port)}
                engine={getEngineForSession(s.port)}
                provider={s.provider ?? ""}
                expanded={isExpanded(s.port)}
                onSelect={() => setActivePort(s.port)}
                onToggle={() => toggleExpanded(s.port)}
                onCloseTab={(tabIndex) => dispatchCloseTab({ port: s.port, tabIndex })}
                onAddTab={() => dispatchAddTab(s.port)}
                onSwitchTab={(tabIndex) => dispatchSwitchTab({ port: s.port, tabIndex })}
                onClose={() => dispatchCloseSession(s.port)}
                onKill={() => dispatchKillSession(s.port)}
              />
            ))
          )}
        </div>
      </ScrollArea>

      <Dialog open={newSessionOpen} onOpenChange={(open) => {
        setNewSessionOpen(open);
        if (open) setCreateError("");
      }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>New session</DialogTitle>
          </DialogHeader>
          <input
            ref={nameInputRef}
            type="text"
            value={newSessionName}
            onChange={(e) => setNewSessionName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                handleCreateSubmit();
              }
            }}
            placeholder="Session name"
            autoFocus
            disabled={creating}
            className="w-full rounded-md border border-border bg-transparent px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"
          />
          <div className="grid grid-cols-3 gap-2">
            {BROWSER_OPTIONS.map((opt) => (
              <button
                key={opt.id}
                type="button"
                disabled={creating}
                onClick={() => setNewSessionBrowser(opt.id)}
                className={cn(
                  "flex items-center justify-center gap-2 rounded-md border px-3 py-2 text-sm transition-colors",
                  newSessionBrowser === opt.id
                    ? "border-ring bg-muted text-foreground"
                    : "border-border text-muted-foreground hover:text-foreground",
                  creating && "opacity-50",
                )}
              >
                {opt.engine
                  ? <EngineLogo engine={opt.engine} />
                  : <ProviderLogo provider={opt.provider!} />}
                {opt.label}
              </button>
            ))}
          </div>
          {createError && (
            <p className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
              {createError}
            </p>
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setNewSessionOpen(false)}
              disabled={creating}
            >
              Cancel
            </Button>
            <Button
              size="sm"
              onClick={handleCreateSubmit}
              disabled={!newSessionName.trim() || creating}
            >
              {creating && <Loader2 className="size-3 animate-spin" />}
              {creating ? "Creating..." : "Create"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={closeAllOpen} onOpenChange={setCloseAllOpen}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Close all sessions</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            This will close {sessions.filter((s) => !s.pending).length} active {sessions.filter((s) => !s.pending).length === 1 ? "session" : "sessions"} and their browsers. This action cannot be undone.
          </p>
          <DialogFooter>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setCloseAllOpen(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => {
                setCloseAllOpen(false);
                dispatchCloseAllSessions();
              }}
            >
              Close all
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
