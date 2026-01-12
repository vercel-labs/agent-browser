import { describe, it, expect } from 'vitest';
import { execSync } from 'child_process';
import { resolve } from 'path';

const CLI_PATH = resolve(__dirname, '../cli/target/release/agent-browser');

// Generate unique session for each test
let testCounter = 0;
function getUniqueSession(): string {
  return `e2e-${Date.now()}-${testCounter++}`;
}

function runCli(session: string, args: string): string {
  return execSync(`${CLI_PATH} --session ${session} ${args}`, {
    encoding: 'utf-8',
    timeout: 30000,
  }).trim();
}

function runCliWithEnv(session: string, args: string, envVars: Record<string, string>): string {
  const env = { ...process.env, ...envVars };
  return execSync(`${CLI_PATH} --session ${session} ${args}`, {
    encoding: 'utf-8',
    env,
    timeout: 30000,
  }).trim();
}

function runCliJson(session: string, args: string): {
  success: boolean;
  data?: Record<string, unknown>;
  error?: string;
} {
  const output = runCli(session, `${args} --json`);
  return JSON.parse(output);
}

function closeBrowser(session: string) {
  try {
    execSync(`${CLI_PATH} --session ${session} close`, {
      encoding: 'utf-8',
      timeout: 10000,
    });
  } catch {
    // Ignore if already closed
  }
}

describe('E2E: Launch Options', () => {
  describe('--args flag', () => {
    it('should disable webdriver detection with --args', () => {
      const session = getUniqueSession();
      try {
        runCli(session, '--args "--disable-blink-features=AutomationControlled" open https://example.com');
        const result = runCliJson(session, 'eval "navigator.webdriver"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(false);
      } finally {
        closeBrowser(session);
      }
    });

    it('should support multiple comma-separated args', () => {
      const session = getUniqueSession();
      try {
        runCli(session, '--args "--disable-blink-features=AutomationControlled,--disable-dev-shm-usage" open https://example.com');
        const result = runCliJson(session, 'eval "navigator.webdriver"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(false);
      } finally {
        closeBrowser(session);
      }
    });

    it('should have webdriver=true without --args (default behavior)', () => {
      const session = getUniqueSession();
      try {
        runCli(session, 'open https://example.com');
        const result = runCliJson(session, 'eval "navigator.webdriver"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(true);
      } finally {
        closeBrowser(session);
      }
    });
  });

  describe('--user-agent flag', () => {
    it('should set custom user-agent', () => {
      const session = getUniqueSession();
      const customUA = 'E2ETestBot/1.0';
      try {
        runCli(session, `--user-agent "${customUA}" open https://example.com`);
        const result = runCliJson(session, 'eval "navigator.userAgent"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(customUA);
      } finally {
        closeBrowser(session);
      }
    });

    it('should use default Chrome user-agent when not specified', () => {
      const session = getUniqueSession();
      try {
        runCli(session, 'open https://example.com');
        const result = runCliJson(session, 'eval "navigator.userAgent"');
        expect(result.success).toBe(true);
        expect(String(result.data?.result)).toContain('Chrome');
      } finally {
        closeBrowser(session);
      }
    });
  });

  describe('--proxy flag', () => {
    it('should fail navigation when proxy is unreachable (proves proxy is used)', () => {
      const session = getUniqueSession();
      try {
        // Launch with unreachable proxy - navigation should fail immediately
        // This proves proxy is being used
        let failed = false;
        try {
          runCli(session, '--proxy "http://127.0.0.1:59999" open https://example.com');
        } catch (e) {
          failed = true;
          expect(String(e)).toContain('ERR_PROXY_CONNECTION_FAILED');
        }
        expect(failed).toBe(true);
      } finally {
        closeBrowser(session);
      }
    });
  });

  describe('environment variables', () => {
    it('should read AGENT_BROWSER_ARGS from environment', () => {
      const session = getUniqueSession();
      try {
        runCliWithEnv(session, 'open https://example.com', {
          AGENT_BROWSER_ARGS: '--disable-blink-features=AutomationControlled',
        });
        const result = runCliJson(session, 'eval "navigator.webdriver"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(false);
      } finally {
        closeBrowser(session);
      }
    });

    it('should read AGENT_BROWSER_USER_AGENT from environment', () => {
      const session = getUniqueSession();
      const customUA = 'EnvTestBot/2.0';
      try {
        runCliWithEnv(session, 'open https://example.com', {
          AGENT_BROWSER_USER_AGENT: customUA,
        });
        const result = runCliJson(session, 'eval "navigator.userAgent"');
        expect(result.success).toBe(true);
        expect(result.data?.result).toBe(customUA);
      } finally {
        closeBrowser(session);
      }
    });
  });

  describe('warning for already running daemon', () => {
    it('should warn when launch-time options are ignored', () => {
      const session = getUniqueSession();
      try {
        // First, start daemon with default options
        runCli(session, 'open https://example.com');
        // Try to use --user-agent with already running daemon
        const output = execSync(
          `${CLI_PATH} --session ${session} --user-agent "IgnoredUA" get url 2>&1`,
          { encoding: 'utf-8' }
        );
        expect(output).toContain('--user-agent ignored');
        expect(output).toContain('daemon already running');
      } finally {
        closeBrowser(session);
      }
    });
  });

  describe('combined options', () => {
    it('should support --args and --user-agent together', () => {
      const session = getUniqueSession();
      const customUA = 'CombinedE2EBot/3.0';
      try {
        runCli(session, `--args "--disable-blink-features=AutomationControlled" --user-agent "${customUA}" open https://example.com`);
        // Verify user-agent
        const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
        expect(uaResult.success).toBe(true);
        expect(uaResult.data?.result).toBe(customUA);
        // Verify webdriver is hidden
        const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
        expect(wdResult.success).toBe(true);
        expect(wdResult.data?.result).toBe(false);
      } finally {
        closeBrowser(session);
      }
    });
  });
});
