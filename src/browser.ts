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

/**
 * Manages the Playwright browser lifecycle with multiple tabs/windows
 */
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

  /**
   * Check if browser is launched
   */
  isLaunched(): boolean {
    return this.browser !== null;
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

    // Select browser type
    const browserType = options.browser ?? 'chromium';
    const launcher =
      browserType === 'firefox' ? firefox : browserType === 'webkit' ? webkit : chromium;

    // Launch browser
    this.browser = await launcher.launch({
      headless: options.headless ?? true,
      executablePath: options.executablePath,
    });
    this.cdpPort = null;

    // Create context with viewport and optional headers
    const context = await this.browser.newContext({
      viewport: options.viewport ?? { width: 1280, height: 720 },
      extraHTTPHeaders: options.headers,
    });

    // Set default timeout to 10 seconds (Playwright default is 30s)
    context.setDefaultTimeout(10000);

    this.contexts.push(context);

    // Create initial page
    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = 0;

    // Automatically start console and error tracking
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

      const allPages = contexts.flatMap((context) => context.pages());
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
    context.setDefaultTimeout(10000);
    this.contexts.push(context);

    const page = await context.newPage();
    this.pages.push(page);
    this.activePageIndex = this.pages.length - 1;

    // Set up tracking for the new page
    this.setupPageTracking(page);

    return { index: this.activePageIndex, total: this.pages.length };
  }

  /**
   * Switch to a specific tab/page by index
   */
  switchTo(index: number): { index: number; url: string; title: string } {
    if (index < 0 || index >= this.pages.length) {
      throw new Error(`Invalid tab index: ${index}. Available: 0-${this.pages.length - 1}`);
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
   * Close the browser and clean up
   */
  async close(): Promise<void> {
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
    this.activePageIndex = 0;
    this.refMap = {};
    this.lastSnapshot = '';
  }
}
