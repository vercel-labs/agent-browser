/**
 * iOS Simulator Manager - Manages iOS Simulator and Safari automation via Appium.
 *
 * This provides 1:1 command parity with BrowserManager for iOS Safari.
 */

// Declare browser globals used in execute() callbacks - these run in browser context, not Node
declare const document: any;
declare const window: any;

import { Simctl } from 'node-simctl';
import { remote, type Browser as WDIOBrowser } from 'webdriverio';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';

// Ref map for element targeting (mirrors snapshot.ts)
export interface IOSRefMap {
  [ref: string]: {
    selector: string;
    role?: string;
    name?: string;
    xpath?: string;
  };
}

export interface IOSEnhancedSnapshot {
  tree: string;
  refs: IOSRefMap;
}

interface ConsoleMessage {
  type: string;
  text: string;
  timestamp: number;
}

interface IOSDeviceInfo {
  name: string;
  udid: string;
  state: string;
  runtime: string;
  isAvailable: boolean;
  isRealDevice?: boolean;
}

/**
 * Manages iOS Simulator and Safari automation via Appium
 */
export class IOSManager {
  private simctl: Simctl;
  private browser: WDIOBrowser | null = null;
  private appiumProcess: ChildProcess | null = null;
  private deviceUdid: string | null = null;
  private deviceName: string | null = null;
  private consoleMessages: ConsoleMessage[] = [];
  private refMap: IOSRefMap = {};
  private lastSnapshot: string = '';
  private refCounter: number = 0;

  // Default Appium port
  private static readonly APPIUM_PORT = 4723;
  private static readonly APPIUM_HOST = '127.0.0.1';

  constructor() {
    this.simctl = new Simctl();
  }

  /**
   * Check if browser is launched
   */
  isLaunched(): boolean {
    return this.browser !== null;
  }

  /**
   * List connected real iOS devices
   */
  private async listRealDevices(): Promise<IOSDeviceInfo[]> {
    const devices: IOSDeviceInfo[] = [];

    try {
      // Use xcrun xctrace to list connected devices
      const { execSync } = await import('node:child_process');
      const output = execSync('xcrun xctrace list devices 2>/dev/null || true', {
        encoding: 'utf-8',
        timeout: 10000,
      });

      // Parse output - format is:
      // == Devices ==
      // Device Name (OS Version) (UDID)
      // Real devices show version as just "26.2", simulators as "iOS 18.0"
      const lines = output.split('\n');
      let inDevicesSection = false;

      for (const line of lines) {
        if (line.includes('== Devices ==')) {
          inDevicesSection = true;
          continue;
        }
        // Stop at Simulators or Devices Offline section
        if (line.includes('== Simulators ==') || line.includes('== Devices Offline ==')) {
          break;
        }

        if (inDevicesSection && line.trim()) {
          // Match pattern: "Device Name (version) (UDID)"
          const match = line.match(/^(.+?)\s+\(([^)]+)\)\s+\(([A-F0-9-]+)\)$/i);
          if (match) {
            const [, name, version, udid] = match;
            const nameLower = name.toLowerCase();
            // Include iOS devices: either name contains iPhone/iPad, or version looks like iOS
            // (a simple version number like "26.2" or "18.6") and isn't a Mac
            const isIOS =
              nameLower.includes('iphone') ||
              nameLower.includes('ipad') ||
              version.includes('iOS') ||
              version.includes('iPadOS');
            const isMac =
              nameLower.includes('mac') ||
              nameLower.includes('macbook') ||
              nameLower.includes('imac');

            if (isIOS || (!isMac && /^\d+\.\d+(\.\d+)?$/.test(version))) {
              devices.push({
                name: name.trim(),
                udid: udid,
                state: 'Connected',
                runtime: `iOS ${version}`,
                isAvailable: true,
                isRealDevice: true,
              });
            }
          }
        }
      }
    } catch {
      // Ignore errors - real device listing is optional
    }

