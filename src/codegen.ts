/**
 * Playwright Code Generator
 *
 * Records browser actions and generates equivalent Playwright TypeScript code.
 * Supports both JSON (with original commands) and code-only output formats.
 *
 * Usage:
 *   agent-browser codegen start [output.json]
 *   agent-browser open example.com
 *   agent-browser click @e1
 *   agent-browser codegen stop
 */

import * as fs from 'fs';
import * as path from 'path';
import type { Command, Response } from './types.js';
import type { RefMap } from './snapshot.js';

/**
 * Recorded action with original command and generated code
 */
export interface RecordedAction {
  command: string;
  code: string;
  timestamp: string;
}

/**
 * Codegen recording metadata
 */
export interface CodegenMetadata {
  startedAt: string;
  endedAt?: string;
  url?: string;
}

/**
 * Full codegen output structure (JSON format)
 */
export interface CodegenOutput {
  metadata: CodegenMetadata;
  actions: RecordedAction[];
}

/**
 * Options for starting codegen recording
 */
export interface CodegenOptions {
  /** Output file path (defaults to timestamp-based name) */
  path?: string;
  /** If true, output only the code without JSON wrapper */
  codeOnly?: boolean;
  /** Working directory for relative paths */
  cwd?: string;
}

/**
 * Result of stopping codegen
 */
export interface CodegenResult {
  path: string;
  actionCount: number;
  codeOnly: boolean;
  /** Generated Playwright code (for AI-readable output) */
  code?: string;
  /** Full JSON output with metadata and actions (when not code-only) */
  output?: CodegenOutput;
  /** True if the code was NOT saved to a file (noSave option) */
  noSave?: boolean;
}

/**
 * CodegenRecorder manages the recording state and code generation
 */
export class CodegenRecorder {
  private actions: RecordedAction[] = [];
  private metadata: CodegenMetadata;
  private options: CodegenOptions;
  private currentUrl: string = '';

  constructor(options: CodegenOptions = {}) {
    this.options = options;
    this.metadata = {
      startedAt: new Date().toISOString(),
    };
  }

  /**
   * Get the output file path (resolves default if not specified)
   */
  getOutputPath(): string {
    if (this.options.path) {
      // If relative path and cwd is specified, resolve it
      if (this.options.cwd && !path.isAbsolute(this.options.path)) {
        return path.join(this.options.cwd, this.options.path);
      }
      return this.options.path;
    }

    // Generate timestamp-based filename
    const timestamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
    const ext = this.options.codeOnly ? '.ts' : '.json';
    const filename = `codegen-${timestamp}${ext}`;

    if (this.options.cwd) {
      return path.join(this.options.cwd, filename);
    }
    return filename;
  }

  /**
   * Check if code-only mode is enabled
   */
  isCodeOnly(): boolean {
    return this.options.codeOnly ?? false;
  }

  /**
   * Update current URL (for metadata)
   */
  setCurrentUrl(url: string): void {
    this.currentUrl = url;
    if (!this.metadata.url) {
      this.metadata.url = url;
    }
  }

  /**
   * Record an action if the response was successful
   */
  recordAction(
    command: Command,
    response: Response,
    refMap: RefMap,
    originalCommand?: string
  ): void {
    // Only record successful actions
    if (!response.success) {
      return;
    }

    // Generate the Playwright code for this action
    const code = this.generateCode(command, refMap);
    if (!code) {
      return; // Action doesn't generate code (e.g., snapshot, status checks)
    }

    // Build the original CLI command string
    const cliCommand = originalCommand ?? this.buildCliCommand(command);

    this.actions.push({
      command: cliCommand,
      code,
      timestamp: new Date().toISOString(),
    });

    // Update URL if this was a navigate action
    if (command.action === 'navigate' && 'url' in command) {
      this.setCurrentUrl(command.url);
    }
  }

