'use client';

import { useAtomValue, useSetAtom } from 'jotai/react';
import { useEffect, useState } from 'react';
import {
  activePortAtom,
  activeExtensionsAtom,
  newSessionDialogAtom,
  sessionsAtom,
} from '@/store/sessions';
import { useSessionsSync } from '@/store/sessions';
import { useStreamSync, hasConsoleErrorsAtom } from '@/store/stream';
import { useActivitySync } from '@/store/activity';
import { useChatStatusSync } from '@/store/chat';
import { useMediaQuery } from '@/hooks/use-media-query';
import { Viewport } from '@/components/viewport';
import { ActivityFeed } from '@/components/activity-feed';
import { ChatPanel } from '@/components/chat-panel';
import { ConsolePanel } from '@/components/console-panel';
import { StoragePanel } from '@/components/storage-panel';
import { ExtensionsPanel } from '@/components/extensions-panel';
import { NetworkPanel } from '@/components/network-panel';
import { SessionTree } from '@/components/session-tree';
import { ResizablePanelGroup, ResizablePanel, ResizableHandle } from '@/components/ui/resizable';
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs';
import { Button } from '@/components/ui/button';
import { Plus } from 'lucide-react';

type DashboardConfig = {
  viewportOnly: boolean;
};

function useDashboardConfig() {
  const [config, setConfig] = useState<DashboardConfig>({ viewportOnly: false });

  useEffect(() => {
    let cancelled = false;

    fetch('/api/config')
      .then((response) => (response.ok ? response.json() : null))
      .then((data) => {
        if (!cancelled && data && typeof data.viewportOnly === 'boolean') {
          setConfig({ viewportOnly: data.viewportOnly });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setConfig({ viewportOnly: false });
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return config;
}

export default function DashboardPage() {
  const activePort = useAtomValue(activePortAtom);
  useStreamSync(activePort);
  useSessionsSync();
  useActivitySync();
  useChatStatusSync();

  const { viewportOnly } = useDashboardConfig();
  const sessions = useAtomValue(sessionsAtom);
  const hasSessions = sessions.length > 0;
  const setNewSessionDialog = useSetAtom(newSessionDialogAtom);
  const isDesktop = useMediaQuery('(min-width: 768px)');
  const hasConsoleErrors = useAtomValue(hasConsoleErrorsAtom);
  const activeExtensions = useAtomValue(activeExtensionsAtom);

  const sidePanel = (
    <Tabs defaultValue="chat" className="flex h-full flex-col">
      <div className="shrink-0 px-2 pt-1">
        <TabsList variant="line" className="h-7 w-full">
          <TabsTrigger value="chat" className="text-[11px]">
            Chat
          </TabsTrigger>
          <TabsTrigger value="activity" className="text-[11px]">
            Activity
          </TabsTrigger>
          <TabsTrigger value="console" className="text-[11px]">
            Console
            {hasConsoleErrors && (
              <span className="ml-1 inline-flex size-1.5 rounded-full bg-destructive" />
            )}
          </TabsTrigger>
          <TabsTrigger value="network" className="text-[11px]">
            Network
          </TabsTrigger>
          <TabsTrigger value="storage" className="text-[11px]">
            Storage
          </TabsTrigger>
          <TabsTrigger value="extensions" className="text-[11px]">
            Extensions
            {activeExtensions.length > 0 && (
              <span className="ml-1 text-[9px] tabular-nums text-muted-foreground">
                {activeExtensions.length}
              </span>
            )}
          </TabsTrigger>
        </TabsList>
      </div>
      <TabsContent value="activity" className="min-h-0 flex-1 overflow-hidden">
        <ActivityFeed />
      </TabsContent>
      <TabsContent value="console" className="min-h-0 flex-1 overflow-hidden">
        <ConsolePanel />
      </TabsContent>
      <TabsContent value="network" className="min-h-0 flex-1 overflow-hidden">
        <NetworkPanel />
      </TabsContent>
      <TabsContent value="storage" className="min-h-0 flex-1 overflow-hidden">
        <StoragePanel />
      </TabsContent>
      <TabsContent value="extensions" className="min-h-0 flex-1 overflow-hidden">
        <ExtensionsPanel />
      </TabsContent>
      <TabsContent value="chat" className="min-h-0 flex-1 overflow-hidden">
        <ChatPanel />
      </TabsContent>
    </Tabs>
  );

  if (viewportOnly) {
    return (
      <div className="flex h-screen flex-col bg-background">
        <Viewport />
      </div>
    );
  }

  if (isDesktop) {
    if (!hasSessions) {
      return (
        <div className="flex h-screen flex-col bg-background">
          <ResizablePanelGroup orientation="horizontal" className="min-h-0 flex-1">
            <ResizablePanel id="sessions" defaultSize="15%" minSize="10%" maxSize="30%">
              <SessionTree />
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel id="empty" defaultSize="85%">
              <div className="flex h-full items-center justify-center">
                <div className="space-y-4 text-center">
                  <div className="space-y-2">
                    <p className="text-sm text-muted-foreground">No active sessions</p>
                    <p className="text-xs text-muted-foreground/60">
                      Create a session to get started
                    </p>
                  </div>
                  <Button size="sm" onClick={() => setNewSessionDialog(true)}>
                    <Plus className="size-3.5" />
                    New session
                  </Button>
                </div>
              </div>
            </ResizablePanel>
          </ResizablePanelGroup>
        </div>
      );
    }

    return (
      <div className="flex h-screen flex-col bg-background">
        <ResizablePanelGroup orientation="horizontal" className="min-h-0 flex-1">
          <ResizablePanel id="sessions" defaultSize="15%" minSize="10%" maxSize="30%">
            <SessionTree />
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel id="viewport" defaultSize="55%" minSize="30%">
            <Viewport />
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel id="activity" defaultSize="30%" minSize="15%" maxSize="50%">
            {sidePanel}
          </ResizablePanel>
        </ResizablePanelGroup>
      </div>
    );
  }

  return (
    <div className="flex h-screen flex-col bg-background">
      <Tabs defaultValue="viewport" className="min-h-0 flex-1">
        <div className="shrink-0 px-2 pt-2">
          <TabsList className="w-full">
            <TabsTrigger value="sessions">Sessions</TabsTrigger>
            <TabsTrigger value="viewport">Viewport</TabsTrigger>
            <TabsTrigger value="activity">Activity</TabsTrigger>
          </TabsList>
        </div>
        <TabsContent value="sessions" className="min-h-0 overflow-hidden">
          <SessionTree />
        </TabsContent>
        <TabsContent value="viewport" className="min-h-0 overflow-hidden">
          <Viewport />
        </TabsContent>
        <TabsContent value="activity" className="min-h-0 overflow-hidden">
          {sidePanel}
        </TabsContent>
      </Tabs>
    </div>
  );
}
