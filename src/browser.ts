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
import { existsSync, mkdirSync, rmSync, readFileSync } from 'node:fs';
import { writeFile, mkdir } from 'node:fs/promises';
import type { LaunchCommand, TraceEvent } from './types.js';
import { type RefMap, type EnhancedSnapshot, getEnhancedSnapshot, parseRef } from './snapshot.js';
import { safeHeaderMerge } from './state-utils.js';
import {
  getEncryptionKey,
  isEncryptedPayload,
  decryptData,
  ENCRYPTION_KEY_ENV,
} from './state-utils.js';

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
  private cdpEndpoint: string | null = null; // stores port number or full URL
  private isPersistentContext: boolean = false;
  private browserbaseSessionId: string | null = null;
  private browserbaseApiKey: string | null = null;
  private browserUseSessionId: string | null = null;
  private browserUseApiKey: string | null = null;
  private kernelSessionId: string | null = null;
  private kernelApiKey: string | null = null;
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
  private colorScheme: 'light' | 'dark' | 'no-preference' | null = null;

  /**
   * Set the persistent color scheme preference.
   * Applied automatically to all new pages and contexts.
   */
  setColorScheme(scheme: 'light' | 'dark' | 'no-preference' | null): void {
    this.colorScheme = scheme;
  }

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
  private launchWarnings: string[] = [];

  /**
   * Get and clear launch warnings (e.g., decryption failures)
   */
  getAndClearWarnings(): string[] {
    const warnings = this.launchWarnings;
    this.launchWarnings = [];
    return warnings;
  }

  // CDP profiling state
  private static readonly MAX_PROFILE_EVENTS = 5_000_000;
  private profilingActive: boolean = false;
  private profileChunks: TraceEvent[] = [];
  private profileEventsDropped: boolean = false;
  private profileCompleteResolver: (() => void) | null = null;
  private profileDataHandler: ((params: { value?: TraceEvent[] }) => void) | null = null;
  private profileCompleteHandler: (() => void) | null = null;

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
    cursor?: boolean;
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
   * Get the last snapshot tree text (empty string if no snapshot has been taken)
   */
  getLastSnapshot(): string {
    return this.lastSnapshot;
  }

  /**
   * Update the stored snapshot (used by diff to keep the baseline current)
   */
  setLastSnapshot(snapshot: string): void {
    this.lastSnapshot = snapshot;
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

    // Check if this is a cursor-interactive element (uses CSS selector, not ARIA role)
    // These have pseudo-roles 'clickable' or 'focusable' and a CSS selector
    if (refData.role === 'clickable' || refData.role === 'focusable') {
      // The selector is a CSS selector, use it directly
      return page.locator(refData.selector);
    }

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
   * Check if the browser has any usable pages
   */
  hasPages(): boolean {
    return this.pages.length > 0;
  }

  /**
   * Ensure at least one page exists. If the browser is launched but all pages
   * were closed (stale session), creates a new page on the existing context.
   * No-op if pages already exist.
   */
  async ensurePage(): Promise<void> {
    if (this.pages.length > 0) return;
    if (!this.browser && !this.isPersistentContext) return;

    // Use the last existing context, or create a new one
    let context: BrowserContext;
    if (this.contexts.length > 0) {
      context = this.contexts[this.contexts.length - 1];
    } else if (this.browser) {
      context = await this.browser.newContext({
        ...(this.colorScheme && { colorScheme: this.colorScheme }),
      });
      context.setDefaultTimeout(60000);
      this.contexts.push(context);
      this.setupContextTracking(context);
    } else {
      return;
    }

    const page = await context.newPage();
    if (!this.pages.includes(page)) {
      this.pages.push(page);
      this.setupPageTracking(page);
    }
    this.activePageIndex = this.pages.length - 1;
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
   * Set device scale factor (devicePixelRatio) via CDP
   * This sets window.devicePixelRatio which affects how the page renders and responds to media queries
   *
   * Note: When using CDP to set deviceScaleFactor, screenshots will be at logical pixel dimensions
   * (viewport size), not physical pixel dimensions (viewport × scale). This is a Playwright limitation
   * when using CDP emulation on existing contexts. For true HiDPI screenshots with physical pixels,
   * deviceScaleFactor must be set at context creation time.
   *
   * Must be called after setViewport to work correctly
   */
  async setDeviceScaleFactor(
    deviceScaleFactor: number,
    width: number,
    height: number,
    mobile: boolean = false
  ): Promise<void> {
    const cdp = await this.getCDPSession();
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width,
      height,
      deviceScaleFactor,
      mobile,
    });
  }

  /**
   * Clear device metrics override to restore default devicePixelRatio
   */
  async clearDeviceMetricsOverride(): Promise<void> {
    const cdp = await this.getCDPSession();
    await cdp.send('Emulation.clearDeviceMetricsOverride');
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
        headers: safeHeaderMerge(requestHeaders, headers),
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
  async stopTracing(path?: string): Promise<void> {
    const context = this.contexts[0];
    if (context) {
      await context.tracing.stop(path ? { path } : undefined);
    }
  }

  /**
   * Get the current browser context (first context)
   */
  getContext(): BrowserContext | null {
    return this.contexts[0] ?? null;
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
  private needsCdpReconnect(cdpEndpoint: string): boolean {
    if (!this.browser?.isConnected()) return true;
    if (this.cdpEndpoint !== cdpEndpoint) return true;
    if (!this.isCdpConnectionAlive()) return true;
    return false;
  }

  /**
   * Close a Browserbase session via API
   */
  private async closeBrowserbaseSession(sessionId: string, apiKey: string): Promise<void> {
    await fetch(`https://api.browserbase.com/v1/sessions/${sessionId}`, {
      method: 'DELETE',
      headers: {
        'X-BB-API-Key': apiKey,
      },
    });
  }

  /**
   * Close a Browser Use session via API
   */
  private async closeBrowserUseSession(sessionId: string, apiKey: string): Promise<void> {
    const response = await fetch(`https://api.browser-use.com/api/v2/browsers/${sessionId}`, {
      method: 'PATCH',
      headers: {
        'Content-Type': 'application/json',
        'X-Browser-Use-API-Key': apiKey,
      },
      body: JSON.stringify({ action: 'stop' }),
    });

    if (!response.ok) {
      throw new Error(`Failed to close Browser Use session: ${response.statusText}`);
    }
  }

  /**
   * Close a Kernel session via API
   */
  private async closeKernelSession(sessionId: string, apiKey: string): Promise<void> {
    const response = await fetch(`https://api.onkernel.com/browsers/${sessionId}`, {
      method: 'DELETE',
      headers: {
        Authorization: `Bearer ${apiKey}`,
      },
    });

    if (!response.ok) {
      throw new Error(`Failed to close Kernel session: ${response.statusText}`);
    }
  }

  /**
   * Connect to Browserbase remote browser via CDP.
   * Requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID environment variables.
   */
  private async connectToBrowserbase(): Promise<void> {
    const browserbaseApiKey = process.env.BROWSERBASE_API_KEY;
    const browserbaseProjectId = process.env.BROWSERBASE_PROJECT_ID;

    if (!browserbaseApiKey || !browserbaseProjectId) {
      throw new Error(
        'BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID are required when using browserbase as a provider'
      );
    }

    const response = await fetch('https://api.browserbase.com/v1/sessions', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-BB-API-Key': browserbaseApiKey,
      },
      body: JSON.stringify({
        projectId: browserbaseProjectId,
      }),
    });

    if (!response.ok) {
      throw new Error(`Failed to create Browserbase session: ${response.statusText}`);
    }

    const session = (await response.json()) as { id: string; connectUrl: string };

    const browser = await chromium.connectOverCDP(session.connectUrl).catch(() => {
      throw new Error('Failed to connect to Browserbase session via CDP');
    });

    try {
      const contexts = browser.contexts();
      if (contexts.length === 0) {
        throw new Error('No browser context found in Browserbase session');
      }

      const context = contexts[0];
      const pages = context.pages();
      const page = pages[0] ?? (await context.newPage());

      this.browserbaseSessionId = session.id;
      this.browserbaseApiKey = browserbaseApiKey;
      this.browser = browser;
      context.setDefaultTimeout(10000);
      this.contexts.push(context);
      this.setupContextTracking(context);
      this.pages.push(page);
      this.activePageIndex = 0;
      this.setupPageTracking(page);
    } catch (error) {
      await this.closeBrowserbaseSession(session.id, browserbaseApiKey).catch((sessionError) => {
        console.error('Failed to close Browserbase session during cleanup:', sessionError);
      });
      throw error;
    }
  }

  /**
   * Find or create a Kernel profile by name.
   * Returns the profile object if successful.
   */
  private async findOrCreateKernelProfile(
    profileName: string,
    apiKey: string
  ): Promise<{ name: string }> {
    // First, try to get the existing profile
    const getResponse = await fetch(
      `https://api.onkernel.com/profiles/${encodeURIComponent(profileName)}`,
      {
        method: 'GET',
        headers: {
          Authorization: `Bearer ${apiKey}`,
        },
      }
    );

    if (getResponse.ok) {
      // Profile exists, return it
      return { name: profileName };
    }

    if (getResponse.status !== 404) {
      throw new Error(`Failed to check Kernel profile: ${getResponse.statusText}`);
    }

    // Profile doesn't exist, create it
    const createResponse = await fetch('https://api.onkernel.com/profiles', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${apiKey}`,
      },
      body: JSON.stringify({ name: profileName }),
    });

    if (!createResponse.ok) {
      throw new Error(`Failed to create Kernel profile: ${createResponse.statusText}`);
    }

    return { name: profileName };
  }

  /**
   * Connect to Kernel remote browser via CDP.
   * Requires KERNEL_API_KEY environment variable.
   */
  private async connectToKernel(): Promise<void> {
    const kernelApiKey = process.env.KERNEL_API_KEY;
    if (!kernelApiKey) {
      throw new Error('KERNEL_API_KEY is required when using kernel as a provider');
    }

    // Find or create profile if KERNEL_PROFILE_NAME is set
    const profileName = process.env.KERNEL_PROFILE_NAME;
    let profileConfig: { profile: { name: string; save_changes: boolean } } | undefined;

    if (profileName) {
      await this.findOrCreateKernelProfile(profileName, kernelApiKey);
      profileConfig = {
        profile: {
          name: profileName,
          save_changes: true, // Save cookies/state back to the profile when session ends
        },
      };
    }

    const response = await fetch('https://api.onkernel.com/browsers', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${kernelApiKey}`,
      },
      body: JSON.stringify({
        // Kernel browsers are headful by default with stealth mode available
        // The user can configure these via environment variables if needed
        headless: process.env.KERNEL_HEADLESS?.toLowerCase() === 'true',
        stealth: process.env.KERNEL_STEALTH?.toLowerCase() !== 'false', // Default to stealth mode
        timeout_seconds: parseInt(process.env.KERNEL_TIMEOUT_SECONDS || '300', 10),
        // Load and save to a profile if specified
        ...profileConfig,
      }),
    });

    if (!response.ok) {
      throw new Error(`Failed to create Kernel session: ${response.statusText}`);
    }

    let session: { session_id: string; cdp_ws_url: string };
    try {
      session = (await response.json()) as { session_id: string; cdp_ws_url: string };
    } catch (error) {
      throw new Error(
        `Failed to parse Kernel session response: ${error instanceof Error ? error.message : String(error)}`
      );
    }

    if (!session.session_id || !session.cdp_ws_url) {
      throw new Error(
        `Invalid Kernel session response: missing ${!session.session_id ? 'session_id' : 'cdp_ws_url'}`
      );
    }

    const browser = await chromium.connectOverCDP(session.cdp_ws_url).catch(() => {
      throw new Error('Failed to connect to Kernel session via CDP');
    });

    try {
      const contexts = browser.contexts();
      let context: BrowserContext;
      let page: Page;

      // Kernel browsers launch with a default context and page
      if (contexts.length === 0) {
        context = await browser.newContext();
        page = await context.newPage();
      } else {
        context = contexts[0];
        const pages = context.pages();
        page = pages[0] ?? (await context.newPage());
      }

      this.kernelSessionId = session.session_id;
      this.kernelApiKey = kernelApiKey;
      this.browser = browser;
      context.setDefaultTimeout(60000);
      this.contexts.push(context);
      this.pages.push(page);
      this.activePageIndex = 0;
      this.setupPageTracking(page);
      this.setupContextTracking(context);
    } catch (error) {
      await this.closeKernelSession(session.session_id, kernelApiKey).catch((sessionError) => {
        console.error('Failed to close Kernel session during cleanup:', sessionError);
      });
      throw error;
    }
  }

  /**
   * Connect to Browser Use remote browser via CDP.
   * Requires BROWSER_USE_API_KEY environment variable.
   */
  private async connectToBrowserUse(): Promise<void> {
    const browserUseApiKey = process.env.BROWSER_USE_API_KEY;
    if (!browserUseApiKey) {
      throw new Error('BROWSER_USE_API_KEY is required when using browseruse as a provider');
    }

    const response = await fetch('https://api.browser-use.com/api/v2/browsers', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Browser-Use-API-Key': browserUseApiKey,
      },
      body: JSON.stringify({}),
    });

    if (!response.ok) {
      throw new Error(`Failed to create Browser Use session: ${response.statusText}`);
    }

    let session: { id: string; cdpUrl: string };
    try {
      session = (await response.json()) as { id: string; cdpUrl: string };
    } catch (error) {
      throw new Error(
        `Failed to parse Browser Use session response: ${error instanceof Error ? error.message : String(error)}`
      );
    }

    if (!session.id || !session.cdpUrl) {
      throw new Error(
        `Invalid Browser Use session response: missing ${!session.id ? 'id' : 'cdpUrl'}`
      );
    }

    const browser = await chromium.connectOverCDP(session.cdpUrl).catch(() => {
      throw new Error('Failed to connect to Browser Use session via CDP');
    });

    try {
      const contexts = browser.contexts();
      let context: BrowserContext;
      let page: Page;

      if (contexts.length === 0) {
        context = await browser.newContext();
        page = await context.newPage();
      } else {
        context = contexts[0];
        const pages = context.pages();
        page = pages[0] ?? (await context.newPage());
      }

      this.browserUseSessionId = session.id;
      this.browserUseApiKey = browserUseApiKey;
      this.browser = browser;
      context.setDefaultTimeout(60000);
      this.contexts.push(context);
      this.pages.push(page);
      this.activePageIndex = 0;
      this.setupPageTracking(page);
      this.setupContextTracking(context);
    } catch (error) {
      await this.closeBrowserUseSession(session.id, browserUseApiKey).catch((sessionError) => {
        console.error('Failed to close Browser Use session during cleanup:', sessionError);
      });
      throw error;
    }
  }

  /**
   * Launch the browser with the specified options
   * If already launched, this is a no-op (browser stays open)
   */
  async launch(options: LaunchCommand): Promise<void> {
    // Determine CDP endpoint: prefer cdpUrl over cdpPort for flexibility
    const cdpEndpoint = options.cdpUrl ?? (options.cdpPort ? String(options.cdpPort) : undefined);
    const hasExtensions = !!options.extensions?.length;
    const hasProfile = !!options.profile;
    const hasStorageState = !!options.storageState;

    if (hasExtensions && cdpEndpoint) {
      throw new Error('Extensions cannot be used with CDP connection');
    }

    if (hasProfile && cdpEndpoint) {
      throw new Error('Profile cannot be used with CDP connection');
    }

    if (hasStorageState && hasProfile) {
      throw new Error(
        'Storage state cannot be used with profile (profile is already persistent storage)'
      );
    }

    if (hasStorageState && hasExtensions) {
      throw new Error(
        'Storage state cannot be used with extensions (extensions require persistent context)'
      );
    }

    if (this.isLaunched()) {
      const needsRelaunch =
        (!cdpEndpoint && !options.autoConnect && this.cdpEndpoint !== null) ||
        (!!cdpEndpoint && this.needsCdpReconnect(cdpEndpoint)) ||
        (!!options.autoConnect && !this.isCdpConnectionAlive());
      if (needsRelaunch) {
        await this.close();
      } else if (options.autoConnect && this.isCdpConnectionAlive()) {
        // Already connected via auto-connect, no need to reconnect
        return;
      } else {
        return;
      }
    }

    if (options.colorScheme) {
      this.colorScheme = options.colorScheme;
    }

    if (cdpEndpoint) {
      await this.connectViaCDP(cdpEndpoint);
      return;
    }

    if (options.autoConnect) {
      await this.autoConnectViaCDP();
      return;
    }

    // Cloud browser providers require explicit opt-in via -p flag or AGENT_BROWSER_PROVIDER env var
    // -p flag takes precedence over env var
    const provider = options.provider ?? process.env.AGENT_BROWSER_PROVIDER;
    if (provider === 'browserbase') {
      await this.connectToBrowserbase();
      return;
    }
    if (provider === 'browseruse') {
      await this.connectToBrowserUse();
      return;
    }

    // Kernel: requires explicit opt-in via -p kernel flag or AGENT_BROWSER_PROVIDER=kernel
    if (provider === 'kernel') {
      await this.connectToKernel();
      return;
    }

    const browserType = options.browser ?? 'chromium';
    if (hasExtensions && browserType !== 'chromium') {
      throw new Error('Extensions are only supported in Chromium');
    }

    // allowFileAccess is only supported in Chromium
    if (options.allowFileAccess && browserType !== 'chromium') {
      throw new Error('allowFileAccess is only supported in Chromium');
    }

    const launcher =
      browserType === 'firefox' ? firefox : browserType === 'webkit' ? webkit : chromium;

    // Build base args array with file access flags if enabled
    // --allow-file-access-from-files: allows file:// URLs to read other file:// URLs via XHR/fetch
    // --allow-file-access: allows the browser to access local files in general
    const fileAccessArgs = options.allowFileAccess
      ? ['--allow-file-access-from-files', '--allow-file-access']
      : [];
    const baseArgs = options.args
      ? [...fileAccessArgs, ...options.args]
      : fileAccessArgs.length > 0
        ? fileAccessArgs
        : undefined;

    // Auto-detect args that control window size and disable viewport emulation
    // so Playwright doesn't override the browser's own sizing behavior
    const hasWindowSizeArgs = baseArgs?.some(
      (arg) => arg === '--start-maximized' || arg.startsWith('--window-size=')
    );
    const viewport =
      options.viewport !== undefined
        ? options.viewport
        : hasWindowSizeArgs
          ? null
          : { width: 1280, height: 720 };

    let context: BrowserContext;
    if (hasExtensions) {
      // Extensions require persistent context in a temp directory
      const extPaths = options.extensions!.join(',');
      const session = process.env.AGENT_BROWSER_SESSION || 'default';
      // Combine extension args with custom args and file access args
      const extArgs = [`--disable-extensions-except=${extPaths}`, `--load-extension=${extPaths}`];
      const allArgs = baseArgs ? [...extArgs, ...baseArgs] : extArgs;
      context = await launcher.launchPersistentContext(
        path.join(os.tmpdir(), `agent-browser-ext-${session}`),
        {
          headless: false,
          executablePath: options.executablePath,
          args: allArgs,
          viewport,
          extraHTTPHeaders: options.headers,
          userAgent: options.userAgent,
          ...(options.proxy && { proxy: options.proxy }),
          ignoreHTTPSErrors: options.ignoreHTTPSErrors ?? false,
          ...(this.colorScheme && { colorScheme: this.colorScheme }),
        }
      );
      this.isPersistentContext = true;
    } else if (hasProfile) {
      // Profile uses persistent context for durable cookies/storage
      // Expand ~ to home directory since it won't be shell-expanded
      const profilePath = options.profile!.replace(/^~\//, os.homedir() + '/');
      context = await launcher.launchPersistentContext(profilePath, {
        headless: options.headless ?? true,
        executablePath: options.executablePath,
        args: baseArgs,
        viewport,
        extraHTTPHeaders: options.headers,
        userAgent: options.userAgent,
        ...(options.proxy && { proxy: options.proxy }),
        ignoreHTTPSErrors: options.ignoreHTTPSErrors ?? false,
        ...(this.colorScheme && { colorScheme: this.colorScheme }),
      });
      this.isPersistentContext = true;
    } else {
      // Regular ephemeral browser
      this.browser = await launcher.launch({
        headless: options.headless ?? true,
        executablePath: options.executablePath,
        args: baseArgs,
      });
      this.cdpEndpoint = null;

      // Check for auto-load state file (supports encrypted files)
      let storageState:
        | string
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
        | undefined = options.storageState ? options.storageState : undefined;

      if (!storageState && options.autoStateFilePath) {
        try {
          const fs = await import('fs');
          if (fs.existsSync(options.autoStateFilePath)) {
            const content = fs.readFileSync(options.autoStateFilePath, 'utf8');
            const parsed = JSON.parse(content);

            if (isEncryptedPayload(parsed)) {
              const key = getEncryptionKey();
              if (key) {
                try {
                  const decrypted = decryptData(parsed, key);
                  storageState = JSON.parse(decrypted);
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(
                      `[DEBUG] Auto-loading session state (decrypted): ${options.autoStateFilePath}`
                    );
                  }
                } catch (decryptErr) {
                  const warning =
                    'Failed to decrypt state file - wrong encryption key? Starting fresh.';
                  this.launchWarnings.push(warning);
                  console.error(`[WARN] ${warning}`);
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(`[DEBUG] Decryption error:`, decryptErr);
                  }
                }
              } else {
                const warning = `State file is encrypted but ${ENCRYPTION_KEY_ENV} not set - starting fresh`;
                this.launchWarnings.push(warning);
                console.error(`[WARN] ${warning}`);
              }
            } else {
              storageState = options.autoStateFilePath;
              if (process.env.AGENT_BROWSER_DEBUG === '1') {
                console.error(`[DEBUG] Auto-loading session state: ${options.autoStateFilePath}`);
              }
            }
          }
        } catch (err) {
          if (process.env.AGENT_BROWSER_DEBUG === '1') {
            console.error(`[DEBUG] Failed to load state file, starting fresh:`, err);
          }
        }
      }

      context = await this.browser.newContext({
        viewport,
        extraHTTPHeaders: options.headers,
        userAgent: options.userAgent,
        storageState,
        ...(options.proxy && { proxy: options.proxy }),
        ignoreHTTPSErrors: options.ignoreHTTPSErrors ?? false,
        ...(this.colorScheme && { colorScheme: this.colorScheme }),
      });
    }

    context.setDefaultTimeout(60000);
    this.contexts.push(context);
    this.setupContextTracking(context);

    const page = context.pages()[0] ?? (await context.newPage());
    // Only add if not already tracked (setupContextTracking may have already added it via 'page' event)
    if (!this.pages.includes(page)) {
      this.pages.push(page);
      this.setupPageTracking(page);
    }
    this.activePageIndex = this.pages.length > 0 ? this.pages.length - 1 : 0;
  }

  /**
   * Connect to a running browser via CDP (Chrome DevTools Protocol)
   * @param cdpEndpoint Either a port number (as string) or a full WebSocket URL (ws:// or wss://)
   */
  private async connectViaCDP(
    cdpEndpoint: string | undefined,
    options?: { timeout?: number }
  ): Promise<void> {
    if (!cdpEndpoint) {
      throw new Error('CDP endpoint is required for CDP connection');
    }

    // Determine the connection URL:
    // - If it starts with ws://, wss://, http://, or https://, use it directly
    // - If it's a numeric string (e.g., "9222"), treat as port for localhost
    // - Otherwise, treat it as a port number for localhost
    let cdpUrl: string;
    if (
      cdpEndpoint.startsWith('ws://') ||
      cdpEndpoint.startsWith('wss://') ||
      cdpEndpoint.startsWith('http://') ||
      cdpEndpoint.startsWith('https://')
    ) {
      cdpUrl = cdpEndpoint;
    } else if (/^\d+$/.test(cdpEndpoint)) {
      // Numeric string - treat as port number (handles JSON serialization quirks)
      cdpUrl = `http://localhost:${cdpEndpoint}`;
    } else {
      // Unknown format - still try as port for backward compatibility
      cdpUrl = `http://localhost:${cdpEndpoint}`;
    }

    const browser = await chromium
      .connectOverCDP(cdpUrl, { timeout: options?.timeout })
      .catch(() => {
        throw new Error(
          `Failed to connect via CDP to ${cdpUrl}. ` +
            (cdpUrl.includes('localhost')
              ? `Make sure the app is running with --remote-debugging-port=${cdpEndpoint}`
              : 'Make sure the remote browser is accessible and the URL is correct.')
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
      this.cdpEndpoint = cdpEndpoint;

      for (const context of contexts) {
        context.setDefaultTimeout(10000);
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
   * Get Chrome's default user data directory paths for the current platform.
   * Returns an array of candidate paths to check (stable, then beta/canary).
   */
  private getChromeUserDataDirs(): string[] {
    const home = os.homedir();
    const platform = os.platform();

    if (platform === 'darwin') {
      return [
        path.join(home, 'Library', 'Application Support', 'Google', 'Chrome'),
        path.join(home, 'Library', 'Application Support', 'Google', 'Chrome Canary'),
        path.join(home, 'Library', 'Application Support', 'Chromium'),
      ];
    } else if (platform === 'win32') {
      const localAppData = process.env.LOCALAPPDATA ?? path.join(home, 'AppData', 'Local');
      return [
        path.join(localAppData, 'Google', 'Chrome', 'User Data'),
        path.join(localAppData, 'Google', 'Chrome SxS', 'User Data'),
        path.join(localAppData, 'Chromium', 'User Data'),
      ];
    } else {
      // Linux
      return [
        path.join(home, '.config', 'google-chrome'),
        path.join(home, '.config', 'google-chrome-unstable'),
        path.join(home, '.config', 'chromium'),
      ];
    }
  }

  /**
   * Try to read the DevToolsActivePort file from a Chrome user data directory.
   * Returns { port, wsPath } if found, or null if not available.
   */
  private readDevToolsActivePort(userDataDir: string): { port: number; wsPath: string } | null {
    const filePath = path.join(userDataDir, 'DevToolsActivePort');
    try {
      if (!existsSync(filePath)) return null;
      const content = readFileSync(filePath, 'utf-8').trim();
      const lines = content.split('\n');
      if (lines.length < 2) return null;

      const port = parseInt(lines[0].trim(), 10);
      const wsPath = lines[1].trim();

      if (isNaN(port) || port <= 0 || port > 65535) return null;
      if (!wsPath) return null;

      return { port, wsPath };
    } catch {
      return null;
    }
  }

  /**
   * Try to discover a Chrome CDP endpoint by querying an HTTP debug port.
   * Returns the WebSocket debugger URL if available.
   */
  private async probeDebugPort(port: number): Promise<string | null> {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`, {
        signal: AbortSignal.timeout(2000),
      });
      if (!response.ok) return null;
      const data = (await response.json()) as { webSocketDebuggerUrl?: string };
      return data.webSocketDebuggerUrl ?? null;
    } catch {
      return null;
    }
  }

  /**
   * Auto-discover and connect to a running Chrome/Chromium instance.
   *
   * Discovery strategy:
   * 1. Read DevToolsActivePort from Chrome's default user data directories
   * 2. If found, connect using the port and WebSocket path from that file
   * 3. If not found, probe common debugging ports (9222, 9229)
   * 4. If a port responds, connect via CDP
   */
  private async autoConnectViaCDP(): Promise<void> {
    // Strategy 1: Check DevToolsActivePort files
    const userDataDirs = this.getChromeUserDataDirs();
    for (const dir of userDataDirs) {
      const activePort = this.readDevToolsActivePort(dir);
      if (activePort) {
        // Try HTTP discovery first (works with --remote-debugging-port mode)
        const wsUrl = await this.probeDebugPort(activePort.port);
        if (wsUrl) {
          await this.connectViaCDP(wsUrl);
          return;
        }
        // HTTP probe failed -- Chrome M144+ chrome://inspect remote debugging uses a
        // WebSocket-only server with no HTTP endpoints. Connect using the WebSocket
        // path read directly from DevToolsActivePort.
        const directWsUrl = `ws://127.0.0.1:${activePort.port}${activePort.wsPath}`;
        try {
          if (process.env.AGENT_BROWSER_DEBUG === '1') {
            console.error(
              `[DEBUG] HTTP probe failed on port ${activePort.port}, ` +
                `attempting direct WebSocket connection to ${directWsUrl}`
            );
          }
          await this.connectViaCDP(directWsUrl, { timeout: 60_000 });
          return;
        } catch {
          // Direct WebSocket also failed, try next directory
        }
      }
    }

    // Strategy 2: Probe common debugging ports
    const commonPorts = [9222, 9229];
    for (const port of commonPorts) {
      const wsUrl = await this.probeDebugPort(port);
      if (wsUrl) {
        await this.connectViaCDP(wsUrl);
        return;
      }
    }

    // Nothing found
    const platform = os.platform();
    let hint: string;
    if (platform === 'darwin') {
      hint =
        'Start Chrome with: /Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome --remote-debugging-port=9222\n' +
        'Or enable remote debugging in Chrome 144+ at chrome://inspect/#remote-debugging';
    } else if (platform === 'win32') {
      hint =
        'Start Chrome with: chrome.exe --remote-debugging-port=9222\n' +
        'Or enable remote debugging in Chrome 144+ at chrome://inspect/#remote-debugging';
    } else {
      hint =
        'Start Chrome with: google-chrome --remote-debugging-port=9222\n' +
        'Or enable remote debugging in Chrome 144+ at chrome://inspect/#remote-debugging';
    }

    throw new Error(`No running Chrome instance with remote debugging found.\n${hint}`);
  }

  /**
   * Set up console, error, and close tracking for a page
   */
  private setupPageTracking(page: Page): void {
    if (this.colorScheme) {
      page.emulateMedia({ colorScheme: this.colorScheme }).catch(() => {});
    }

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
   * Set up tracking for new pages in a context (for CDP connections and popups/new tabs)
   * This handles pages created externally (e.g., via target="_blank" links, window.open)
   */
  private setupContextTracking(context: BrowserContext): void {
    context.on('page', (page) => {
      // Only add if not already tracked (avoids duplicates when newTab() creates pages)
      if (!this.pages.includes(page)) {
        this.pages.push(page);
        this.setupPageTracking(page);
      }

      // Auto-switch to the newly opened tab so subsequent commands target it.
      // For tabs created via newTab()/newWindow(), this is redundant (they set activePageIndex after),
      // but for externally opened tabs (window.open, target="_blank"), this ensures the active tab
      // stays in sync with the browser.
      const newIndex = this.pages.indexOf(page);
      if (newIndex !== -1 && newIndex !== this.activePageIndex) {
        this.activePageIndex = newIndex;
        // Invalidate CDP session since the active page changed
        this.invalidateCDPSession().catch(() => {});
      }
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
    // Only add if not already tracked (setupContextTracking may have already added it via 'page' event)
    if (!this.pages.includes(page)) {
      this.pages.push(page);
      this.setupPageTracking(page);
    }
    this.activePageIndex = this.pages.length - 1;

    return { index: this.activePageIndex, total: this.pages.length };
  }

  /**
   * Create a new window (new context)
   */
  async newWindow(viewport?: { width: number; height: number } | null): Promise<{
    index: number;
    total: number;
  }> {
    if (!this.browser) {
      throw new Error('Browser not launched');
    }

    const context = await this.browser.newContext({
      viewport: viewport === undefined ? { width: 1280, height: 720 } : viewport,
      ...(this.colorScheme && { colorScheme: this.colorScheme }),
    });
    context.setDefaultTimeout(60000);
    this.contexts.push(context);
    this.setupContextTracking(context);

    const page = await context.newPage();
    // Only add if not already tracked (setupContextTracking may have already added it via 'page' event)
    if (!this.pages.includes(page)) {
      this.pages.push(page);
      this.setupPageTracking(page);
    }
    this.activePageIndex = this.pages.length - 1;

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
   * Check if profiling is currently active
   */
  isProfilingActive(): boolean {
    return this.profilingActive;
  }

  /**
   * Start CDP profiling (Tracing)
   */
  async startProfiling(options?: { categories?: string[] }): Promise<void> {
    if (this.profilingActive) {
      throw new Error('Profiling already active');
    }

    const cdp = await this.getCDPSession();

    const dataHandler = (params: { value?: TraceEvent[] }) => {
      if (params.value) {
        for (const evt of params.value) {
          if (this.profileChunks.length >= BrowserManager.MAX_PROFILE_EVENTS) {
            if (!this.profileEventsDropped) {
              this.profileEventsDropped = true;
              console.warn(
                `Profiling: exceeded ${BrowserManager.MAX_PROFILE_EVENTS} events, dropping further data`
              );
            }
            return;
          }
          this.profileChunks.push(evt);
        }
      }
    };

    const completeHandler = () => {
      if (this.profileCompleteResolver) {
        this.profileCompleteResolver();
      }
    };

    cdp.on('Tracing.dataCollected', dataHandler);
    cdp.on('Tracing.tracingComplete', completeHandler);

    const categories = options?.categories ?? [
      'devtools.timeline',
      'disabled-by-default-devtools.timeline',
      'disabled-by-default-devtools.timeline.frame',
      'disabled-by-default-devtools.timeline.stack',
      'v8.execute',
      'disabled-by-default-v8.cpu_profiler',
      'disabled-by-default-v8.cpu_profiler.hires',
      'v8',
      'disabled-by-default-v8.runtime_stats',
      'blink',
      'blink.user_timing',
      'latencyInfo',
      'renderer.scheduler',
      'sequence_manager',
      'toplevel',
    ];

    try {
      await cdp.send('Tracing.start', {
        traceConfig: {
          includedCategories: categories,
          enableSampling: true,
        },
        transferMode: 'ReportEvents',
      });
    } catch (error) {
      cdp.off('Tracing.dataCollected', dataHandler);
      cdp.off('Tracing.tracingComplete', completeHandler);
      throw error;
    }

    // Only commit state after the CDP call succeeds
    this.profilingActive = true;
    this.profileChunks = [];
    this.profileEventsDropped = false;
    this.profileDataHandler = dataHandler;
    this.profileCompleteHandler = completeHandler;
  }

  /**
   * Stop CDP profiling and save to file
   */
  async stopProfiling(outputPath: string): Promise<{ path: string; eventCount: number }> {
    if (!this.profilingActive) {
      throw new Error('No profiling session active');
    }

    const cdp = await this.getCDPSession();

    const TRACE_TIMEOUT_MS = 30_000;
    const completePromise = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error('Profiling data collection timed out')),
        TRACE_TIMEOUT_MS
      );
      this.profileCompleteResolver = () => {
        clearTimeout(timer);
        resolve();
      };
    });

    await cdp.send('Tracing.end');

    let chunks: TraceEvent[];
    try {
      await completePromise;
      chunks = this.profileChunks;
    } finally {
      if (this.profileDataHandler) {
        cdp.off('Tracing.dataCollected', this.profileDataHandler);
      }
      if (this.profileCompleteHandler) {
        cdp.off('Tracing.tracingComplete', this.profileCompleteHandler);
      }
      this.profilingActive = false;
      this.profileChunks = [];
      this.profileEventsDropped = false;
      this.profileCompleteResolver = null;
      this.profileDataHandler = null;
      this.profileCompleteHandler = null;
    }

    const clockDomain =
      process.platform === 'linux'
        ? 'LINUX_CLOCK_MONOTONIC'
        : process.platform === 'darwin'
          ? 'MAC_MACH_ABSOLUTE_TIME'
          : undefined;

    const traceData: Record<string, unknown> = {
      traceEvents: chunks,
    };
    if (clockDomain) {
      traceData.metadata = { 'clock-domain': clockDomain };
    }

    const dir = path.dirname(outputPath);
    await mkdir(dir, { recursive: true });

    await writeFile(outputPath, JSON.stringify(traceData));

    const eventCount = chunks.length;

    return { path: outputPath, eventCount };
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

    // Clean up profiling state if active (without saving)
    if (this.profilingActive) {
      const cdp = this.cdpSession;
      if (cdp) {
        if (this.profileDataHandler) {
          cdp.off('Tracing.dataCollected', this.profileDataHandler);
        }
        if (this.profileCompleteHandler) {
          cdp.off('Tracing.tracingComplete', this.profileCompleteHandler);
        }
        await cdp.send('Tracing.end').catch(() => {});
      }
      this.profilingActive = false;
      this.profileChunks = [];
      this.profileEventsDropped = false;
      this.profileCompleteResolver = null;
      this.profileDataHandler = null;
      this.profileCompleteHandler = null;
    }

    // Clean up CDP session
    if (this.cdpSession) {
      await this.cdpSession.detach().catch(() => {});
      this.cdpSession = null;
    }

    if (this.browserbaseSessionId && this.browserbaseApiKey) {
      await this.closeBrowserbaseSession(this.browserbaseSessionId, this.browserbaseApiKey).catch(
        (error) => {
          console.error('Failed to close Browserbase session:', error);
        }
      );
      this.browser = null;
    } else if (this.browserUseSessionId && this.browserUseApiKey) {
      await this.closeBrowserUseSession(this.browserUseSessionId, this.browserUseApiKey).catch(
        (error) => {
          console.error('Failed to close Browser Use session:', error);
        }
      );
      this.browser = null;
    } else if (this.kernelSessionId && this.kernelApiKey) {
      await this.closeKernelSession(this.kernelSessionId, this.kernelApiKey).catch((error) => {
        console.error('Failed to close Kernel session:', error);
      });
      this.browser = null;
    } else if (this.cdpEndpoint !== null) {
      // CDP: only disconnect, don't close external app's pages
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
    this.cdpEndpoint = null;
    this.browserbaseSessionId = null;
    this.browserbaseApiKey = null;
    this.browserUseSessionId = null;
    this.browserUseApiKey = null;
    this.kernelSessionId = null;
    this.kernelApiKey = null;
    this.isPersistentContext = false;
    this.activePageIndex = 0;
    this.colorScheme = null;
    this.refMap = {};
    this.lastSnapshot = '';
    this.frameCallback = null;
  }
}
