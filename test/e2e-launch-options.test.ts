import { describe, it, expect, beforeAll, beforeEach, afterEach } from 'vitest';
import { execSync } from 'child_process';
import { resolve } from 'path';
import { existsSync } from 'fs';

const CLI_PATH = resolve(__dirname, '../cli/target/release/agent-browser');

// Check if binary exists before running any tests
beforeAll(() => {
  if (!existsSync(CLI_PATH)) {
    throw new Error(`CLI binary not found at: ${CLI_PATH}. Please build the project first with 'cargo build --release'.`);
  }
});

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

// Helper to ensure browser is closed before test
function ensureBrowserClosed(session: string) {
  closeBrowser(session);
  // Wait a bit to ensure daemon is fully stopped
  execSync('sleep 0.2', { timeout: 1000 });
}

describe('E2E: Launch Options', () => {
  let session: string;

  beforeEach(() => {
    session = getUniqueSession();
    ensureBrowserClosed(session);
  });

  afterEach(() => {
    closeBrowser(session);
  });

  describe('--args flag', () => {
    it('should disable webdriver detection with --args', () => {
      runCli(session, '--args "--disable-blink-features=AutomationControlled" open https://example.com');
      const result = runCliJson(session, 'eval "navigator.webdriver"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(false);
    });

    it('should support multiple comma-separated args', () => {
      runCli(session, '--args "--disable-blink-features=AutomationControlled,--disable-dev-shm-usage" open https://example.com');
      const result = runCliJson(session, 'eval "navigator.webdriver"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(false);
    });

    it('should have webdriver=true without --args (default behavior)', () => {
      runCli(session, 'open https://example.com');
      const result = runCliJson(session, 'eval "navigator.webdriver"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(true);
    });
  });

  describe('--user-agent flag', () => {
    it('should set custom user-agent', () => {
      const customUA = 'E2ETestBot/1.0';
      runCli(session, `--user-agent "${customUA}" open https://example.com`);
      const result = runCliJson(session, 'eval "navigator.userAgent"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(customUA);
    });

    it('should use default Chrome user-agent when not specified', () => {
      runCli(session, 'open https://example.com');
      const result = runCliJson(session, 'eval "navigator.userAgent"');
      expect(result.success).toBe(true);
      expect(String(result.data?.result)).toContain('Chrome');
    });
  });

  describe('--proxy flag', () => {
    it('should fail navigation when proxy is unreachable (proves proxy is used)', () => {
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
    });
  });

  describe('environment variables', () => {
    it('should read AGENT_BROWSER_ARGS from environment', () => {
      runCliWithEnv(session, 'open https://example.com', {
        AGENT_BROWSER_ARGS: '--disable-blink-features=AutomationControlled',
      });
      const result = runCliJson(session, 'eval "navigator.webdriver"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(false);
    });

    it('should read AGENT_BROWSER_USER_AGENT from environment', () => {
      const customUA = 'EnvTestBot/2.0';
      runCliWithEnv(session, 'open https://example.com', {
        AGENT_BROWSER_USER_AGENT: customUA,
      });
      const result = runCliJson(session, 'eval "navigator.userAgent"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(customUA);
    });
  });

  describe('warning for already running daemon', () => {
    it('should warn when launch-time options are ignored', () => {
      // First, start daemon with default options
      runCli(session, 'open https://example.com');
      // Try to use --user-agent with already running daemon
      const output = execSync(
        `${CLI_PATH} --session ${session} --user-agent "IgnoredUA" get url 2>&1`,
        { encoding: 'utf-8' }
      );
      expect(output).toContain('--user-agent ignored');
      expect(output).toContain('daemon already running');
    });
  });

  describe('Single parameter tests', () => {
    it('--args only', () => {
      runCli(session, '--args "--disable-blink-features=AutomationControlled" open https://example.com');
      const result = runCliJson(session, 'eval "navigator.webdriver"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(false);
    });

    it('--user-agent only', () => {
      const ua = 'SingleParam/1.0';
      runCli(session, `--user-agent "${ua}" open https://example.com`);
      const result = runCliJson(session, 'eval "navigator.userAgent"');
      expect(result.success).toBe(true);
      expect(result.data?.result).toBe(ua);
    });

    it('--proxy only', () => {
      // Use real proxy to verify it works
      runCli(session, '--proxy "http://localhost:7890" open https://httpbin.org/ip');
      const urlResult = runCliJson(session, 'get url');
      expect(urlResult.success).toBe(true);
      expect(urlResult.data?.url).toBe('https://httpbin.org/ip');
    });
  });

  describe('Two parameter combinations', () => {
    it('--args + --user-agent', () => {
      const ua = 'TwoParam/1.0';
      runCli(session, `--args "--disable-blink-features=AutomationControlled" --user-agent "${ua}" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });

    it('--proxy + --user-agent', () => {
      const ua = 'ProxyUA/1.0';
      runCli(session, `--proxy "http://localhost:7890" --user-agent "${ua}" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);
    });

    it('--proxy + --args', () => {
      runCli(session, '--proxy "http://localhost:7890" --args "--disable-blink-features=AutomationControlled" open https://example.com');

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });

    it('--proxy + --proxy-bypass', () => {
      // example.com should bypass proxy
      runCli(session, '--proxy "http://localhost:7890" --proxy-bypass "example.com" open https://example.com');

      const urlResult = runCliJson(session, 'get url');
      expect(urlResult.success).toBe(true);
      expect(urlResult.data?.url).toBe('https://example.com/');

      const titleResult = runCliJson(session, 'get title');
      expect(titleResult.success).toBe(true);
      expect(String(titleResult.data?.title)).toContain('Example Domain');
    });
  });

  describe('Three parameter combinations', () => {
    it('--proxy + --user-agent + --args', () => {
      const ua = 'ThreeParam/1.0';
      runCli(session, `--proxy "http://localhost:7890" --user-agent "${ua}" --args "--disable-blink-features=AutomationControlled" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });

    it('--proxy + --proxy-bypass + --user-agent', () => {
      const ua = 'BypassUA/1.0';
      runCli(session, `--proxy "http://localhost:7890" --proxy-bypass "example.com" --user-agent "${ua}" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      const urlResult = runCliJson(session, 'get url');
      expect(urlResult.success).toBe(true);
      expect(urlResult.data?.url).toBe('https://example.com/');
    });

    it('--proxy + --proxy-bypass + --args', () => {
      runCli(session, '--proxy "http://localhost:7890" --proxy-bypass "example.com" --args "--disable-blink-features=AutomationControlled" open https://example.com');

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });

    it('--user-agent + --args + --proxy-bypass (no proxy)', () => {
      const ua = 'NoProxy/1.0';
      // Without proxy, proxy-bypass should be ignored
      runCli(session, `--user-agent "${ua}" --args "--disable-blink-features=AutomationControlled" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });
  });

  describe('Four parameter combinations (all parameters)', () => {
    it('--proxy + --proxy-bypass + --user-agent + --args', () => {
      const ua = 'AllParams/1.0';
      runCli(session, `--proxy "http://localhost:7890" --proxy-bypass "example.com" --user-agent "${ua}" --args "--disable-blink-features=AutomationControlled" open https://example.com`);

      // Verify user-agent
      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      // Verify webdriver hidden (args works)
      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);

      // Verify page loaded (proxy + bypass works)
      const urlResult = runCliJson(session, 'get url');
      expect(urlResult.success).toBe(true);
      expect(urlResult.data?.url).toBe('https://example.com/');

      // Navigate to httpbin through proxy (not bypassed)
      runCli(session, 'goto https://httpbin.org/headers');
      const bodyResult = runCliJson(session, 'get text body');
      expect(bodyResult.success).toBe(true);
      const text = String(bodyResult.data?.text);
      expect(text).toContain(ua);
    });
  });

  describe('Multiple args formats', () => {
    it('comma-separated args with proxy and user-agent', () => {
      const ua = 'CommaArgs/1.0';
      runCli(session, `--proxy "http://localhost:7890" --user-agent "${ua}" --args "--disable-blink-features=AutomationControlled,--disable-dev-shm-usage" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(ua);

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });
  });

  describe('Real-world scenarios', () => {
    it('mobile device simulation with proxy', () => {
      const mobileUA = 'Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15';
      runCli(session, `--proxy "http://localhost:7890" --proxy-bypass "example.com" --user-agent "${mobileUA}" --args "--disable-blink-features=AutomationControlled" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(String(uaResult.data?.result)).toContain('iPhone');

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);
    });

    it('stealth browsing with all anti-detection features', () => {
      const stealthUA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36';
      runCli(session, `--proxy "http://localhost:7890" --proxy-bypass "example.com,localhost" --user-agent "${stealthUA}" --args "--disable-blink-features=AutomationControlled,--disable-web-security" open https://example.com`);

      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(stealthUA);

      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);

      const pluginsResult = runCliJson(session, 'eval "typeof navigator.plugins"');
      expect(pluginsResult.success).toBe(true);
      expect(pluginsResult.data?.result).toBe('object');
    });

    it('combine proxy with user-agent and args', () => {
      const customUA = 'ProxyTestBot/1.0';
      runCli(session, `--proxy "http://localhost:7890" --user-agent "${customUA}" --args "--disable-blink-features=AutomationControlled" open https://example.com`);

      // navigator.userAgent
      const uaResult = runCliJson(session, 'eval "navigator.userAgent"');
      expect(uaResult.success).toBe(true);
      expect(uaResult.data?.result).toBe(customUA);

      // webdriver
      const wdResult = runCliJson(session, 'eval "navigator.webdriver"');
      expect(wdResult.success).toBe(true);
      expect(wdResult.data?.result).toBe(false);

      // proxy well
      const urlResult = runCliJson(session, 'get url');
      expect(urlResult.success).toBe(true);
      expect(urlResult.data?.url).toBe('https://example.com/');

      // proxy httpbin validate User-Agent
      runCli(session, 'goto https://httpbin.org/headers');
      const bodyResult = runCliJson(session, 'get text body');
      expect(bodyResult.success).toBe(true);
      const text = String(bodyResult.data?.text);
      expect(text).toContain(customUA);
    });
  });
});