  /**
   * Build CLI command string from command object
   */
  private buildCliCommand(command: Command): string {
    const parts: string[] = ['agent-browser'];

    switch (command.action) {
      case 'navigate':
        parts.push('open', command.url);
        break;
      case 'click':
        parts.push('click', command.selector);
        break;
      case 'fill':
        parts.push('fill', command.selector, JSON.stringify(command.value));
        break;
      case 'type':
        parts.push('type', command.selector, JSON.stringify(command.text));
        break;
      case 'press':
        parts.push('press', command.key);
        break;
      case 'check':
        parts.push('check', command.selector);
        break;
      case 'uncheck':
        parts.push('uncheck', command.selector);
        break;
      case 'select':
        parts.push('select', command.selector);
        if (Array.isArray(command.values)) {
          parts.push(...command.values);
        } else {
          parts.push(command.values);
        }
        break;
      case 'hover':
        parts.push('hover', command.selector);
        break;
      case 'dblclick':
        parts.push('dblclick', command.selector);
        break;
      case 'focus':
        parts.push('focus', command.selector);
        break;
      case 'scroll':
        parts.push('scroll');
        if (command.direction) parts.push(command.direction);
        if (command.amount) parts.push(String(command.amount));
        break;
      case 'scrollintoview':
        parts.push('scrollintoview', command.selector);
        break;
      case 'drag':
        parts.push('drag', command.source, command.target);
        break;
      case 'upload':
        parts.push('upload', command.selector);
        if (Array.isArray(command.files)) {
          parts.push(...command.files);
        } else {
          parts.push(command.files);
        }
        break;
      case 'keydown':
        parts.push('keydown', command.key);
        break;
      case 'keyup':
        parts.push('keyup', command.key);
        break;
      case 'wait':
        parts.push('wait');
        if (command.selector) parts.push(command.selector);
        else if (command.timeout) parts.push(String(command.timeout));
        break;
      case 'back':
        parts.push('back');
        break;
      case 'forward':
        parts.push('forward');
        break;
      case 'reload':
        parts.push('reload');
        break;
      default:
        parts.push(command.action);
    }

    return parts.join(' ');
  }

