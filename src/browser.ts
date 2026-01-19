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
  type CDPSession,
  type Video,
} from 'playwright-core';
import path from 'node:path';
import os from 'node:os';
import { existsSync, mkdirSync, rmSync } from 'node:fs';
import type { LaunchCommand } from './types.js';
import { type RefMap, type EnhancedSnapshot, getEnhancedSnapshot, parseRef } from './snapshot.js';

// Screencast frame data from CDP
export interface ScreencastFrame {
  data: string; // base64 encoded image
  metadata: {
    offsetTop: number;
    pageScaleFactor: number;
    deviceWidth: number;
    deviceHeight: number;
    scrollOffsetX: number;
    scrollOffsetY: number;
    timestamp?: number;
  };
  sessionId: number;
}

// Screencast options
export interface ScreencastOptions {
  format?: 'jpeg' | 'png';
  quality?: number; // 0-100, only for jpeg
  maxWidth?: number;
  maxHeight?: number;
  everyNthFrame?: number;
}

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

/**
 * Manages the Playwright browser lifecycle with multiple tabs/windows
 */
export class BrowserManager {
  private browser: Browser | null = null;
  private cdpPort: number | null = null;
  private isPersistentContext: boolean = false;
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

  // CDP session for screencast and input injection
  private cdpSession: CDPSession | null = null;
  private screencastActive: boolean = false;
  private screencastSessionId: number = 0;
  private frameCallback: ((frame: ScreencastFrame) => void) | null = null;
  private screencastFrameHandler: ((params: any) => void) | null = null;

  // Video recording (Playwright native)
  private recordingContext: BrowserContext | null = null;
  private recordingPage: Page | null = null;
  private recordingOutputPath: string = '';
  private recordingTempDir: string = '';

  /**
   * Check if browser is launched
   */
  isLaunched(): boolean {
    return this.browser !== null || this.isPersistentContext;
  }

  /**
   * Get enhanced snapshot with refs and cache the ref map
   */
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

  /**
   * Get the cached ref map from last snapshot
   */
  getRefMap(): RefMap {
    return this.refMap;
  }

  /**
   * Get a locator from a ref (e.g., "e1", "@e1", "ref=e1")
   * Returns null if ref doesn't exist or is invalid
   */
  getLocatorFromRef(refArg: string): Locator | null {
    const ref = parseRef(refArg);
    if (!ref) return null;

    const refData = this.refMap[ref];
    if (!refData) return null;

    const page = this.getPage();

    // Build locator with exact: true to avoid substring matches
    let locator: Locator;
    if (refData.name) {
      locator = page.getByRole(refData.role as any, { name: refData.name, exact: true });
    } else {
      locator = page.getByRole(refData.role as any);
    }

    // If an nth index is stored (for disambiguation), use it
    if (refData.nth !== undefined) {
      locator = locator.nth(refData.nth);
    }

    return locator;
  }

  /**
   * Check if a selector looks like a ref
   */
  isRef(selector: string): boolean {
    return parseRef(selector) !== null;
  }

  /**
   * Get locator - supports both refs and regular selectors
   */
  getLocator(selectorOrRef: string): Locator {
    // Check if it's a ref first
    const locator = this.getLocatorFromRef(selectorOrRef);
    if (locator) return locator;

    // Otherwise treat as regular selector
    const page = this.getPage();
    return page.locator(selectorOrRef);
  }

  /**
   * Get the current active page, throws if not launched
   */
  getPage(): Page {
    if (this.pages.length === 0) {
      throw new Error('Browser not launched. Call launch first.');
    }
    return this.pages[this.activePageIndex];
  }

  /**
   * Get the current frame (or page's main frame if no frame is selected)
   */
  getFrame(): Frame {
    if (this.activeFrame) {
      return this.activeFrame;
    }
    return this.getPage().mainFrame();
  }

  /**
   * Switch to a frame by selector, name, or URL
   */
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

  /**
   * Switch back to main frame
   */
  switchToMainFrame(): void {
    this.activeFrame = null;
  }

