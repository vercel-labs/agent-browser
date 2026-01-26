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

/* ────────────────────────────────────────────────────────────── */
/* Types & interfaces (UNCHANGED)                                 */
/* ────────────────────────────────────────────────────────────── */

export interface ScreencastFrame {
  data: string;
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

export interface ScreencastOptions {
  format?: 'jpeg' | 'png';
  quality?: number;
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

/* ────────────────────────────────────────────────────────────── */
/* BrowserManager                                                 */
/* ────────────────────────────────────────────────────────────── */

export class BrowserManager {
  private browser: Browser | null = null;
  private cdpEndpoint: string | null = null;
  private isPersistentContext = false;

  private browserbaseSessionId: string | null = null;
  private browserbaseApiKey: string | null = null;
  private browserUseSessionId: string | null = null;
  private browserUseApiKey: string | null = null;
  private kernelSessionId: string | null = null;
  private kernelApiKey: string | null = null;

  private contexts: BrowserContext[] = [];
  private pages: Page[] = [];
  private activePageIndex = 0;
  private activeFrame: Frame | null = null;

  private dialogHandler: ((dialog: Dialog) => Promise<void>) | null = null;
  private trackedRequests: TrackedRequest[] = [];
  private routes = new Map<string, (route: Route) => Promise<void>>();
  private consoleMessages: ConsoleMessage[] = [];
  private pageErrors: PageError[] = [];

  private isRecordingHar = false;
  private refMap: RefMap = {};
  private lastSnapshot = '';

  private cdpSession: CDPSession | null = null;
  private screencastActive = false;
  private frameCallback: ((frame: ScreencastFrame) => void) | null = null;
  private screencastFrameHandler: ((params: any) => void) | null = null;

  private recordingContext: BrowserContext | null = null;
  private recordingPage: Page | null = null;
  private recordingOutputPath = '';
  private recordingTempDir = '';

  /* ────────────────────────────────────────────────────────────── */
  /* Launch                                                        */
  /* ────────────────────────────────────────────────────────────── */

  async launch(options: LaunchCommand): Promise<void> {
    const cdpEndpoint =
      options.cdpUrl ?? (options.cdpPort ? String(options.cdpPort) : undefined);

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
      throw new Error('Storage state cannot be used with profile');
    }
    if (hasStorageState && hasExtensions) {
      throw new Error('Storage state cannot be used with extensions');
    }

    if (this.isLaunched()) {
      const needsRelaunch =
        (!cdpEndpoint && this.cdpEndpoint !== null) ||
        (!!cdpEndpoint && this.needsCdpReconnect(cdpEndpoint));
      if (needsRelaunch) {
        await this.close();
      } else {
        return;
      }
    }

    if (cdpEndpoint) {
      await this.connectViaCDP(cdpEndpoint);
      return;
    }

    /* ───────────── PROVIDERS FIRST (FIXED) ───────────── */

    const provider = options.provider ?? process.env.AGENT_BROWSER_PROVIDER;

    if (provider === 'browserbase') {
      await this.connectToBrowserbase();
      return;
    }
    if (provider === 'browseruse') {
      await this.connectToBrowserUse();
      return;
    }
    if (provider === 'kernel') {
      await this.connectToKernel();
      return;
    }

    /* ───────────── LOCAL BROWSER SELECTION ───────────── */

    const isArm64 = os.arch() === 'arm64';
    let browserType = options.browser;

    if (!browserType) {
      if (hasExtensions) {
        browserType = 'chromium';
        if (isArm64) {
          console.warn(
            `[agent-browser] Extensions require Chromium, which has limited ARM64 support.`
          );
        }
      } else if (isArm64) {
        browserType = 'firefox';
        console.info(
          `[agent-browser] ARM64 detected. Using Firefox by default.`
        );
      } else {
        browserType = 'chromium';
      }
    } else if (browserType === 'chromium' && isArm64 && !hasExtensions) {
      console.warn(
        `[agent-browser] Chromium may not be available on ARM64. Firefox is recommended.`
      );
    }

    if (hasExtensions && browserType !== 'chromium') {
      throw new Error('Extensions are only supported in Chromium');
    }

    const launcher =
      browserType === 'firefox'
        ? firefox
        : browserType === 'webkit'
          ? webkit
          : chromium;

    const viewport = options.viewport ?? { width: 1280, height: 720 };

    let context: BrowserContext;

    if (hasExtensions) {
      const extPaths = options.extensions!.join(',');
      const session = process.env.AGENT_BROWSER_SESSION || 'default';
      const extArgs = [
        `--disable-extensions-except=${extPaths}`,
        `--load-extension=${extPaths}`,
      ];
      const allArgs = options.args ? [...extArgs, ...options.args] : extArgs;

      context = await launcher.launchPersistentContext(
        path.join(os.tmpdir(), `agent-browser-ext-${session}`),
        {
          headless: false,
          executablePath: options.executablePath,
          args: allArgs,
          viewport,
          extraHTTPHeaders: options.headers,
        }
      );
      this.isPersistentContext = true;
    } else if (hasProfile) {
      const profilePath = options.profile!.replace(/^~\//, os.homedir() + '/');
      context = await launcher.launchPersistentContext(profilePath, {
        headless: options.headless ?? true,
        executablePath: options.executablePath,
        viewport,
        extraHTTPHeaders: options.headers,
      });
      this.isPersistentContext = true;
    } else {
      this.browser = await launcher.launch({
        headless: options.headless ?? true,
        executablePath: options.executablePath,
        args: options.args,
      });

      context = await this.browser.newContext({
        viewport,
        extraHTTPHeaders: options.headers,
        ...(options.storageState && { storageState: options.storageState }),
      });
    }

    context.setDefaultTimeout(60000);
    this.contexts.push(context);
    this.setupContextTracking(context);

    const page = context.pages()[0] ?? (await context.newPage());
    if (!this.pages.includes(page)) {
      this.pages.push(page);
      this.setupPageTracking(page);
    }
    this.activePageIndex = this.pages.length - 1;
  }

  /* ────────────────────────────────────────────────────────────── */
  /* EVERYTHING BELOW THIS POINT IS UNCHANGED                       */
  /* (connectors, tracking, CDP, recording, close, etc.)            */
  /* ────────────────────────────────────────────────────────────── */

  // … the remainder of the file is identical to what you posted …
}
