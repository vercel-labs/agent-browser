import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { CodegenRecorder, type RecordedAction, type CodegenOutput } from './codegen.js';
import type { Command, Response } from './types.js';
import type { RefMap } from './snapshot.js';

describe('CodegenRecorder', () => {
  let tempDir: string;

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegen-test-'));
  });

  afterEach(() => {
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  describe('constructor and options', () => {
    it('should create recorder with default options', () => {
      const recorder = new CodegenRecorder();
      expect(recorder.isCodeOnly()).toBe(false);
      expect(recorder.getActionCount()).toBe(0);
    });

    it('should create recorder with code-only option', () => {
      const recorder = new CodegenRecorder({ codeOnly: true });
      expect(recorder.isCodeOnly()).toBe(true);
    });

    it('should create recorder with custom path', () => {
      const recorder = new CodegenRecorder({ path: 'custom.json', cwd: tempDir });
      expect(recorder.getOutputPath()).toBe(path.join(tempDir, 'custom.json'));
    });
  });

  describe('code generation', () => {
    it('should generate code for navigate action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);

      expect(recorder.getActionCount()).toBe(1);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe("await page.goto('https://example.com');");
    });

    it('should generate code for click action with CSS selector', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'click',
        selector: '#submit',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe("await page.locator('#submit').click();");
    });

    it('should generate code for click action with ref', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'click',
        selector: '@e1',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {
        e1: {
          selector: 'getByRole(\'button\', { name: "Submit", exact: true })',
          role: 'button',
          name: 'Submit',
        },
      };

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe(
        "await page.getByRole('button', { name: 'Submit', exact: true }).click();"
      );
    });

    it('should generate code for fill action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'fill',
        selector: '#email',
        value: 'test@example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe(
        "await page.locator('#email').fill('test@example.com');"
      );
    });

    it('should generate code for type action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'type',
        selector: '#search',
        text: 'hello world',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe(
        "await page.locator('#search').pressSequentially('hello world');"
      );
    });

    it('should generate code for press action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'press',
        key: 'Enter',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe("await page.keyboard.press('Enter');");
    });

    it('should generate code for check action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'check',
        selector: '#agree',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe("await page.locator('#agree').check();");
    });

    it('should generate code for select action', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'select',
        selector: '#country',
        values: ['US', 'UK'],
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe(
        "await page.locator('#country').selectOption(['US', 'UK']);"
      );
    });
  });

  describe('recording behavior', () => {
    it('should not record failed actions', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'click',
        selector: '#nonexistent',
      };
      const response: Response = { id: '1', success: false, error: 'Element not found' };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);

      expect(recorder.getActionCount()).toBe(0);
    });

    it('should not record snapshot actions', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'snapshot',
      };
      const response: Response = { id: '1', success: true, data: { snapshot: 'tree' } };
      const refMap: RefMap = {};

      recorder.recordAction(command, response, refMap);

      expect(recorder.getActionCount()).toBe(0);
    });
  });

  describe('output formats', () => {
    it('should save JSON format by default', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      expect(result.codeOnly).toBe(false);
      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.metadata).toBeDefined();
      expect(content.metadata.startedAt).toBeDefined();
      expect(content.metadata.endedAt).toBeDefined();
      expect(content.actions).toHaveLength(1);
      expect(content.actions[0].command).toContain('agent-browser');
      expect(content.actions[0].code).toContain('await page');
    });

    it('should save code-only format when specified', () => {
      const recorder = new CodegenRecorder({ path: 'test.ts', cwd: tempDir, codeOnly: true });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      expect(result.codeOnly).toBe(true);
      const content = fs.readFileSync(result.path, 'utf-8');
      expect(content).toContain('// Generated by agent-browser codegen');
      expect(content).toContain("await page.goto('https://example.com');");
    });

    it('should include code in result for AI-readable output', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      // Code should be included in result for AI to read from CLI output
      expect(result.code).toBeDefined();
      expect(result.code).toContain('// Generated by agent-browser codegen');
      expect(result.code).toContain("await page.goto('https://example.com');");
    });

    it('should include full output in result for JSON format', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'click',
        selector: '#button',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      // Output should be included for JSON format
      expect(result.output).toBeDefined();
      expect(result.output?.metadata.startedAt).toBeDefined();
      expect(result.output?.actions).toHaveLength(1);
    });
  });

  describe('status and metadata', () => {
    it('should return correct status', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });

      const status = recorder.getStatus();

      expect(status.recording).toBe(true);
      expect(status.actionCount).toBe(0);
      expect(status.startedAt).toBeDefined();
      expect(status.path).toContain('test.json');
    });

    it('should generate timestamp-based filename when no path specified', () => {
      const recorder = new CodegenRecorder({ cwd: tempDir });
      const outputPath = recorder.getOutputPath();

      expect(outputPath).toMatch(/codegen-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}\.json$/);
    });

    it('should generate .ts extension for code-only format', () => {
      const recorder = new CodegenRecorder({ cwd: tempDir, codeOnly: true });
      const outputPath = recorder.getOutputPath();

      expect(outputPath).toMatch(/\.ts$/);
    });
  });

  describe('string escaping', () => {
    it('should escape single quotes in values', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'fill',
        selector: '#name',
        value: "O'Brien",
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe("await page.locator('#name').fill('O\\'Brien');");
    });

    it('should escape backslashes in URLs', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com/path\\with\\backslashes',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toContain('\\\\');
    });
  });

  describe('ref resolution', () => {
    it('should use nth when ref has disambiguation', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'click',
        selector: '@e2',
      };
      const response: Response = { id: '1', success: true, data: {} };
      const refMap: RefMap = {
        e2: { selector: "getByRole('button')", role: 'button', name: 'Save', nth: 1 },
      };

      recorder.recordAction(command, response, refMap);
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toBe(
        "await page.getByRole('button', { name: 'Save', exact: true }).nth(1).click();"
      );
    });
  });

  describe('no-save mode', () => {
    it('should return code without saving to file', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'navigate',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.generate();

      expect(result.noSave).toBe(true);
      expect(result.path).toBe('');
      expect(result.code).toContain("await page.goto('https://example.com');");
      // File should NOT exist
      expect(fs.existsSync(path.join(tempDir, 'test.json'))).toBe(false);
    });
  });

  describe('non-convertible actions', () => {
    it('should record comment for recording operations', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'recording_start',
        path: 'video.webm',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toContain('Recording/tracing operation');
    });

    it('should record comment for environment emulation', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'viewport',
        width: 1920,
        height: 1080,
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toContain('Environment emulation');
    });

    it('should not record read-only operations like snapshot', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'snapshot',
      };
      const response: Response = { id: '1', success: true, data: { snapshot: 'tree' } };

      recorder.recordAction(command, response, {});

      // Snapshot should NOT be recorded
      expect(recorder.getActionCount()).toBe(0);
    });
  });

  describe('additional code generation', () => {
    it('should generate code for tab_new with URL', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'tab_new',
        url: 'https://example.com',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toContain('context.newPage()');
      expect(content.actions[0].code).toContain("goto('https://example.com')");
    });

    it('should generate code for dialog accept', () => {
      const recorder = new CodegenRecorder({ path: 'test.json', cwd: tempDir });
      const command: Command = {
        id: '1',
        action: 'dialog',
        response: 'accept',
      };
      const response: Response = { id: '1', success: true, data: {} };

      recorder.recordAction(command, response, {});
      const result = recorder.save();

      const content = JSON.parse(fs.readFileSync(result.path, 'utf-8')) as CodegenOutput;
      expect(content.actions[0].code).toContain("page.on('dialog'");
      expect(content.actions[0].code).toContain('dialog.accept()');
    });
  });
});
