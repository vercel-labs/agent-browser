import * as path from 'path';
import * as os from 'os';
import {
  chromium,
  firefox,
  webkit,
  devices,
  type Browser,
  type BrowserContext,
  type Page,
  type Frame,
  type Dialog,
  type Request,
  type Route,
  type Locator,
} from 'playwright-core';
import type { LaunchCommand } from './types.js';
import { type RefMap, type EnhancedSnapshot, getEnhancedSnapshot, parseRef } from './snapshot.js';

interface TrackedRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  timestamp: number;
  resourceType: string;
}

interface ConsoleMessage {
  type: string;
  text: string;
  timestamp: number;
}

interface PageError {
  message: string;
  timestamp: number;
}

export class BrowserManager {
  private browser: Browser | null = null;
  private cdpPort: number | null = null;
  private contexts: BrowserContext[] = [];
  private pages: Page[] = [];
  private activePageIndex: number = 0;
  private activeFrame: Frame | null = null;
  private dialogHandler: ((dialog: Dialog) => Promise<void>) | null = null;
  private trackedRequests: TrackedRequest[] = [];
  private routes: Map<string, (route: Route) => Promise<void>> = new Map();
  private consoleMessages: ConsoleMessage[] = [];
  private pageErrors: PageError[] = [];
  private isRecordingHar: boolean = false;
  private refMap: RefMap = {};
  private lastSnapshot: string = '';
  private scopedHeaderRoutes: Map<string, (route: Route) => Promise<void>> = new Map();

  isLaunched(): boolean {
    return this.browser !== null || this.contexts.length > 0;
  }

  async getSnapshot(options?: {
    interactive?: boolean;
    maxDepth?: number;
    compact?: boolean;
    selector?: string;
  }): Promise<EnhancedSnapshot> {
    const page = this.getPage();
    const snapshot = await getEnhancedSnapshot(page, options);
    this.refMap = snapshot.refs;
    this.lastSnapshot = snapshot.tree;
    return snapshot;
  }

  getRefMap(): RefMap {
    return this.refMap;
  }

  getLocatorFromRef(refArg: string): Locator | null {
    const ref = parseRef(refArg);
    if (!ref) return null;

    const refData = this.refMap[ref];
    if (!refData) return null;

    const page = this.getPage();

    let locator: Locator;
    if (refData.name) {
      locator = page.getByRole(refData.role as any, { name: refData.name, exact: true });
    } else {
      locator = page.getByRole(refData.role as any);
    }

    if (refData.nth !== undefined) {
      locator = locator.nth(refData.nth);
    }

    return locator;
  }

  isRef(selector: string): boolean {
    return parseRef(selector) !== null;
  }

  getLocator(selectorOrRef: string): Locator {
    const locator = this.getLocatorFromRef(selectorOrRef);
    if (locator) return locator;

    const page = this.getPage();
    return page.locator(selectorOrRef);
  }

  getPage(): Page {
    if (this.pages.length === 0) {
      throw new Error('Browser not launched. Call launch first.');
    }
    return this.pages[this.activePageIndex];
  }

  getFrame(): Frame {
    if (this.activeFrame) {
      return this.activeFrame;
    }
    return this.getPage().mainFrame();
  }

  async switchToFrame(options: { selector?: string; name?: string; url?: string }): Promise<void> {
    const page = this.getPage();

    if (options.selector) {
      const frameElement = await page.$(options.selector);
      if (!frameElement) {
        throw new Error(`Frame not found: ${options.selector}`);
      }
      const frame = await frameElement.contentFrame();
      if (!frame) {
        throw new Error(`Element is not a frame: ${options.selector}`);
      }
      this.activeFrame = frame;
    } else if (options.name) {
      const frame = page.frame({ name: options.name });
      if (!frame) {
        throw new Error(`Frame not found with name: ${options.name}`);
      }
      this.activeFrame = frame;
    } else if (options.url) {
      const frame = page.frame({ url: options.url });
      if (!frame) {
        throw new Error(`Frame not found with URL: ${options.url}`);
      }
      this.activeFrame = frame;
    }
  }

  switchToMainFrame(): void {
    this.activeFrame = null;
  }

  setDialogHandler(response: 'accept' | 'dismiss', promptText?: string): void {
    const page = this.getPage();

    if (this.dialogHandler) {
      page.removeListener('dialog', this.dialogHandler);
    }

    this.dialogHandler = async (dialog: Dialog) => {
      if (response === 'accept') {
        await dialog.accept(promptText);
      } else {
        await dialog.dismiss();
      }
    };

    page.on('dialog', this.dialogHandler);
  }