  /**
   * Generate Playwright code for a command
   */
  private generateCode(command: Command, refMap: RefMap): string | null {
    switch (command.action) {
      case 'navigate':
        return `await page.goto('${this.escapeString(command.url)}');`;

      case 'click':
        return `await ${this.buildLocator(command.selector, refMap)}.click();`;

      case 'dblclick':
        return `await ${this.buildLocator(command.selector, refMap)}.dblclick();`;

      case 'fill':
        return `await ${this.buildLocator(command.selector, refMap)}.fill('${this.escapeString(command.value)}');`;

      case 'type':
        return `await ${this.buildLocator(command.selector, refMap)}.pressSequentially('${this.escapeString(command.text)}');`;

      case 'press':
        if (command.selector) {
          return `await page.press('${this.escapeString(command.selector)}', '${command.key}');`;
        }
        return `await page.keyboard.press('${command.key}');`;

      case 'check':
        return `await ${this.buildLocator(command.selector, refMap)}.check();`;

      case 'uncheck':
        return `await ${this.buildLocator(command.selector, refMap)}.uncheck();`;

      case 'select': {
        const values = Array.isArray(command.values) ? command.values : [command.values];
        const valuesStr = values.map((v) => `'${this.escapeString(v)}'`).join(', ');
        return `await ${this.buildLocator(command.selector, refMap)}.selectOption([${valuesStr}]);`;
      }

      case 'hover':
        return `await ${this.buildLocator(command.selector, refMap)}.hover();`;

      case 'focus':
        return `await ${this.buildLocator(command.selector, refMap)}.focus();`;

      case 'scroll':
        if (command.selector) {
          return `await ${this.buildLocator(command.selector, refMap)}.scrollIntoViewIfNeeded();`;
        }
        const deltaX =
          command.x ??
          (command.direction === 'left'
            ? -(command.amount ?? 100)
            : command.direction === 'right'
              ? (command.amount ?? 100)
              : 0);
        const deltaY =
          command.y ??
          (command.direction === 'up'
            ? -(command.amount ?? 100)
            : command.direction === 'down'
              ? (command.amount ?? 100)
              : 0);
        return `await page.evaluate(() => window.scrollBy(${deltaX}, ${deltaY}));`;

      case 'scrollintoview':
        return `await ${this.buildLocator(command.selector, refMap)}.scrollIntoViewIfNeeded();`;

      case 'drag':
        return `await page.dragAndDrop('${this.escapeString(command.source)}', '${this.escapeString(command.target)}');`;

      case 'upload': {
        const files = Array.isArray(command.files) ? command.files : [command.files];
        const filesStr = files.map((f) => `'${this.escapeString(f)}'`).join(', ');
        return `await ${this.buildLocator(command.selector, refMap)}.setInputFiles([${filesStr}]);`;
      }

      case 'keydown':
        return `await page.keyboard.down('${command.key}');`;

      case 'keyup':
        return `await page.keyboard.up('${command.key}');`;

      case 'wait':
        if (command.selector) {
          return `await page.waitForSelector('${this.escapeString(command.selector)}', { state: '${command.state ?? 'visible'}' });`;
        }
        if (command.timeout) {
          return `await page.waitForTimeout(${command.timeout});`;
        }
        return `await page.waitForLoadState('load');`;

      case 'waitforurl':
        return `await page.waitForURL('${this.escapeString(command.url)}');`;

      case 'waitforloadstate':
        return `await page.waitForLoadState('${command.state}');`;

      case 'waitforfunction':
        return `await page.waitForFunction(() => ${this.escapeTemplateLiteral(command.expression)});`;

      case 'back':
        return `await page.goBack();`;

      case 'forward':
        return `await page.goForward();`;

      case 'reload':
        return `await page.reload();`;

      case 'evaluate':
        return `await page.evaluate(() => { ${this.escapeTemplateLiteral(command.script)} });`;

      case 'screenshot':
        if (command.path) {
          return `await page.screenshot({ path: '${this.escapeString(command.path)}'${command.fullPage ? ', fullPage: true' : ''} });`;
        }
        return `await page.screenshot(${command.fullPage ? '{ fullPage: true }' : ''});`;

      case 'pdf':
        return `await page.pdf({ path: '${this.escapeString(command.path)}' });`;

      case 'mousemove':
        return `await page.mouse.move(${command.x}, ${command.y});`;

      case 'mousedown':
        return `await page.mouse.down({ button: '${command.button ?? 'left'}' });`;

      case 'mouseup':
        return `await page.mouse.up({ button: '${command.button ?? 'left'}' });`;

      case 'wheel':
        return `await page.mouse.wheel(${command.deltaX ?? 0}, ${command.deltaY ?? 0});`;

      case 'getbyrole':
        return this.generateGetByRoleCode(command);

      case 'getbytext':
        return `await page.getByText('${this.escapeString(command.text)}'${command.exact ? ', { exact: true }' : ''}).${command.subaction}();`;

      case 'getbylabel': {
        const locator = `page.getByLabel('${this.escapeString(command.label)}')`;
        if (command.subaction === 'fill' && command.value) {
          return `await ${locator}.fill('${this.escapeString(command.value)}');`;
        }
        return `await ${locator}.${command.subaction}();`;
      }

      case 'getbyplaceholder': {
        const locator = `page.getByPlaceholder('${this.escapeString(command.placeholder)}')`;
        if (command.subaction === 'fill' && command.value) {
          return `await ${locator}.fill('${this.escapeString(command.value)}');`;
        }
        return `await ${locator}.${command.subaction}();`;
      }

      case 'getbytestid': {
        const locator = `page.getByTestId('${this.escapeString(command.testId)}')`;
        if (command.subaction === 'fill' && command.value) {
          return `await ${locator}.fill('${this.escapeString(command.value)}');`;
        }
        return `await ${locator}.${command.subaction}();`;
      }

      case 'getbyalttext':
        return `await page.getByAltText('${this.escapeString(command.text)}'${command.exact ? ', { exact: true }' : ''}).${command.subaction}();`;

      case 'getbytitle':
        return `await page.getByTitle('${this.escapeString(command.text)}'${command.exact ? ', { exact: true }' : ''}).${command.subaction}();`;

      case 'nth': {
        const locator =
          command.index === -1
            ? `page.locator('${this.escapeString(command.selector)}').last()`
            : `page.locator('${this.escapeString(command.selector)}').nth(${command.index})`;
        if (command.subaction === 'fill' && command.value) {
          return `await ${locator}.fill('${this.escapeString(command.value)}');`;
        }
        if (command.subaction === 'text') {
          return `await ${locator}.textContent();`;
        }
        return `await ${locator}.${command.subaction}();`;
      }

      case 'clear':
        return `await ${this.buildLocator(command.selector, refMap)}.clear();`;

      case 'tap':
        return `await page.tap('${this.escapeString(command.selector)}');`;

      case 'frame':
        if (command.selector) {
          return `const frame = await page.$('${this.escapeString(command.selector)}').then(el => el?.contentFrame());`;
        }
        if (command.name) {
          return `const frame = page.frame({ name: '${command.name}' });`;
        }
        return null;

      case 'mainframe':
        return `// Switch back to main frame`;

      // Tab/window operations
      case 'tab_new':
        if ('url' in command && command.url) {
          return `const newPage = await context.newPage();\nawait newPage.goto('${this.escapeString(command.url)}');`;
        }
        return `const newPage = await context.newPage();`;

      case 'tab_switch':
        return `// Tab switch - in Playwright, use page variables directly (e.g., page1, page2)`;

      case 'tab_close':
        return `await page.close();`;

      case 'window_new':
        return `const newContext = await browser.newContext();\nconst newPage = await newContext.newPage();`;

      // Dialog handling
      case 'dialog':
        if ('response' in command) {
          if (command.response === 'accept') {
            return `page.on('dialog', dialog => dialog.accept(${command.promptText ? `'${this.escapeString(command.promptText)}'` : ''}));`;
          }
          return `page.on('dialog', dialog => dialog.dismiss());`;
        }
        return null;

      // Network interception
      case 'route':
        if ('url' in command) {
          if ('abort' in command && command.abort) {
            return `await page.route('${command.url}', route => route.abort());`;
          }
          return `await page.route('${command.url}', route => route.continue());`;
        }
        return null;

      case 'unroute':
        if ('url' in command && command.url) {
          return `await page.unroute('${command.url}');`;
        }
        return `await page.unroute('**/*');`;

      // Clipboard operations
      case 'clipboard':
        if ('operation' in command) {
          if (command.operation === 'copy') {
            return `await page.keyboard.press('Control+c');`;
          }
          if (command.operation === 'paste') {
            return `await page.keyboard.press('Control+v');`;
          }
        }
        return null;

      // Keyboard shortcuts
      case 'keyboard':
        if ('keys' in command) {
          return `await page.keyboard.press('${command.keys}');`;
        }
        return null;

      // Insert text
      case 'inserttext':
        if ('text' in command) {
          return `await page.keyboard.insertText('${this.escapeString(command.text)}');`;
        }
        return null;

      // Multi-select
      case 'multiselect':
        if ('selector' in command && 'values' in command) {
          const values = command.values.map((v) => `'${this.escapeString(v)}'`).join(', ');
          return `await page.locator('${this.escapeString(command.selector)}').selectOption([${values}]);`;
        }
        return null;

      // Dispatch event
      case 'dispatch':
        if ('selector' in command && 'event' in command) {
          return `await page.locator('${this.escapeString(command.selector)}').dispatchEvent('${this.escapeString(command.event)}');`;
        }
        return null;

      // Set value directly
      case 'setvalue':
        if ('selector' in command && 'value' in command) {
          return `await page.locator('${this.escapeString(command.selector)}').fill('${this.escapeString(command.value)}');`;
        }
        return null;

      // Bring to front
      case 'bringtofront':
        return `await page.bringToFront();`;

      // Add scripts/styles
      case 'addscript':
        if ('content' in command && command.content) {
          return `await page.addScriptTag({ content: \`${this.escapeTemplateLiteral(command.content)}\` });`;
        }
        if ('url' in command && command.url) {
          return `await page.addScriptTag({ url: '${this.escapeString(command.url)}' });`;
        }
        return null;

      case 'addstyle':
        if ('content' in command && command.content) {
          return `await page.addStyleTag({ content: \`${this.escapeTemplateLiteral(command.content)}\` });`;
        }
        if ('url' in command && command.url) {
          return `await page.addStyleTag({ url: '${this.escapeString(command.url)}' });`;
        }
        return null;

      case 'addinitscript':
        if ('script' in command) {
          return `await context.addInitScript(() => { ${this.escapeTemplateLiteral(command.script)} });`;
        }
        return null;

      // Set content
      case 'setcontent':
        if ('html' in command) {
          return `await page.setContent(\`${this.escapeTemplateLiteral(command.html)}\`);`;
        }
        return null;

      // Actions that don't generate replayable code (read-only operations)
      case 'snapshot':
      case 'isvisible':
      case 'isenabled':
      case 'ischecked':
      case 'gettext':
      case 'getattribute':
      case 'innertext':
      case 'innerhtml':
      case 'inputvalue':
      case 'count':
      case 'boundingbox':
      case 'styles':
      case 'url':
      case 'title':
      case 'console':
      case 'errors':
      case 'cookies_get':
      case 'storage_get':
      case 'tab_list':
        return null;

      // Codegen meta commands (not recorded)
      case 'codegen_start':
      case 'codegen_stop':
      case 'codegen_status':
        return null;

      // Lifecycle commands (not recorded as they're implicit in test structure)
      case 'close':
      case 'launch':
        return null;

      // Recording/streaming operations (tooling, not replayable)
      case 'recording_start':
      case 'recording_stop':
      case 'recording_restart':
      case 'screencast_start':
      case 'screencast_stop':
      case 'trace_start':
      case 'trace_stop':
      case 'har_start':
      case 'har_stop':
      case 'video_start':
      case 'video_stop':
        return `// ${command.action}: Recording/tracing operation (not replayable in tests)`;

      // State operations (require external files, not directly replayable)
      case 'state_save':
      case 'state_load':
        return `// ${command.action}: State operation (requires external state file)`;

      // Low-level input injection (CDP-specific, use higher-level APIs in Playwright)
      case 'input_mouse':
      case 'input_keyboard':
      case 'input_touch':
        return `// ${command.action}: Low-level CDP input injection (use page.mouse/keyboard/tap APIs instead)`;

      // Environment emulation (typically done in test setup, not actions)
      case 'viewport':
      case 'device':
      case 'geolocation':
      case 'permissions':
      case 'offline':
      case 'headers':
      case 'credentials':
      case 'emulatemedia':
      case 'timezone':
      case 'locale':
        return `// ${command.action}: Environment emulation (typically set in test setup, not recorded as action)`;

      // Debugging operations
      case 'highlight':
      case 'pause':
        return `// ${command.action}: Debugging operation (not needed in replay)`;

      // Network requests (read-only)
      case 'requests':
      case 'responsebody':
      case 'waitfordownload':
      case 'download':
        return `// ${command.action}: Network/download operation (context-specific, may need manual adaptation)`;

      default:
        // For truly unhandled actions, return a comment
        return `// Unhandled action: ${command.action}`;
    }
  }