    return devices;
  }

  /**
   * List available iOS simulators
   */
  async listDevices(): Promise<IOSDeviceInfo[]> {
    const devices: IOSDeviceInfo[] = [];

    try {
      const rawDevices = await this.simctl.getDevices();

      for (const [runtime, deviceList] of Object.entries(rawDevices)) {
        if (!Array.isArray(deviceList)) continue;

        for (const device of deviceList) {
          // Only include iPhone and iPad simulators
          if (device.name && (device.name.includes('iPhone') || device.name.includes('iPad'))) {
            devices.push({
              name: device.name,
              udid: device.udid,
              state: device.state,
              runtime: runtime,
              isAvailable: device.isAvailable ?? true,
              isRealDevice: false,
            });
          }
        }
      }
    } catch (error) {
      throw new Error(
        `Failed to list iOS simulators. Is Xcode installed? Error: ${error instanceof Error ? error.message : String(error)}`
      );
    }

    return devices;
  }

  /**
   * List all devices (simulators + real devices)
   */
  async listAllDevices(): Promise<IOSDeviceInfo[]> {
    const [simulators, realDevices] = await Promise.all([
      this.listDevices(),
      this.listRealDevices(),
    ]);

    // Real devices first, then simulators
    return [...realDevices, ...simulators];
  }

  /**
   * Find the best default device (most recent iPhone)
   */
  private async findDefaultDevice(): Promise<IOSDeviceInfo | null> {
    const devices = await this.listDevices();

    // Filter to available iPhones, prefer Pro models, then by name (which typically indicates recency)
    const iphones = devices
      .filter((d) => d.isAvailable && d.name.includes('iPhone'))
      .sort((a, b) => {
        // Prefer Pro models
        const aIsPro = a.name.includes('Pro') ? 1 : 0;
        const bIsPro = b.name.includes('Pro') ? 1 : 0;
        if (aIsPro !== bIsPro) return bIsPro - aIsPro;

        // Then sort by name descending (iPhone 15 > iPhone 14)
        return b.name.localeCompare(a.name);
      });

    return iphones[0] ?? null;
  }

  /**
   * Find device by name or UDID (searches both simulators and real devices)
   */
  private async findDevice(nameOrUdid: string): Promise<IOSDeviceInfo | null> {
    const devices = await this.listAllDevices();

    // Try exact UDID match first
    const byUdid = devices.find((d) => d.udid === nameOrUdid);
    if (byUdid) return byUdid;

    // Try exact name match
    const byExactName = devices.find((d) => d.name === nameOrUdid);
    if (byExactName) return byExactName;

    // Try partial name match
    const byPartialName = devices.find((d) =>
      d.name.toLowerCase().includes(nameOrUdid.toLowerCase())
    );
    return byPartialName ?? null;
  }

  /**
   * Check if Appium is installed
   */
  private async checkAppiumInstalled(): Promise<boolean> {
    return new Promise((resolve) => {
      const proc = spawn('appium', ['--version'], { shell: true });
      proc.on('close', (code) => resolve(code === 0));
      proc.on('error', () => resolve(false));
    });
  }

  /**
   * Check if Appium server is already running
   */
  private async isAppiumRunning(): Promise<boolean> {
    try {
      const response = await fetch(
        `http://${IOSManager.APPIUM_HOST}:${IOSManager.APPIUM_PORT}/status`
      );
      return response.ok;
    } catch {
      return false;
    }
  }

  /**
   * Start Appium server if not already running
   */
  private async startAppiumServer(): Promise<void> {
    if (await this.isAppiumRunning()) {
      return; // Already running
    }

    if (!(await this.checkAppiumInstalled())) {
      throw new Error(
        'Appium not installed. Run: npm install -g appium && appium driver install xcuitest'
      );
    }

    return new Promise((resolve, reject) => {
      this.appiumProcess = spawn(
        'appium',
        ['--port', String(IOSManager.APPIUM_PORT), '--relaxed-security'],
        {
          shell: true,
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );

      let started = false;
      const timeout = setTimeout(() => {
        if (!started) {
          reject(new Error('Appium server failed to start within 30 seconds'));
        }
      }, 30000);

      this.appiumProcess.stdout?.on('data', (data: Buffer) => {
        const output = data.toString();
        if (output.includes('Appium REST http interface listener started')) {
          started = true;
          clearTimeout(timeout);
          resolve();
        }
      });

      this.appiumProcess.stderr?.on('data', (data: Buffer) => {
        const output = data.toString();
        // Appium logs to stderr for info messages too
        if (output.includes('Appium REST http interface listener started')) {
          started = true;
          clearTimeout(timeout);
          resolve();
        }
      });

      this.appiumProcess.on('error', (err) => {
        clearTimeout(timeout);
        reject(new Error(`Failed to start Appium: ${err.message}`));
      });

      this.appiumProcess.on('close', (code) => {
        if (!started) {
          clearTimeout(timeout);
          reject(new Error(`Appium exited with code ${code}`));
        }
      });
    });
  }

  /**
   * Boot the iOS simulator
   */
  private async bootSimulator(udid: string): Promise<void> {
    try {
      const devices = await this.simctl.getDevices();
      let currentState: string | undefined;

      // Find current device state
      for (const deviceList of Object.values(devices)) {
        if (!Array.isArray(deviceList)) continue;
        const device = (deviceList as any[]).find((d: any) => d.udid === udid);
        if (device) {
          currentState = device.state;
          break;
        }
      }

      if (currentState === 'Booted') {
        return; // Already booted
      }

      // node-simctl expects udid to be set on the instance
      this.simctl.udid = udid;
      await this.simctl.bootDevice();

      // Wait for device to be fully booted
      let attempts = 0;
      while (attempts < 60) {
        const updatedDevices = await this.simctl.getDevices();
        for (const deviceList of Object.values(updatedDevices)) {
          if (!Array.isArray(deviceList)) continue;
          const device = (deviceList as any[]).find((d: any) => d.udid === udid);
          if (device?.state === 'Booted') {
            return;
          }
        }
        await new Promise((r) => setTimeout(r, 1000));
        attempts++;
      }

      throw new Error('Simulator failed to boot within 60 seconds');
    } catch (error) {
      throw new Error(
        `Failed to boot simulator: ${error instanceof Error ? error.message : String(error)}`
      );
    }
  }

  /**
   * Launch iOS Safari via Appium
   */
  async launch(
    options: {
      device?: string;
      udid?: string;
      headless?: boolean;
    } = {}
  ): Promise<void> {
    if (this.isLaunched()) {
      return; // Already launched
    }

    // Find device
    let device: IOSDeviceInfo | null = null;

    if (options.udid) {
      device = await this.findDevice(options.udid);
      if (!device) {
        throw new Error(`Device with UDID ${options.udid} not found`);
      }
    } else if (options.device) {
      device = await this.findDevice(options.device);
      if (!device) {
        throw new Error(`Device "${options.device}" not found. Run: agent-browser device list`);
      }
    } else {
      // Check environment variable
      const envDevice = process.env.AGENT_BROWSER_IOS_DEVICE;
      const envUdid = process.env.AGENT_BROWSER_IOS_UDID;

      if (envUdid) {
        device = await this.findDevice(envUdid);
        if (!device) {
          throw new Error(`Device with UDID ${envUdid} not found. Run: agent-browser device list`);
        }
      } else if (envDevice) {
        device = await this.findDevice(envDevice);
        if (!device) {
          throw new Error(`Device "${envDevice}" not found. Run: agent-browser device list`);
        }
      } else {
        device = await this.findDefaultDevice();
        if (!device) {
          throw new Error(
            'No iOS simulators available. Open Xcode and download simulator runtimes.'
          );
        }
      }
    }

    this.deviceUdid = device.udid;
    this.deviceName = device.name;

    // Start Appium server
    await this.startAppiumServer();

    // Boot simulator (skip for real devices - they're already running)
    if (!device.isRealDevice) {
      await this.bootSimulator(device.udid);
    }

    // Connect to Safari via Appium
    try {
      this.browser = await remote({
        hostname: IOSManager.APPIUM_HOST,
        port: IOSManager.APPIUM_PORT,
        path: '/',
        capabilities: {
          platformName: 'iOS',
          'appium:automationName': 'XCUITest',
          'appium:deviceName': device.name,
          'appium:udid': device.udid,
          browserName: 'Safari',
          'appium:noReset': true,
          'appium:newCommandTimeout': 300,
        },
        connectionRetryTimeout: 120000,
        connectionRetryCount: 3,
      });
    } catch (error) {
      throw new Error(
        `Failed to connect to Safari: ${error instanceof Error ? error.message : String(error)}. ` +
          'Make sure XCUITest driver is installed: appium driver install xcuitest'
      );
    }
  }

  /**
   * Navigate to URL
   */
  async navigate(url: string): Promise<{ url: string; title: string }> {
    if (!this.browser) {
      throw new Error('iOS browser not launched. Call launch first.');
    }

    await this.browser.url(url);

    // Wait for page to load
    await this.browser.waitUntil(
      async () => {
        const state = (await this.browser!.execute(
          'return document.readyState'
        )) as unknown as string;
        return state === 'complete';
      },
      { timeout: 30000, interval: 500 }
    );

    const title = await this.browser.getTitle();
    const currentUrl = await this.browser.getUrl();

    return { url: currentUrl, title };
  }

  /**
   * Get current URL
   */
  async getUrl(): Promise<string> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }
    return this.browser.getUrl();
  }

  /**
   * Get page title
   */
  async getTitle(): Promise<string> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }
    return this.browser.getTitle();
  }

  /**
   * Click/tap an element
   */
  async click(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    await element.click();
  }

  /**
   * Alias for click (semantic clarity for touch)
   */
  async tap(selector: string): Promise<void> {
    return this.click(selector);
  }

  /**
   * Type text into an element
   */
  async type(
    selector: string,
    text: string,
    options?: { delay?: number; clear?: boolean }
  ): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);

    if (options?.clear) {
      await element.clearValue();
    }

    // WebdriverIO doesn't have a delay option, so we simulate it
    if (options?.delay && options.delay > 0) {
      for (const char of text) {
        await element.addValue(char);
        await new Promise((r) => setTimeout(r, options.delay));
      }
    } else {
      await element.addValue(text);
    }
  }

  /**
   * Fill an element (clear first, then type)
   */
  async fill(selector: string, value: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    await element.clearValue();
    await element.setValue(value);
  }

  /**
   * Get element by selector or ref
   */
  private async getElement(selectorOrRef: string) {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    // Check if it's a ref
    const refData = this.getRefData(selectorOrRef);
    if (refData) {
      if (refData.xpath) {
        return this.browser.$(refData.xpath);
      }
      return this.browser.$(refData.selector);
    }

    // Regular CSS selector
    return this.browser.$(selectorOrRef);
  }

  /**
   * Get ref data from ref string
   */
  private getRefData(refArg: string): IOSRefMap[string] | null {
    let ref: string | null = null;

    if (refArg.startsWith('@')) {
      ref = refArg.slice(1);
    } else if (refArg.startsWith('ref=')) {
      ref = refArg.slice(4);
    } else if (/^e\d+$/.test(refArg)) {
      ref = refArg;
    }

    if (ref && this.refMap[ref]) {
      return this.refMap[ref];
    }

    return null;
  }

  /**
   * Take a screenshot
   */
  async screenshot(options?: {
    path?: string;
    fullPage?: boolean;
  }): Promise<{ path?: string; base64?: string }> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const base64 = await this.browser.takeScreenshot();

    if (options?.path) {
      const { writeFileSync, mkdirSync } = await import('node:fs');
      const dir = path.dirname(options.path);
      if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true });
      }
      writeFileSync(options.path, base64, 'base64');
      return { path: options.path };
    }

    return { base64 };
  }

  /**
   * Get page snapshot with refs
   */
  async getSnapshot(options?: { interactive?: boolean }): Promise<IOSEnhancedSnapshot> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    this.refCounter = 0;
    this.refMap = {};

    // Get page structure via JavaScript execution
    // Note: The function runs in browser context, so we use 'any' for DOM types
    const snapshot = await this.browser.execute(function (interactiveOnly: boolean): any {
      const INTERACTIVE_ROLES = new Set([
        'button',
        'link',
        'textbox',
        'checkbox',
        'radio',
        'combobox',
        'listbox',
        'menuitem',
        'option',
        'searchbox',
        'slider',
        'spinbutton',
        'switch',
        'tab',
        'treeitem',
      ]);

      const INTERACTIVE_TAGS = new Set([
        'A',
        'BUTTON',
        'INPUT',
        'SELECT',
        'TEXTAREA',
        'DETAILS',
        'SUMMARY',
      ]);

      function getXPath(element: any): string {
        if (element.id) {
          return `//*[@id="${element.id}"]`;
        }

        const parts: string[] = [];
        let current: any = element;

        while (current && current.nodeType === 1) {
          // Node.ELEMENT_NODE = 1
          let index = 1;
          let sibling: any = current.previousElementSibling;

          while (sibling) {
            if (sibling.nodeName === current.nodeName) {
              index++;
            }
            sibling = sibling.previousElementSibling;
          }

          const tagName = current.nodeName.toLowerCase();
          parts.unshift(`${tagName}[${index}]`);
          current = current.parentElement;
        }

        return '/' + parts.join('/');
      }

      function getAccessibleName(element: any): string {
        // aria-label takes precedence
        const ariaLabel = element.getAttribute('aria-label');
        if (ariaLabel) return ariaLabel;

        // For inputs, check placeholder and label
        const tagName = element.tagName;
        if (tagName === 'INPUT' || tagName === 'TEXTAREA') {
          const id = element.id;
          if (id) {
            const label = (document as any).querySelector(`label[for="${id}"]`);
            if (label) return label.textContent?.trim() || '';
          }
          if (element.placeholder) return element.placeholder;
        }

        // For buttons and links, use text content
        if (tagName === 'BUTTON' || tagName === 'A') {
          return element.textContent?.trim() || '';
        }

        // aria-labelledby
        const labelledBy = element.getAttribute('aria-labelledby');
        if (labelledBy) {
          const labelElement = (document as any).getElementById(labelledBy);
          if (labelElement) return labelElement.textContent?.trim() || '';
        }

        return element.textContent?.trim().slice(0, 50) || '';
      }

      function getRole(element: any): string | null {
        // Explicit role
        const role = element.getAttribute('role');
        if (role) return role;

        // Implicit roles
        const tag = element.tagName;
        if (tag === 'A' && element.hasAttribute('href')) return 'link';
        if (tag === 'BUTTON') return 'button';
        if (tag === 'INPUT') {
          const type = element.type;
          if (type === 'checkbox') return 'checkbox';
          if (type === 'radio') return 'radio';
          if (type === 'text' || type === 'email' || type === 'password' || type === 'search')
            return 'textbox';
          if (type === 'submit' || type === 'button') return 'button';
        }
        if (tag === 'TEXTAREA') return 'textbox';
        if (tag === 'SELECT') return 'combobox';
        if (
          tag === 'H1' ||
          tag === 'H2' ||
          tag === 'H3' ||
          tag === 'H4' ||
          tag === 'H5' ||
          tag === 'H6'
        )
          return 'heading';
        if (tag === 'IMG') return 'img';
        if (tag === 'NAV') return 'navigation';
        if (tag === 'MAIN') return 'main';
        if (tag === 'HEADER') return 'banner';
        if (tag === 'FOOTER') return 'contentinfo';

        return null;
      }

      function traverse(element: any, depth: number): any {
        if (depth > 10) return null; // Limit depth

        const tag = element.tagName;
        const role = getRole(element);
        const name = getAccessibleName(element);
        const isInteractive =
          INTERACTIVE_TAGS.has(tag) || (role !== null && INTERACTIVE_ROLES.has(role));

        // Skip hidden elements
        const style = (window as any).getComputedStyle(element);
        if (style.display === 'none' || style.visibility === 'hidden') {
          return null;
        }

        const children: any[] = [];
        for (const child of element.children) {
          const childInfo = traverse(child, depth + 1);
          if (childInfo) {
            children.push(childInfo);
          }
        }

        // In interactive mode, skip non-interactive elements without interactive children
        if (interactiveOnly && !isInteractive && children.length === 0) {
          return null;
        }

        return {
          tag,
          role,
          name,
          text: element.textContent?.trim().slice(0, 100) || '',
          isInteractive,
          xpath: getXPath(element),
          children,
        };
      }

      const root = traverse((document as any).body, 0);
      return root;
    }, options?.interactive ?? false);

    // Build tree string and refs
    const lines: string[] = [];
    const buildTree = (node: any, indent: number) => {
      if (!node) return;

      const prefix = '  '.repeat(indent) + '- ';
      const role = node.role || node.tag.toLowerCase();
      const name = node.name;

      let line = `${prefix}${role}`;
      if (name) {
        line += ` "${name}"`;
      }

      // Add ref for interactive elements
      if (node.isInteractive) {
        const ref = `e${++this.refCounter}`;
        line += ` [ref=${ref}]`;

        this.refMap[ref] = {
          selector: node.xpath.startsWith('/') ? node.xpath : `#${node.xpath}`,
          role: node.role,
          name: node.name,
          xpath: node.xpath,
        };
      }

      lines.push(line);

      for (const child of node.children || []) {
        buildTree(child, indent + 1);
      }
    };

    if (snapshot) {
      buildTree(snapshot, 0);
    }

    const tree = lines.join('\n') || '(empty)';
    this.lastSnapshot = tree;

    return { tree, refs: this.refMap };
  }

  /**
   * Get cached ref map
   */
  getRefMap(): IOSRefMap {
    return this.refMap;
  }

  /**
   * Scroll the page
   */
  async scroll(options?: {
    selector?: string;
    x?: number;
    y?: number;
    direction?: 'up' | 'down' | 'left' | 'right';
    amount?: number;
  }): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const amount = options?.amount ?? 300;

    if (options?.selector) {
      const element = await this.getElement(options.selector);
      await element.scrollIntoView();
      return;
    }

    // Use JavaScript scrolling
    let deltaX = options?.x ?? 0;
    let deltaY = options?.y ?? 0;

    if (options?.direction) {
      switch (options.direction) {
        case 'up':
          deltaY = -amount;
          break;
        case 'down':
          deltaY = amount;
          break;
        case 'left':
          deltaX = -amount;
          break;
        case 'right':
          deltaX = amount;
          break;
      }
    }

    await this.browser.execute(
      function (x: number, y: number) {
        (window as any).scrollBy(x, y);
      },
      deltaX,
      deltaY
    );
  }

  /**
   * Swipe gesture (iOS-specific)
   */
  async swipe(
    direction: 'up' | 'down' | 'left' | 'right',
    options?: { distance?: number }
  ): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const distance = options?.distance ?? 300;

    // Map direction to scroll (opposite direction)
    const scrollDirection = {
      up: 'down',
      down: 'up',
      left: 'right',
      right: 'left',
    }[direction] as 'up' | 'down' | 'left' | 'right';

    await this.scroll({ direction: scrollDirection, amount: distance });
  }

  /**
   * Execute JavaScript
   */
  async evaluate<T = unknown>(script: string, ...args: unknown[]): Promise<T> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    // Execute the script directly - WebdriverIO handles the context
    const result = await this.browser.execute(
      function (code: string, evalArgs: any[]) {
        // Create a function from the code and execute it
        const fn = new Function(...evalArgs.map((_: any, i: number) => `arg${i}`), code);
        return fn(...evalArgs);
      },
      script.includes('return') ? script : `return (${script})`,
      args
    );

    return result as T;
  }

  /**
   * Wait for element
   */
  async wait(options: {
    selector?: string;
    timeout?: number;
    state?: 'attached' | 'detached' | 'visible' | 'hidden';
  }): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const timeout = options.timeout ?? 30000;

    if (options.selector) {
      const element = await this.getElement(options.selector);

      switch (options.state) {
        case 'detached':
          await element.waitForExist({ timeout, reverse: true });
          break;
        case 'hidden':
          await element.waitForDisplayed({ timeout, reverse: true });
          break;
        case 'visible':
          await element.waitForDisplayed({ timeout });
          break;
        case 'attached':
        default:
          await element.waitForExist({ timeout });
          break;
      }
    } else {
      // Just wait for timeout
      await new Promise((r) => setTimeout(r, timeout));
    }
  }

  /**
   * Press a key
   */
  async press(key: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    // Map common key names
    const keyMap: Record<string, string> = {
      Enter: '\uE007',
      Tab: '\uE004',
      Escape: '\uE00C',
      Backspace: '\uE003',
      Delete: '\uE017',
      ArrowUp: '\uE013',
      ArrowDown: '\uE015',
      ArrowLeft: '\uE012',
      ArrowRight: '\uE014',
    };

    const mappedKey = keyMap[key] ?? key;
    await this.browser.keys(mappedKey);
  }

  /**
   * Hover over element (limited on touch - just scrolls into view)
   */
  async hover(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    await element.scrollIntoView();
  }

  /**
   * Get page content (HTML)
   */
  async getContent(selector?: string): Promise<string> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    if (selector) {
      const element = await this.getElement(selector);
      return element.getHTML();
    }

    return this.browser.getPageSource();
  }

  /**
   * Get text content of element
   */
  async getText(selector: string): Promise<string> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    return element.getText();
  }

  /**
   * Get attribute value
   */
  async getAttribute(selector: string, attribute: string): Promise<string | null> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    return element.getAttribute(attribute);
  }

  /**
   * Check if element is visible
   */
  async isVisible(selector: string): Promise<boolean> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    try {
      const element = await this.getElement(selector);
      return element.isDisplayed();
    } catch {
      return false;
    }
  }

  /**
   * Check if element is enabled
   */
  async isEnabled(selector: string): Promise<boolean> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    return element.isEnabled();
  }

  /**
   * Navigate back
   */
  async goBack(): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }
    await this.browser.back();
  }

  /**
   * Navigate forward
   */
  async goForward(): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }
    await this.browser.forward();
  }

  /**
   * Reload page
   */
  async reload(): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }
    await this.browser.refresh();
  }

  /**
   * Select option(s) from dropdown
   */
  async select(selector: string, values: string | string[]): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    const valueArray = Array.isArray(values) ? values : [values];

    for (const value of valueArray) {
      await element.selectByAttribute('value', value);
    }
  }

  /**
   * Check a checkbox
   */
  async check(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    const isChecked = await element.isSelected();
    if (!isChecked) {
      await element.click();
    }
  }

  /**
   * Uncheck a checkbox
   */
  async uncheck(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    const isChecked = await element.isSelected();
    if (isChecked) {
      await element.click();
    }
  }

  /**
   * Focus an element
   */
  async focus(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    await this.browser.execute(function (el: any) {
      el.focus();
    }, element as any);
  }

  /**
   * Clear input field
   */
  async clear(selector: string): Promise<void> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const element = await this.getElement(selector);
    await element.clearValue();
  }

  /**
   * Get element count
   */
  async count(selector: string): Promise<number> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    const elements = await this.browser.$$(selector);
    return elements.length;
  }

  /**
   * Get bounding box
   */
  async getBoundingBox(
    selector: string
  ): Promise<{ x: number; y: number; width: number; height: number } | null> {
    if (!this.browser) {
      throw new Error('iOS browser not launched');
    }

    try {
      const element = await this.getElement(selector);
      const location = await element.getLocation();
      const size = await element.getSize();
      return {
        x: location.x,
        y: location.y,
        width: size.width,
        height: size.height,
      };
    } catch {
      return null;
    }
  }

  /**
   * Get device info
   */
  getDeviceInfo(): { name: string; udid: string } | null {
    if (!this.deviceName || !this.deviceUdid) {
      return null;
    }
    return {
      name: this.deviceName,
      udid: this.deviceUdid,
    };
  }

  /**
   * Close browser and cleanup
   */
  async close(): Promise<void> {
    if (this.browser) {
      try {
        await this.browser.deleteSession();
      } catch {
        // Ignore cleanup errors
      }
      this.browser = null;
    }

    if (this.appiumProcess) {
      this.appiumProcess.kill();
      this.appiumProcess = null;
    }

    // Optionally shutdown simulator
    if (this.deviceUdid) {
      try {
        this.simctl.udid = this.deviceUdid;
        await this.simctl.shutdownDevice();
      } catch {
        // Ignore - simulator might already be shutdown
      }
    }

    this.deviceUdid = null;
    this.deviceName = null;
    this.refMap = {};
    this.lastSnapshot = '';
    this.refCounter = 0;
  }
}
