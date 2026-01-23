import { describe, it, expect } from 'vitest';
import { execSync, spawnSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

const binDir = path.join(__dirname, '..', 'bin');

describe('CLI Entry Point', () => {
  describe('agent-browser.cjs', () => {
    const cjsPath = path.join(binDir, 'agent-browser.cjs');

    it('should exist as cross-platform entry point', () => {
      expect(fs.existsSync(cjsPath)).toBe(true);
    });

    it('should have correct shebang for Node.js', () => {
      const content = fs.readFileSync(cjsPath, 'utf-8');
      expect(content.startsWith('#!/usr/bin/env node')).toBe(true);
    });

    it('should detect platform correctly', () => {
      const content = fs.readFileSync(cjsPath, 'utf-8');
      // Should handle win32, darwin, linux
      expect(content).toContain("case 'win32':");
      expect(content).toContain("case 'darwin':");
      expect(content).toContain("case 'linux':");
    });

    it('should detect architecture correctly', () => {
      const content = fs.readFileSync(cjsPath, 'utf-8');
      // Should handle x64 and arm64
      expect(content).toContain("case 'x64':");
      expect(content).toContain("case 'arm64':");
    });

    it('should use windowsHide option for spawn', () => {
      const content = fs.readFileSync(cjsPath, 'utf-8');
      expect(content).toContain('windowsHide: true');
    });

    it('should run successfully via node', () => {
      // This will test the actual execution
      const result = spawnSync('node', [cjsPath, '--version'], {
        cwd: binDir,
        encoding: 'utf-8',
        timeout: 10000,
      });

      // Should either succeed or fail with "binary not found"
      // Both are acceptable - we're testing the entry point, not the binary
      const output = result.stdout + result.stderr;
      const validOutput =
        output.includes('agent-browser') || // version output
        output.includes('Error: Binary not found') || // binary missing
        result.status === 0;

      expect(validOutput).toBe(true);
    });
  });

  describe('package.json bin configuration', () => {
    const packageJsonPath = path.join(__dirname, '..', 'package.json');

    it('should point to agent-browser.cjs', () => {
      const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf-8'));
      expect(packageJson.bin['agent-browser']).toBe('./bin/agent-browser.cjs');
    });
  });

  describe('Windows compatibility', () => {
    it('should work in PowerShell without /bin/sh dependency', () => {
      if (os.platform() !== 'win32') {
        // Skip on non-Windows
        return;
      }

      // Test that the CJS entry point works in PowerShell
      const cjsPath = path.join(binDir, 'agent-browser.cjs');
      const result = spawnSync(
        'powershell.exe',
        ['-NoProfile', '-Command', `node "${cjsPath}" --version`],
        {
          encoding: 'utf-8',
          timeout: 15000,
        }
      );

      const output = result.stdout + result.stderr;
      // Should not contain /bin/sh error
      expect(output).not.toContain('/bin/sh');
      expect(output).not.toContain('is not recognized');
    });

    it('should work in CMD without /bin/sh dependency', () => {
      if (os.platform() !== 'win32') {
        // Skip on non-Windows
        return;
      }

      const cjsPath = path.join(binDir, 'agent-browser.cjs');
      const result = spawnSync('cmd.exe', ['/c', `node "${cjsPath}" --version`], {
        encoding: 'utf-8',
        timeout: 15000,
      });

      const output = result.stdout + result.stderr;
      // Should not contain /bin/sh error
      expect(output).not.toContain('/bin/sh');
      expect(output).not.toContain('is not recognized');
    });
  });

  describe('agent-browser.cmd (Windows local dev)', () => {
    const cmdPath = path.join(binDir, 'agent-browser.cmd');

    it('should exist', () => {
      expect(fs.existsSync(cmdPath)).toBe(true);
    });

    it('should call native binary directly (not dist/index.js)', () => {
      const content = fs.readFileSync(cmdPath, 'utf-8');
      // Should NOT point to dist/index.js
      expect(content).not.toContain('dist\\index.js');
      expect(content).not.toContain('dist/index.js');
      // Should call the native binary
      expect(content).toContain('agent-browser-win32-');
    });

    it('should detect architecture', () => {
      const content = fs.readFileSync(cmdPath, 'utf-8');
      expect(content).toContain('PROCESSOR_ARCHITECTURE');
      expect(content).toContain('AMD64');
      expect(content).toContain('ARM64');
    });

    it('should run successfully on Windows', () => {
      if (os.platform() !== 'win32') {
        // Skip on non-Windows
        return;
      }

      const result = spawnSync(cmdPath, ['--version'], {
        cwd: binDir,
        encoding: 'utf-8',
        timeout: 10000,
        shell: true,
      });

      const output = result.stdout + result.stderr;
      // Should either show version or "binary not found" error
      const validOutput =
        output.includes('agent-browser') ||
        output.includes('No binary found') ||
        result.status === 0;

      expect(validOutput).toBe(true);
    });
  });
});