  /**
   * Generate code for getbyrole command
   */
  private generateGetByRoleCode(command: Command & { action: 'getbyrole' }): string {
    const options: string[] = [];
    if (command.name) {
      options.push(`name: '${this.escapeString(command.name)}'`);
    }
    const optionsStr = options.length > 0 ? `, { ${options.join(', ')} }` : '';
    const locator = `page.getByRole('${command.role}'${optionsStr})`;

    if (command.subaction === 'fill' && command.value) {
      return `await ${locator}.fill('${this.escapeString(command.value)}');`;
    }
    return `await ${locator}.${command.subaction}();`;
  }

  /**
   * Build a Playwright locator string from a selector or ref
   */
  private buildLocator(selectorOrRef: string, refMap: RefMap): string {
    // Check if it's a ref (e.g., @e1, e1)
    const refMatch = selectorOrRef.match(/^@?(e\d+)$/);
    if (refMatch) {
      const ref = refMatch[1];
      const refData = refMap[ref];
      if (refData) {
        // Use semantic locator from ref
        if (refData.name) {
          const escapedName = this.escapeString(refData.name);
          let locator = `page.getByRole('${refData.role}', { name: '${escapedName}', exact: true })`;
          if (refData.nth !== undefined) {
            locator += `.nth(${refData.nth})`;
          }
          return locator;
        } else {
          let locator = `page.getByRole('${refData.role}')`;
          if (refData.nth !== undefined) {
            locator += `.nth(${refData.nth})`;
          }
          return locator;
        }
      }
    }

    // Handle special selector prefixes
    if (selectorOrRef.startsWith('text=')) {
      return `page.getByText('${this.escapeString(selectorOrRef.slice(5))}')`;
    }
    if (selectorOrRef.startsWith('xpath=')) {
      return `page.locator('${this.escapeString(selectorOrRef)}')`;
    }

    // Default to CSS selector
    return `page.locator('${this.escapeString(selectorOrRef)}')`;
  }