  /**
   * Set up dialog handler
   */
  setDialogHandler(response: 'accept' | 'dismiss', promptText?: string): void {
    const page = this.getPage();

    // Remove existing handler if any
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

  /**
   * Clear dialog handler
   */
  clearDialogHandler(): void {
    if (this.dialogHandler) {
      const page = this.getPage();
      page.removeListener('dialog', this.dialogHandler);
      this.dialogHandler = null;
    }
  }

  /**
   * Start tracking requests
   */
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

  /**
   * Get tracked requests
   */
  getRequests(filter?: string): TrackedRequest[] {
    if (filter) {
      return this.trackedRequests.filter((r) => r.url.includes(filter));
    }
    return this.trackedRequests;
  }

  /**
   * Clear tracked requests
   */
  clearRequests(): void {
    this.trackedRequests = [];
  }

  /**
   * Add a route to intercept requests
   */
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

  /**
   * Remove a route
   */
  async removeRoute(url?: string): Promise<void> {
    const page = this.getPage();

    if (url) {
      const handler = this.routes.get(url);
      if (handler) {
        await page.unroute(url, handler);
        this.routes.delete(url);
      }
    } else {
      // Remove all routes
      for (const [routeUrl, handler] of this.routes) {
        await page.unroute(routeUrl, handler);
      }
      this.routes.clear();
    }
  }

  /**
   * Set geolocation
   */
  async setGeolocation(latitude: number, longitude: number, accuracy?: number): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setGeolocation({ latitude, longitude, accuracy });
    }
  }

  /**
   * Set permissions
   */
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

  /**
   * Set viewport
   */
  async setViewport(width: number, height: number): Promise<void> {
    const page = this.getPage();
    await page.setViewportSize({ width, height });
  }

  /**
   * Get device descriptor
   */
  getDevice(deviceName: string): (typeof devices)[keyof typeof devices] | undefined {
    return devices[deviceName as keyof typeof devices];
  }

  /**
   * List available devices
   */
  listDevices(): string[] {
    return Object.keys(devices);
  }

  /**
   * Start console message tracking
   */
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

  /**
   * Get console messages
   */
  getConsoleMessages(): ConsoleMessage[] {
    return this.consoleMessages;
  }

  /**
   * Clear console messages
   */
  clearConsoleMessages(): void {
    this.consoleMessages = [];
  }

  /**
   * Start error tracking
   */
  startErrorTracking(): void {
    const page = this.getPage();
    page.on('pageerror', (error) => {
      this.pageErrors.push({
        message: error.message,
        timestamp: Date.now(),
      });
    });
  }

  /**
   * Get page errors
   */
  getPageErrors(): PageError[] {
    return this.pageErrors;
  }

  /**
   * Clear page errors
   */
  clearPageErrors(): void {
    this.pageErrors = [];
  }

  /**
   * Start HAR recording
   */
  async startHarRecording(): Promise<void> {
    // HAR is started at context level, flag for tracking
    this.isRecordingHar = true;
  }

  /**
   * Check if HAR recording
   */
  isHarRecording(): boolean {
    return this.isRecordingHar;
  }

  /**
   * Set offline mode
   */
  async setOffline(offline: boolean): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setOffline(offline);
    }
  }

  /**
   * Set extra HTTP headers (global - all requests)
   */
  async setExtraHeaders(headers: Record<string, string>): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.setExtraHTTPHeaders(headers);
    }
  }

  /**
   * Set scoped HTTP headers (only for requests matching the origin)
   * Uses route interception to add headers only to matching requests
   */
  async setScopedHeaders(origin: string, headers: Record<string, string>): Promise<void> {
    const page = this.getPage();

    // Build URL pattern from origin (e.g., "api.example.com" -> "**://api.example.com/**")
    // Handle both full URLs and just hostnames
    let urlPattern: string;
    try {
      const url = new URL(origin.startsWith('http') ? origin : `https://${origin}`);
      // Match any protocol, the host, and any path
      urlPattern = `**://${url.host}/**`;
    } catch {
      // If parsing fails, treat as hostname pattern
      urlPattern = `**://${origin}/**`;
    }

    // Remove existing route for this origin if any
    const existingHandler = this.scopedHeaderRoutes.get(urlPattern);
    if (existingHandler) {
      await page.unroute(urlPattern, existingHandler);
    }

    // Create handler that adds headers to matching requests
    const handler = async (route: Route) => {
      const requestHeaders = route.request().headers();
      await route.continue({
        headers: {
          ...requestHeaders,
          ...headers,
        },
      });
    };

    // Store and register the route
    this.scopedHeaderRoutes.set(urlPattern, handler);
    await page.route(urlPattern, handler);
  }

  /**
   * Clear scoped headers for an origin (or all if no origin specified)
   */
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
      // Clear all scoped header routes
      for (const [pattern, handler] of this.scopedHeaderRoutes) {
        await page.unroute(pattern, handler);
      }
      this.scopedHeaderRoutes.clear();
    }
  }

  /**
   * Start tracing
   */
  async startTracing(options: { screenshots?: boolean; snapshots?: boolean }): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.tracing.start({
        screenshots: options.screenshots ?? true,
        snapshots: options.snapshots ?? true,
      });
    }
  }

  /**
   * Stop tracing and save
   */
  async stopTracing(path: string): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.tracing.stop({ path });
    }
  }

  /**
   * Save storage state (cookies, localStorage, etc.)
   */
  async saveStorageState(path: string): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.storageState({ path });
    }
  }

  /**
   * Get all pages
   */
  getPages(): Page[] {
    return this.pages;
  }

  /**
   * Get current page index
   */
  getActiveIndex(): number {
    return this.activePageIndex;
  }

  /**
   * Get the current browser instance
   */
  getBrowser(): Browser | null {
    return this.browser;
  }

  /**
   * Check if an existing CDP connection is still alive
   * by verifying we can access browser contexts and that at least one has pages
   */
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

  /**
   * Check if CDP connection needs to be re-established
   */
  private needsCdpReconnect(cdpPort: number): boolean {
    if (!this.browser?.isConnected()) return true;
    if (this.cdpPort !== cdpPort) return true;
    if (!this.isCdpConnectionAlive()) return true;
    return false;
  }

  /**
   * Launch the browser with the specified options
   * If already launched, this is a no-op (browser stays open)
   */
  async launch(options: LaunchCommand): Promise<void> {
    const cdpPort = options.cdpPort;
    const hasExtensions = !!options.extensions?.length;

    if (hasExtensions && cdpPort) {
      throw new Error('Extensions cannot be used with CDP connection');
    }

    if (this.isLaunched()) {
      const needsRelaunch =
        (!cdpPort && this.cdpPort !== null) || (!!cdpPort && this.needsCdpReconnect(cdpPort));
      if (needsRelaunch) {
        await this.close();
      } else {
        return;
      }
    }

    if (cdpPort) {
      await this.connectViaCDP(cdpPort);
      return;
    }

    const browserType = options.browser ?? 'chromium';
    if (hasExtensions && browserType !== 'chromium') {
      throw new Error('Extensions are only supported in Chromium');
    }

    const launcher =
      browserType === 'firefox' ? firefox : browserType === 'webkit' ? webkit : chromium;
    const viewport = options.viewport ?? { width: 1280, height: 720 };

    let context: BrowserContext;
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
          ...(options.proxy && { proxy: options.proxy }),
        }
      );
      this.isPersistentContext = true;
    } else {
      this.browser = await launcher.launch({
        headless: options.headless ?? true,
        executablePath: options.executablePath,
      });
      this.cdpPort = null;
      context = await this.browser.newContext({
        viewport,
        extraHTTPHeaders: options.headers,
        ...(options.proxy && { proxy: options.proxy }),
      });
    }

    context.setDefaultTimeout(60000);
    this.contexts.push(context);

    const page = context.pages()[0] ?? (await context.newPage());
    this.pages.push(page);
    this.activePageIndex = 0;
    this.setupPageTracking(page);
  }

  /**
   * Connect to a running browser via CDP (Chrome DevTools Protocol)
   */
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

    // Validate and set up state, cleaning up browser connection if anything fails
    try {
      const contexts = browser.contexts();
      if (contexts.length === 0) {
        throw new Error('No browser context found. Make sure the app has an open window.');
      }

      // Filter out pages with empty URLs, which can cause Playwright to hang
      const allPages = contexts.flatMap((context) => context.pages()).filter((page) => page.url());

      if (allPages.length === 0) {
        throw new Error('No page found. Make sure the app has loaded content.');
      }

      // All validation passed - commit state
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
      // Clean up browser connection if validation or setup failed
      await browser.close().catch(() => {});
      throw error;
    }
  }

  /**
   * Set up console, error, and close tracking for a page
   */
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

  /**
   * Set up tracking for new pages in a context (for CDP connections)
   */
  private setupContextTracking(context: BrowserContext): void {
    context.on('page', (page) => {
      this.pages.push(page);
      this.setupPageTracking(page);
    });
  }

  /**
   * Create a new tab in the current context
   */
  async newTab(): Promise<{ index: number; total: number }> {
    if (!this.browser || this.contexts.length === 0) {
      throw new Error('Browser not launched');
    }

    // Invalidate CDP session since we're switching to a new page
    await this.invalidateCDPSession();

    const context = this.contexts[0]; // Use first context for tabs
    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = this.pages.length - 1;

    // Set up tracking for the new page
    this.setupPageTracking(page);

    return { index: this.activePageIndex, total: this.pages.length };
  }

  /**
   * Create a new window (new context)
   */
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
    context.setDefaultTimeout(60000);
    this.contexts.push(context);

    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = this.pages.length - 1;

    // Set up tracking for the new page
    this.setupPageTracking(page);

    return { index: this.activePageIndex, total: this.pages.length };
  }

  /**
   * Invalidate the current CDP session (must be called before switching pages)
   * This ensures screencast and input injection work correctly after tab switch
   */
  private async invalidateCDPSession(): Promise<void> {
    // Stop screencast if active (it's tied to the current page's CDP session)
    if (this.screencastActive) {
      await this.stopScreencast();
    }

    // Detach and clear the CDP session
    if (this.cdpSession) {
      await this.cdpSession.detach().catch(() => {});
      this.cdpSession = null;
    }
  }

  /**
   * Switch to a specific tab/page by index
   */
  async switchTo(index: number): Promise<{ index: number; url: string; title: string }> {
    if (index < 0 || index >= this.pages.length) {
      throw new Error(`Invalid tab index: ${index}. Available: 0-${this.pages.length - 1}`);
    }

    // Invalidate CDP session before switching (it's page-specific)
    if (index !== this.activePageIndex) {
      await this.invalidateCDPSession();
    }

    this.activePageIndex = index;
    const page = this.pages[index];

    return {
      index: this.activePageIndex,
      url: page.url(),
      title: '', // Title requires async, will be fetched separately
    };
  }

  /**
   * Close a specific tab/page
   */
  async closeTab(index?: number): Promise<{ closed: number; remaining: number }> {
    const targetIndex = index ?? this.activePageIndex;

    if (targetIndex < 0 || targetIndex >= this.pages.length) {
      throw new Error(`Invalid tab index: ${targetIndex}`);
    }

    if (this.pages.length === 1) {
      throw new Error('Cannot close the last tab. Use "close" to close the browser.');
    }

    // If closing the active tab, invalidate CDP session first
    if (targetIndex === this.activePageIndex) {
      await this.invalidateCDPSession();
    }

    const page = this.pages[targetIndex];
    await page.close();
    this.pages.splice(targetIndex, 1);

    // Adjust active index if needed
    if (this.activePageIndex >= this.pages.length) {
      this.activePageIndex = this.pages.length - 1;
    } else if (this.activePageIndex > targetIndex) {
      this.activePageIndex--;
    }

    return { closed: targetIndex, remaining: this.pages.length };
  }

  /**
   * List all tabs with their info
   */
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

  /**
   * Get or create a CDP session for the current page
   * Only works with Chromium-based browsers
   */
  async getCDPSession(): Promise<CDPSession> {
    if (this.cdpSession) {
      return this.cdpSession;
    }

    const page = this.getPage();
    const context = page.context();

    // Create a new CDP session attached to the page
    this.cdpSession = await context.newCDPSession(page);
    return this.cdpSession;
  }

  /**
   * Check if screencast is currently active
   */
  isScreencasting(): boolean {
    return this.screencastActive;
  }

  /**
   * Start screencast - streams viewport frames via CDP
   * @param callback Function called for each frame
   * @param options Screencast options
   */
  async startScreencast(
    callback: (frame: ScreencastFrame) => void,
    options?: ScreencastOptions
  ): Promise<void> {
    if (this.screencastActive) {
      throw new Error('Screencast already active');
    }

    const cdp = await this.getCDPSession();
    this.frameCallback = callback;
    this.screencastActive = true;

    // Create and store the frame handler so we can remove it later
    this.screencastFrameHandler = async (params: any) => {
      const frame: ScreencastFrame = {
        data: params.data,
        metadata: params.metadata,
        sessionId: params.sessionId,
      };

      // Acknowledge the frame to receive the next one
      await cdp.send('Page.screencastFrameAck', { sessionId: params.sessionId });

      // Call the callback with the frame
      if (this.frameCallback) {
        this.frameCallback(frame);
      }
    };

    // Listen for screencast frames
    cdp.on('Page.screencastFrame', this.screencastFrameHandler);

    // Start the screencast
    await cdp.send('Page.startScreencast', {
      format: options?.format ?? 'jpeg',
      quality: options?.quality ?? 80,
      maxWidth: options?.maxWidth ?? 1280,
      maxHeight: options?.maxHeight ?? 720,
      everyNthFrame: options?.everyNthFrame ?? 1,
    });
  }

  /**
   * Stop screencast
   */
  async stopScreencast(): Promise<void> {
    if (!this.screencastActive) {
      return;
    }

    try {
      const cdp = await this.getCDPSession();
      await cdp.send('Page.stopScreencast');

      // Remove the event listener to prevent accumulation
      if (this.screencastFrameHandler) {
        cdp.off('Page.screencastFrame', this.screencastFrameHandler);
      }
    } catch {
      // Ignore errors when stopping
    }

    this.screencastActive = false;
    this.frameCallback = null;
    this.screencastFrameHandler = null;
  }

  /**
   * Inject a mouse event via CDP
   */
  async injectMouseEvent(params: {
    type: 'mousePressed' | 'mouseReleased' | 'mouseMoved' | 'mouseWheel';
    x: number;
    y: number;
    button?: 'left' | 'right' | 'middle' | 'none';
    clickCount?: number;
    deltaX?: number;
    deltaY?: number;
    modifiers?: number; // 1=Alt, 2=Ctrl, 4=Meta, 8=Shift
  }): Promise<void> {
    const cdp = await this.getCDPSession();

    const cdpButton =
      params.button === 'left'
        ? 'left'
        : params.button === 'right'
          ? 'right'
          : params.button === 'middle'
            ? 'middle'
            : 'none';

    await cdp.send('Input.dispatchMouseEvent', {
      type: params.type,
      x: params.x,
      y: params.y,
      button: cdpButton,
      clickCount: params.clickCount ?? 1,
      deltaX: params.deltaX ?? 0,
      deltaY: params.deltaY ?? 0,
      modifiers: params.modifiers ?? 0,
    });
  }

  /**
   * Inject a keyboard event via CDP
   */
  async injectKeyboardEvent(params: {
    type: 'keyDown' | 'keyUp' | 'char';
    key?: string;
    code?: string;
    text?: string;
    modifiers?: number; // 1=Alt, 2=Ctrl, 4=Meta, 8=Shift
  }): Promise<void> {
    const cdp = await this.getCDPSession();

    await cdp.send('Input.dispatchKeyEvent', {
      type: params.type,
      key: params.key,
      code: params.code,
      text: params.text,
      modifiers: params.modifiers ?? 0,
    });
  }

  /**
   * Inject touch event via CDP (for mobile emulation)
   */
  async injectTouchEvent(params: {
    type: 'touchStart' | 'touchEnd' | 'touchMove' | 'touchCancel';
    touchPoints: Array<{ x: number; y: number; id?: number }>;
    modifiers?: number;
  }): Promise<void> {
    const cdp = await this.getCDPSession();

    await cdp.send('Input.dispatchTouchEvent', {
      type: params.type,
      touchPoints: params.touchPoints.map((tp, i) => ({
        x: tp.x,
        y: tp.y,
        id: tp.id ?? i,
      })),
      modifiers: params.modifiers ?? 0,
    });
  }

  /**
   * Check if video recording is currently active
   */
  isRecording(): boolean {
    return this.recordingContext !== null;
  }

  /**
   * Start recording to a video file using Playwright's native video recording.
   * Creates a fresh browser context with video recording enabled.
   * Automatically captures current URL and transfers cookies/storage if no URL provided.
   *
   * @param outputPath - Path to the output video file (will be .webm)
   * @param url - Optional URL to navigate to (defaults to current page URL)
   */
  async startRecording(outputPath: string, url?: string): Promise<void> {
    if (this.recordingContext) {
      throw new Error(
        "Recording already in progress. Run 'record stop' first, or use 'record restart' to stop and start a new recording."
      );
    }

    if (!this.browser) {
      throw new Error('Browser not launched. Call launch first.');
    }

    // Check if output file already exists
    if (existsSync(outputPath)) {
      throw new Error(`Output file already exists: ${outputPath}`);
    }

    // Validate output path is .webm (Playwright native format)
    if (!outputPath.endsWith('.webm')) {
      throw new Error(
        'Playwright native recording only supports WebM format. Please use a .webm extension.'
      );
    }

    // Auto-capture current URL if none provided
    const currentPage = this.pages.length > 0 ? this.pages[this.activePageIndex] : null;
    const currentContext = this.contexts.length > 0 ? this.contexts[0] : null;
    if (!url && currentPage) {
      const currentUrl = currentPage.url();
      if (currentUrl && currentUrl !== 'about:blank') {
        url = currentUrl;
      }
    }

    // Capture state from current context (cookies + storage)
    let storageState:
      | {
          cookies: Array<{
            name: string;
            value: string;
            domain: string;
            path: string;
            expires: number;
            httpOnly: boolean;
            secure: boolean;
            sameSite: 'Strict' | 'Lax' | 'None';
          }>;
          origins: Array<{
            origin: string;
            localStorage: Array<{ name: string; value: string }>;
          }>;
        }
      | undefined;

    if (currentContext) {
      try {
        storageState = await currentContext.storageState();
      } catch {
        // Ignore errors - context might be closed or invalid
      }
    }

    // Create a temp directory for video recording
    const session = process.env.AGENT_BROWSER_SESSION || 'default';
    this.recordingTempDir = path.join(
      os.tmpdir(),
      `agent-browser-recording-${session}-${Date.now()}`
    );
    mkdirSync(this.recordingTempDir, { recursive: true });

    this.recordingOutputPath = outputPath;

    // Create a new context with video recording enabled and restored state
    const viewport = { width: 1280, height: 720 };
    this.recordingContext = await this.browser.newContext({
      viewport,
      recordVideo: {
        dir: this.recordingTempDir,
        size: viewport,
      },
      storageState,
    });
    this.recordingContext.setDefaultTimeout(10000);

    // Create a page in the recording context
    this.recordingPage = await this.recordingContext.newPage();

    // Add the recording context and page to our managed lists
    this.contexts.push(this.recordingContext);
    this.pages.push(this.recordingPage);
    this.activePageIndex = this.pages.length - 1;

    // Set up page tracking
    this.setupPageTracking(this.recordingPage);

    // Invalidate CDP session since we switched pages
    await this.invalidateCDPSession();

    // Navigate to URL if provided or captured
    if (url) {
      await this.recordingPage.goto(url, { waitUntil: 'load' });
    }
  }

  /**
   * Stop recording and save the video file
   * @returns Recording result with path
   */
  async stopRecording(): Promise<{ path: string; frames: number; error?: string }> {
    if (!this.recordingContext || !this.recordingPage) {
      return { path: '', frames: 0, error: 'No recording in progress' };
    }

    const outputPath = this.recordingOutputPath;

    try {
      // Get the video object before closing the page
      const video = this.recordingPage.video();

      // Remove recording page/context from our managed lists before closing
      const pageIndex = this.pages.indexOf(this.recordingPage);
      if (pageIndex !== -1) {
        this.pages.splice(pageIndex, 1);
      }
      const contextIndex = this.contexts.indexOf(this.recordingContext);
      if (contextIndex !== -1) {
        this.contexts.splice(contextIndex, 1);
      }

      // Close the page to finalize the video
      await this.recordingPage.close();

      // Save the video to the desired output path
      if (video) {
        await video.saveAs(outputPath);
      }

      // Clean up temp directory
      if (this.recordingTempDir) {
        rmSync(this.recordingTempDir, { recursive: true, force: true });
      }

      // Close the recording context
      await this.recordingContext.close();

      // Reset recording state
      this.recordingContext = null;
      this.recordingPage = null;
      this.recordingOutputPath = '';
      this.recordingTempDir = '';

      // Adjust active page index
      if (this.pages.length > 0) {
        this.activePageIndex = Math.min(this.activePageIndex, this.pages.length - 1);
      } else {
        this.activePageIndex = 0;
      }

      // Invalidate CDP session since we may have switched pages
      await this.invalidateCDPSession();

      return { path: outputPath, frames: 0 }; // Playwright doesn't expose frame count
    } catch (error) {
      // Clean up temp directory on error
      if (this.recordingTempDir) {
        rmSync(this.recordingTempDir, { recursive: true, force: true });
      }

      // Reset state on error
      this.recordingContext = null;
      this.recordingPage = null;
      this.recordingOutputPath = '';
      this.recordingTempDir = '';

      const message = error instanceof Error ? error.message : String(error);
      return { path: outputPath, frames: 0, error: message };
    }
  }

  /**
   * Restart recording - stops current recording (if any) and starts a new one.
   * Convenience method that combines stopRecording and startRecording.
   *
   * @param outputPath - Path to the output video file (must be .webm)
   * @param url - Optional URL to navigate to (defaults to current page URL)
   * @returns Result from stopping the previous recording (if any)
   */
  async restartRecording(
    outputPath: string,
    url?: string
  ): Promise<{ previousPath?: string; stopped: boolean }> {
    let previousPath: string | undefined;
    let stopped = false;

    // Stop current recording if active
    if (this.recordingContext) {
      const result = await this.stopRecording();
      previousPath = result.path;
      stopped = true;
    }

    // Start new recording
    await this.startRecording(outputPath, url);

    return { previousPath, stopped };
  }

  /**
   * Close the browser and clean up
   */
  async close(): Promise<void> {
    // Stop recording if active (saves video)
    if (this.recordingContext) {
      await this.stopRecording();
    }

    // Stop screencast if active
    if (this.screencastActive) {
      await this.stopScreencast();
    }

    // Clean up CDP session
    if (this.cdpSession) {
      await this.cdpSession.detach().catch(() => {});
      this.cdpSession = null;
    }

    // CDP: only disconnect, don't close external app's pages
    if (this.cdpPort !== null) {
      if (this.browser) {
        await this.browser.close().catch(() => {});
        this.browser = null;
      }
    } else {
      // Regular browser: close everything
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
    this.isPersistentContext = false;
    this.activePageIndex = 0;
    this.refMap = {};
    this.lastSnapshot = '';
    this.frameCallback = null;
  }
}