  clearDialogHandler(): void {
    if (this.dialogHandler) {
      const page = this.getPage();
      page.removeListener('dialog', this.dialogHandler);
      this.dialogHandler = null;
    }
  }

  startRequestTracking(): void {
    const page = this.getPage();
    page.on('request', (request: Request) => {
      this.trackedRequests.push({
        url: request.url(),
        method: request.method(),
        headers: request.headers(),
        timestamp: Date.now(),
        resourceType: request.resourceType(),
      });
    });
  }

  getRequests(filter?: string): TrackedRequest[] {
    if (filter) {
      return this.trackedRequests.filter((r) => r.url.includes(filter));
    }
    return this.trackedRequests;
  }

  clearRequests(): void {
    this.trackedRequests = [];
  }

  async addRoute(
    url: string,
    options: {
      response?: {
        status?: number;
        body?: string;
        contentType?: string;
        headers?: Record<string, string>;
      };
      abort?: boolean;
    }
  ): Promise<void> {
    const page = this.getPage();

    const handler = async (route: Route) => {
      if (options.abort) {
        await route.abort();
      } else if (options.response) {
        await route.fulfill({
          status: options.response.status ?? 200,
          body: options.response.body ?? '',
          contentType: options.response.contentType ?? 'text/plain',
          headers: options.response.headers,
        });
      } else {
        await route.continue();
      }
    };

    this.routes.set(url, handler);
    await page.route(url, handler);
  }

  async removeRoute(url?: string): Promise<void> {
    const page = this.getPage();

    if (url) {
      const handler = this.routes.get(url);
      if (handler) {
        await page.unroute(url, handler);
        this.routes.delete(url);
      }
    } else {
      for (const [routeUrl, handler] of this.routes) {
        await page.unroute(routeUrl, handler);
      }
      this.routes.clear();
    }
  }