  /**
   * Escape string for JavaScript/TypeScript (single-quoted strings)
   */
  private escapeString(str: string): string {
    return str
      .replace(/\\/g, '\\\\')
      .replace(/'/g, "\\'")
      .replace(/\n/g, '\\n')
      .replace(/\r/g, '\\r')
      .replace(/\t/g, '\\t');
  }

  /**
   * Escape content for template literals (backtick strings)
   * Escapes backticks, backslashes, and ${} interpolation
   */
  private escapeTemplateLiteral(str: string): string {
    return str.replace(/\\/g, '\\\\').replace(/`/g, '\\`').replace(/\$\{/g, '\\${');
  }

  /**
   * Get the number of recorded actions
   */
  getActionCount(): number {
    return this.actions.length;
  }

  /**
   * Generate the code string (without saving)
   */
  generateCodeString(): string {
    const lines: string[] = [
      '// Generated by agent-browser codegen',
      `// Started: ${this.metadata.startedAt}`,
      `// Ended: ${this.metadata.endedAt ?? new Date().toISOString()}`,
      '',
    ];

    for (const action of this.actions) {
      lines.push(action.code);
    }

    return lines.join('\n');
  }

  /**
   * Generate the full output object (without saving)
   */
  generateOutput(): CodegenOutput {
    return {
      metadata: {
        ...this.metadata,
        endedAt: this.metadata.endedAt ?? new Date().toISOString(),
      },
      actions: this.actions,
    };
  }

  /**
   * Generate the result without saving to file
   * Use this when --no-save is specified
   */
  generate(): CodegenResult {
    this.metadata.endedAt = new Date().toISOString();

    // Generate the code string (always available for AI-readable output)
    const code = this.generateCodeString();

    if (this.options.codeOnly) {
      return {
        path: '',
        actionCount: this.actions.length,
        codeOnly: true,
        code,
        noSave: true,
      };
    } else {
      // Generate JSON output (but don't save)
      const output = this.generateOutput();

      return {
        path: '',
        actionCount: this.actions.length,
        codeOnly: false,
        code,
        output,
        noSave: true,
      };
    }
  }

  /**
   * Stop recording and save to file
   */
  save(): CodegenResult {
    this.metadata.endedAt = new Date().toISOString();
    const outputPath = this.getOutputPath();

    // Ensure directory exists
    const dir = path.dirname(outputPath);
    if (dir && dir !== '.' && !fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }

    // Generate the code string (always available for AI-readable output)
    const code = this.generateCodeString();

    if (this.options.codeOnly) {
      // Save code-only output
      fs.writeFileSync(outputPath, code);

      return {
        path: outputPath,
        actionCount: this.actions.length,
        codeOnly: true,
        code,
      };
    } else {
      // Generate and save JSON output
      const output = this.generateOutput();
      fs.writeFileSync(outputPath, JSON.stringify(output, null, 2));

      return {
        path: outputPath,
        actionCount: this.actions.length,
        codeOnly: false,
        code,
        output,
      };
    }
  }

  /**
   * Get current recording status
   */
  getStatus(): { recording: boolean; actionCount: number; startedAt: string; path: string } {
    return {
      recording: true,
      actionCount: this.actions.length,
      startedAt: this.metadata.startedAt,
      path: this.getOutputPath(),
    };
  }
}
