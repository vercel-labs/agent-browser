import { afterEach, describe, expect, it } from 'vitest';
import { chromium, type Browser } from 'playwright-core';
import { WebSocket, type RawData } from 'ws';
import { createServer } from 'node:net';
import { BridgeRelayServer, type RelayMessage } from './bridge-relay.js';

async function getFreePort(): Promise<number> {
  return await new Promise<number>((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close();
        reject(new Error('Could not allocate free port'));
        return;
      }
      server.close(() => resolve(address.port));
    });
  });
}

async function waitForSocketOpen(socket: WebSocket): Promise<void> {
  if (socket.readyState === WebSocket.OPEN) return;
  await new Promise<void>((resolve, reject) => {
    const onOpen = () => {
      socket.off('error', onError);
      resolve();
    };
    const onError = (error: Error) => {
      socket.off('open', onOpen);
      reject(error);
    };
    socket.once('open', onOpen);
    socket.once('error', onError);
  });
}

interface ExtensionProxyHandle {
  close: () => Promise<void>;
  getAttachCount: () => number;
}

async function getTargetInfoFromSocket(socket: WebSocket): Promise<Record<string, unknown>> {
  const requestId = 900001;
  socket.send(
    JSON.stringify({
      id: requestId,
      method: 'Target.getTargetInfo',
      params: {},
    })
  );

  return await new Promise<Record<string, unknown>>((resolve, reject) => {
    const onMessage = (data: RawData) => {
      const message = JSON.parse(data.toString()) as RelayMessage & {
        result?: { targetInfo?: Record<string, unknown> };
      };
      if (message.id !== requestId) return;
      socket.off('message', onMessage);
      if (message.error) {
        reject(
          new Error(typeof message.error === 'string' ? message.error : message.error.message)
        );
        return;
      }
      if (!message.result?.targetInfo) {
        reject(new Error('Target.getTargetInfo returned no targetInfo'));
        return;
      }
      resolve(message.result.targetInfo);
    };
    socket.on('message', onMessage);
  });
}

async function startExtensionProxy(
  relayEndpoint: string,
  backendPort: number
): Promise<ExtensionProxyHandle> {
  const targets = (await fetch(`http://127.0.0.1:${backendPort}/json/list`).then((response) => {
    if (!response.ok) {
      throw new Error(`Could not list CDP targets: ${response.status}`);
    }
    return response.json() as Promise<
      Array<{
        id: string;
        type: string;
        title: string;
        url: string;
        webSocketDebuggerUrl: string;
      }>
    >;
  })) as Array<{
    id: string;
    type: string;
    title: string;
    url: string;
    webSocketDebuggerUrl: string;
  }>;

  const pageTarget = targets.find((target) => target.type === 'page');
  if (!pageTarget) {
    throw new Error('No page target available for extension proxy');
  }

  const backendSocket = new WebSocket(pageTarget.webSocketDebuggerUrl);
  const extensionSocket = new WebSocket(relayEndpoint);

  await Promise.all([waitForSocketOpen(backendSocket), waitForSocketOpen(extensionSocket)]);
  const targetInfo = await getTargetInfoFromSocket(backendSocket);

  const pendingRelayIds = new Map<number, number>();
  let nextBackendId = 1;
  let attachCount = 0;

  backendSocket.on('message', (data) => {
    const message = JSON.parse(data.toString()) as RelayMessage;
    if (typeof message.id === 'number') {
      const relayId = pendingRelayIds.get(message.id);
      if (!relayId) return;
      pendingRelayIds.delete(message.id);
      const response: RelayMessage = message.error
        ? { id: relayId, error: message.error }
        : { id: relayId, result: message.result ?? {} };
      if (extensionSocket.readyState === WebSocket.OPEN) {
        extensionSocket.send(JSON.stringify(response));
      }
      return;
    }

    if (message.method) {
      const eventMessage: RelayMessage = {
        method: 'forwardCDPEvent',
        params: {
          method: message.method,
          params: message.params ?? {},
        },
      };
      if (extensionSocket.readyState === WebSocket.OPEN) {
        extensionSocket.send(JSON.stringify(eventMessage));
      }
    }
  });

  extensionSocket.on('message', (data) => {
    const relayMessage = JSON.parse(data.toString()) as RelayMessage;

    if (relayMessage.method === 'attachToTab') {
      attachCount += 1;
      const attachResponse: RelayMessage = {
        id: relayMessage.id,
        result: {
          targetInfo,
        },
      };
      extensionSocket.send(JSON.stringify(attachResponse));
      return;
    }

    if (relayMessage.method === 'forwardCDPCommand') {
      const params = (relayMessage.params ?? {}) as {
        method?: string;
        params?: Record<string, unknown>;
      };

      if (!params.method || typeof relayMessage.id !== 'number') {
        return;
      }

      const backendId = nextBackendId++;
      pendingRelayIds.set(backendId, relayMessage.id);
      backendSocket.send(
        JSON.stringify({
          id: backendId,
          method: params.method,
          params: params.params ?? {},
        })
      );
    }
  });

  return {
    close: async () => {
      if (extensionSocket.readyState === WebSocket.OPEN) {
        extensionSocket.close(1000, 'done');
      }
      if (backendSocket.readyState === WebSocket.OPEN) {
        backendSocket.close(1000, 'done');
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    },
    getAttachCount: () => attachCount,
  };
}

describe('BridgeRelayServer E2E', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const cleanup = cleanups.pop();
      if (!cleanup) continue;
      await cleanup();
    }
  });

  it('bridges Playwright connectOverCDP through relay + extension protocol', async () => {
    const backendPort = await getFreePort();
    const relayPort = await getFreePort();

    const backendBrowser = await chromium.launch({
      headless: true,
      args: [`--remote-debugging-port=${backendPort}`],
    });
    cleanups.push(async () => {
      await backendBrowser.close().catch(() => {});
    });

    const backendContext = await backendBrowser.newContext();
    const backendPage = await backendContext.newPage();
    await backendPage.goto(
      'data:text/html,<html><head><title>Bridge Relay E2E</title></head><body><button id="cta">Launch</button></body></html>'
    );

    const relay = new BridgeRelayServer({ port: relayPort });
    await relay.start();
    cleanups.push(async () => {
      await relay.stop('test cleanup').catch(() => {});
    });

    const extensionProxy = await startExtensionProxy(relay.extensionEndpoint(), backendPort);
    cleanups.push(extensionProxy.close);

    await relay.waitForExtensionConnection(2000);

    let connectedBrowser: Browser | null = null;
    try {
      connectedBrowser = await chromium.connectOverCDP(relay.cdpEndpoint());
      cleanups.push(async () => {
        await connectedBrowser?.close().catch(() => {});
      });

      const contexts = connectedBrowser.contexts();
      expect(contexts.length).toBeGreaterThan(0);

      const page = contexts[0].pages()[0] ?? (await contexts[0].newPage());
      await expect(page.title()).resolves.toBe('Bridge Relay E2E');
      await expect(page.locator('#cta').textContent()).resolves.toBe('Launch');

      expect(extensionProxy.getAttachCount()).toBe(1);
    } finally {
      await connectedBrowser?.close().catch(() => {});
    }
  }, 45000);

  it('rejects extension waiters immediately when relay is stopped', async () => {
    const relayPort = await getFreePort();
    const relay = new BridgeRelayServer({ port: relayPort });
    await relay.start();

    const waitPromise = relay.waitForExtensionConnection(30000);
    await relay.stop('relay stopped for test');

    await expect(waitPromise).rejects.toThrow('relay stopped for test');
  });
});
