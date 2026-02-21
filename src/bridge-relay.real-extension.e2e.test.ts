import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { chromium, type BrowserContext, type Page } from 'playwright-core';
import { BrowserManager } from './browser.js';

async function getFreePort(): Promise<number> {
  const { createServer } = await import('node:net');
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

async function waitForExtensionId(context: BrowserContext): Promise<string> {
  let worker = context.serviceWorkers()[0];
  if (!worker) {
    worker = await context.waitForEvent('serviceworker', { timeout: 15000 });
  }

  const workerUrl = worker.url();
  if (!workerUrl.startsWith('chrome-extension://')) {
    throw new Error(`Unexpected service worker URL for extension: ${workerUrl}`);
  }
  return new URL(workerUrl).hostname;
}

async function maybeClickAllow(page: Page): Promise<void> {
  const allowButton = page.getByRole('button', { name: 'Allow' });
  const visible = await allowButton
    .isVisible({
      timeout: 1500,
    })
    .catch(() => false);
  if (visible) {
    await allowButton.click();
  }
}

const shouldRunRealExtensionE2E = process.env.AGENT_BROWSER_REAL_EXTENSION_E2E === '1';
const extensionPath = process.env.AGENT_BROWSER_REAL_EXTENSION_PATH;

const suite = shouldRunRealExtensionE2E ? describe : describe.skip;

suite('Bridge provider real extension E2E', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const cleanup = cleanups.pop();
      if (!cleanup) continue;
      await cleanup();
    }
    vi.restoreAllMocks();
  });

  it('connects through a real Playwright MCP Bridge extension instance', async () => {
    if (!extensionPath) {
      throw new Error(
        'Set AGENT_BROWSER_REAL_EXTENSION_PATH to an unpacked Playwright MCP Bridge extension directory.'
      );
    }

    const manifestPath = path.join(extensionPath, 'manifest.json');
    await fs.access(manifestPath).catch(() => {
      throw new Error(
        `Invalid AGENT_BROWSER_REAL_EXTENSION_PATH: manifest.json not found at ${manifestPath}`
      );
    });

    const userDataDir = await fs.mkdtemp(path.join(os.tmpdir(), 'agent-browser-bridge-ext-'));
    cleanups.push(async () => {
      await fs.rm(userDataDir, { recursive: true, force: true }).catch(() => {});
    });

    // Chromium "channel" is required for loading unpacked extensions.
    const context = await chromium.launchPersistentContext(userDataDir, {
      channel: 'chromium',
      headless: false,
      ignoreDefaultArgs: ['--enable-automation'],
      args: [`--disable-extensions-except=${extensionPath}`, `--load-extension=${extensionPath}`],
    });
    cleanups.push(async () => {
      await context.close().catch(() => {});
    });

    const extensionId = await waitForExtensionId(context);

    const targetPage = await context.newPage();
    const targetTitle = 'Bridge Real Extension E2E';
    await targetPage.goto(
      `data:text/html,<html><head><title>${targetTitle}</title></head><body><button id="cta">Launch</button></body></html>`
    );

    const relayPort = await getFreePort();
    const manager = new BrowserManager();
    cleanups.push(async () => {
      await manager.close().catch(() => {});
    });

    const openSpy = vi
      .spyOn(manager as any, 'openBridgeConnectUrl')
      .mockImplementation(async (connectUrl: string) => {
        const connectPage = await context.newPage();
        await connectPage.goto(connectUrl);
        await maybeClickAllow(connectPage);
        await connectPage
          .locator('.tab-item', { hasText: targetTitle })
          .getByRole('button', { name: 'Connect' })
          .click({ timeout: 10000 });
      });

    await manager.launch({
      id: 'test-real-extension',
      action: 'launch',
      provider: 'bridge',
      bridgePort: relayPort,
      bridgeExtensionId: extensionId,
    });

    expect(openSpy).toHaveBeenCalledTimes(1);

    const connectedPage = manager.getPage();
    await expect(connectedPage.title()).resolves.toBe(targetTitle);
    await expect(connectedPage.locator('#cta').textContent()).resolves.toBe('Launch');
  }, 120000);
});