  async setGeolocation(latitude: number, longitude: number, accuracy?: number): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setGeolocation({ latitude, longitude, accuracy });
    }
  }

  async setPermissions(permissions: string[], grant: boolean): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      if (grant) {
        await context.grantPermissions(permissions);
      } else {
        await context.clearPermissions();
      }
    }
  }

  async setViewport(width: number, height: number): Promise<void> {
    const page = this.getPage();
    await page.setViewportSize({ width, height });
  }

  getDevice(deviceName: string): (typeof devices)[keyof typeof devices] | undefined {
    return devices[deviceName as keyof typeof devices];
  }

  listDevices(): string[] {
    return Object.keys(devices);
  }

  startConsoleTracking(): void {
    const page = this.getPage();
    page.on('console', (msg) => {
      this.consoleMessages.push({
        type: msg.type(),
        text: msg.text(),
        timestamp: Date.now(),
      });
    });
  }

  getConsoleMessages(): ConsoleMessage[] {
    return this.consoleMessages;
  }

  clearConsoleMessages(): void {
    this.consoleMessages = [];
  }

  startErrorTracking(): void {
    const page = this.getPage();
    page.on('pageerror', (error) => {
      this.pageErrors.push({
        message: error.message,
        timestamp: Date.now(),
      });
    });
  }

  getPageErrors(): PageError[] {
    return this.pageErrors;
  }

  clearPageErrors(): void {
    this.pageErrors = [];
  }

  async startHarRecording(): Promise<void> {
    this.isRecordingHar = true;
  }

  isHarRecording(): boolean {
    return this.isRecordingHar;
  }

  async setOffline(offline: boolean): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setOffline(offline);
    }
  }

  async setExtraHeaders(headers: Record<string, string>): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setExtraHTTPHeaders(headers);
    }
  }

  async setScopedHeaders(origin: string, headers: Record<string, string>): Promise<void> {
    const page = this.getPage();

    let urlPattern: string;
    try {
      const url = new URL(origin.startsWith('http') ? origin : `https://${origin}`);
      urlPattern = `**://${url.host}/**`;
    } catch {
      urlPattern = `**://${origin}/**`;
    }

    const existingHandler = this.scopedHeaderRoutes.get(urlPattern);
    if (existingHandler) {
      await page.unroute(urlPattern, existingHandler);
    }

    const handler = async (route: Route) => {
      const requestHeaders = route.request().headers();
      await route.continue({
        headers: {
          ...requestHeaders,
          ...headers,
        },
      });
    };

    this.scopedHeaderRoutes.set(urlPattern, handler);
    await page.route(urlPattern, handler);
  }

  async clearScopedHeaders(origin?: string): Promise<void> {
    const page = this.getPage();

    if (origin) {
      let urlPattern: string;
      try {
        const url = new URL(origin.startsWith('http') ? origin : `https://${origin}`);
        urlPattern = `**://${url.host}/**`;
      } catch {
        urlPattern = `**://${origin}/**`;
      }

      const handler = this.scopedHeaderRoutes.get(urlPattern);
      if (handler) {
        await page.unroute(urlPattern, handler);
        this.scopedHeaderRoutes.delete(urlPattern);
      }
    } else {
      for (const [pattern, handler] of this.scopedHeaderRoutes) {
        await page.unroute(pattern, handler);
      }
      this.scopedHeaderRoutes.clear();
    }
  }

  async startTracing(options: { screenshots?: boolean; snapshots?: boolean }): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.tracing.start({
        screenshots: options.screenshots ?? true,
        snapshots: options.snapshots ?? true,
      });
    }
  }

  async stopTracing(path: string): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.tracing.stop({ path });
    }
  }

  async saveStorageState(path: string): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.storageState({ path });
    }
  }

  getPages(): Page[] {
    return this.pages;
  }

  getActiveIndex(): number {
    return this.activePageIndex;
  }

  getBrowser(): Browser | null {
    return this.browser;
  }

  private isCdpConnectionAlive(): boolean {
    if (!this.browser) return false;
    try {
      const contexts = this.browser.contexts();
      if (contexts.length === 0) return false;
      return contexts.some((context) => context.pages().length > 0);
    } catch {
      return false;
    }
  }

  private needsCdpReconnect(cdpPort: number): boolean {
    if (!this.browser?.isConnected()) return true;
    if (this.cdpPort !== cdpPort) return true;
    if (!this.isCdpConnectionAlive()) return true;
    return false;
  }

  async launch(options: LaunchCommand): Promise<void> {
    const cdpPort = options.cdpPort;

    if (this.browser) {
      const switchingFromCdpToBrowser = !cdpPort && this.cdpPort !== null;
      const needsCdpReconnect = !!cdpPort && this.needsCdpReconnect(cdpPort);

      if (switchingFromCdpToBrowser || needsCdpReconnect) {
        await this.close();
      } else {
        return;
      }
    }

    if (cdpPort) {
      await this.connectViaCDP(cdpPort);
      return;
    }

    if (this.isLaunched()) return;

    const browserType = options.browser ?? 'chromium';
    const launcher =
      browserType === 'firefox' ? firefox : browserType === 'webkit' ? webkit : chromium;

    const hasExtensions = options.extensions?.length;
    if (hasExtensions && browserType !== 'chromium') {
      throw new Error('Extensions are only supported in Chromium');
    }

    const viewport = options.viewport ?? { width: 1280, height: 720 };
    let context: BrowserContext;
    let existingPages: Page[] = [];

    if (hasExtensions) {
      const extPaths = options.extensions!.join(',');
      const session = process.env.AGENT_BROWSER_SESSION || 'default';
      context = await launcher.launchPersistentContext(
        path.join(os.tmpdir(), `agent-browser-ext-${session}`),
        {
          headless: false,
          executablePath: options.executablePath,
          args: [`--disable-extensions-except=${extPaths}`, `--load-extension=${extPaths}`],
          viewport,
          extraHTTPHeaders: options.headers,
        }
      );
      existingPages = context.pages();
    } else {
      this.browser = await launcher.launch({
        headless: options.headless ?? true,
        executablePath: options.executablePath,
      });
      this.cdpPort = null;
      context = await this.browser.newContext({ viewport, extraHTTPHeaders: options.headers });
    }

    context.setDefaultTimeout(10000);
    this.contexts.push(context);

    const page = existingPages[0] ?? (await context.newPage());
    this.pages.push(page);
    this.activePageIndex = 0;
    this.setupPageTracking(page);
  }

  private async connectViaCDP(cdpPort: number | undefined): Promise<void> {
    if (!cdpPort) {
      throw new Error('cdpPort is required for CDP connection');
    }

    const browser = await chromium.connectOverCDP(`http://localhost:${cdpPort}`).catch(() => {
      throw new Error(
        `Failed to connect via CDP on port ${cdpPort}. ` +
          `Make sure the app is running with --remote-debugging-port=${cdpPort}`
      );
    });

    try {
      const contexts = browser.contexts();
      if (contexts.length === 0) {
        throw new Error('No browser context found. Make sure the app has an open window.');
      }

      const allPages = contexts.flatMap((context) => context.pages());
      if (allPages.length === 0) {
        throw new Error('No page found. Make sure the app has loaded content.');
      }

      this.browser = browser;
      this.cdpPort = cdpPort;

      for (const context of contexts) {
        this.contexts.push(context);
        this.setupContextTracking(context);
      }

      for (const page of allPages) {
        this.pages.push(page);
        this.setupPageTracking(page);
      }

      this.activePageIndex = 0;
    } catch (error) {
      await browser.close().catch(() => {});
      throw error;
    }
  }

  private setupPageTracking(page: Page): void {
    page.on('console', (msg) => {
      this.consoleMessages.push({
        type: msg.type(),
        text: msg.text(),
        timestamp: Date.now(),
      });
    });

    page.on('pageerror', (error) => {
      this.pageErrors.push({
        message: error.message,
        timestamp: Date.now(),
      });
    });

    page.on('close', () => {
      const index = this.pages.indexOf(page);
      if (index !== -1) {
        this.pages.splice(index, 1);
        if (this.activePageIndex >= this.pages.length) {
          this.activePageIndex = Math.max(0, this.pages.length - 1);
        }
      }
    });
  }

  private setupContextTracking(context: BrowserContext): void {
    context.on('page', (page) => {
      this.pages.push(page);
      this.setupPageTracking(page);
    });
  }

  async newTab(): Promise<{ index: number; total: number }> {
    if (!this.isLaunched()) {
      throw new Error('Browser not launched');
    }

    const context = this.contexts[0];
    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = this.pages.length - 1;

    this.setupPageTracking(page);

    return { index: this.activePageIndex, total: this.pages.length };
  }

  async newWindow(viewport?: {
    width: number;
    height: number;
  }): Promise<{ index: number; total: number }> {
    if (!this.browser) {
      throw new Error('Browser not launched');
    }

    const context = await this.browser.newContext({
      viewport: viewport ?? { width: 1280, height: 720 },
    });
    context.setDefaultTimeout(10000);
    this.contexts.push(context);

    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = this.pages.length - 1;

    this.setupPageTracking(page);

    return { index: this.activePageIndex, total: this.pages.length };
  }

  switchTo(index: number): { index: number; url: string; title: string } {
    if (index < 0 || index >= this.pages.length) {
      throw new Error(`Invalid tab index: ${index}. Available: 0-${this.pages.length - 1}`);
    }

    this.activePageIndex = index;
    const page = this.pages[index];

    return {
      index: this.activePageIndex,
      url: page.url(),
      title: '',
    };
  }

  async closeTab(index?: number): Promise<{ closed: number; remaining: number }> {
    const targetIndex = index ?? this.activePageIndex;

    if (targetIndex < 0 || targetIndex >= this.pages.length) {
      throw new Error(`Invalid tab index: ${targetIndex}`);
    }

    if (this.pages.length === 1) {
      throw new Error('Cannot close the last tab. Use "close" to close the browser.');
    }

    const page = this.pages[targetIndex];
    await page.close();
    this.pages.splice(targetIndex, 1);

    if (this.activePageIndex >= this.pages.length) {
      this.activePageIndex = this.pages.length - 1;
    } else if (this.activePageIndex > targetIndex) {
      this.activePageIndex--;
    }

    return { closed: targetIndex, remaining: this.pages.length };
  }

  async listTabs(): Promise<Array<{ index: number; url: string; title: string; active: boolean }>> {
    const tabs = await Promise.all(
      this.pages.map(async (page, index) => ({
        index,
        url: page.url(),
        title: await page.title().catch(() => ''),
        active: index === this.activePageIndex,
      }))
    );
    return tabs;
  }

  async close(): Promise<void> {
    if (this.cdpPort !== null) {
      if (this.browser) {
        await this.browser.close().catch(() => {});
        this.browser = null;
      }
    } else {
      for (const page of this.pages) {
        await page.close().catch(() => {});
      }
      for (const context of this.contexts) {
        await context.close().catch(() => {});
      }
      if (this.browser) {
        await this.browser.close().catch(() => {});
        this.browser = null;
      }
    }

    this.pages = [];
    this.contexts = [];
    this.cdpPort = null;
    this.activePageIndex = 0;
    this.refMap = {};
    this.lastSnapshot = '';
  }
}
