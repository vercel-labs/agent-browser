/**
 * Run agent-browser inside a Vercel Sandbox.
 *
 * No external server needed -- a Linux microVM spins up on demand,
 * runs agent-browser + headless Chrome, and shuts down when done.
 *
 * For production, create a snapshot with agent-browser and Chromium
 * pre-installed so startup is sub-second instead of ~30s.
 */

import { Sandbox } from "@vercel/sandbox";

export type SandboxResult = {
  exitCode: number;
  stdout: string;
  stderr: string;
};

export type StepEvent = {
  step: string;
  status: "running" | "done" | "error";
  elapsed?: number;
};

export type OnStep = (event: StepEvent) => void;

const SNAPSHOT_ID = process.env.AGENT_BROWSER_SNAPSHOT_ID;

const CHROMIUM_SYSTEM_DEPS = [
  "nss",
  "nspr",
  "libxkbcommon",
  "atk",
  "at-spi2-atk",
  "at-spi2-core",
  "libXcomposite",
  "libXdamage",
  "libXrandr",
  "libXfixes",
  "libXcursor",
  "libXi",
  "libXtst",
  "libXScrnSaver",
  "libXext",
  "mesa-libgbm",
  "libdrm",
  "mesa-libGL",
  "mesa-libEGL",
  "cups-libs",
  "alsa-lib",
  "pango",
  "cairo",
  "gtk3",
  "dbus-libs",
];

/**
 * Returns credentials to spread into Sandbox.create() calls.
 * When explicit env vars are set they take precedence; otherwise returns
 * an empty object so the SDK falls back to VERCEL_OIDC_TOKEN automatically.
 */
export function getSandboxCredentials():
  | { token: string; teamId: string; projectId: string }
  | Record<string, never> {
  if (
    process.env.VERCEL_TOKEN &&
    process.env.VERCEL_TEAM_ID &&
    process.env.VERCEL_PROJECT_ID
  ) {
    return {
      token: process.env.VERCEL_TOKEN,
      teamId: process.env.VERCEL_TEAM_ID,
      projectId: process.env.VERCEL_PROJECT_ID,
    };
  }
  return {};
}

async function runStep<T>(
  step: string,
  fn: () => Promise<T>,
  onStep?: OnStep,
): Promise<T> {
  const start = Date.now();
  onStep?.({ step, status: "running" });
  try {
    const result = await fn();
    onStep?.({ step, status: "done", elapsed: Date.now() - start });
    return result;
  } catch (err) {
    onStep?.({ step, status: "error", elapsed: Date.now() - start });
    throw err;
  }
}

/**
 * Install system dependencies + agent-browser + Chromium into a fresh sandbox.
 * The sandbox base image is Amazon Linux (dnf).
 */
async function bootstrapSandbox(
  sandbox: InstanceType<typeof Sandbox>,
  onStep?: OnStep,
): Promise<void> {
  await runStep("Installing system dependencies", async () => {
    await sandbox.runCommand("sh", [
      "-c",
      `sudo dnf clean all 2>&1 && sudo dnf install -y --skip-broken ${CHROMIUM_SYSTEM_DEPS.join(" ")} 2>&1 && sudo ldconfig 2>&1`,
    ]);
  }, onStep);

  await runStep("Installing agent-browser", async () => {
    await sandbox.runCommand("npm", ["install", "-g", "agent-browser"]);
    await sandbox.runCommand("npx", ["agent-browser", "install"]);
  }, onStep);
}

async function createSandbox(
  onStep?: OnStep,
): Promise<InstanceType<typeof Sandbox>> {
  const credentials = getSandboxCredentials();

  return runStep(
    SNAPSHOT_ID ? "Booting sandbox from snapshot" : "Creating sandbox",
    async () => {
      if (SNAPSHOT_ID) {
        return Sandbox.create({
          ...credentials,
          source: { type: "snapshot", snapshotId: SNAPSHOT_ID },
          timeout: 120_000,
        });
      }

      const sb = await Sandbox.create({
        ...credentials,
        runtime: "node24",
        timeout: 120_000,
      });
      await bootstrapSandbox(sb, onStep);
      return sb;
    },
    onStep,
  );
}

async function exec(
  sandbox: InstanceType<typeof Sandbox>,
  cmd: string,
  args: string[],
  onStep?: OnStep,
  stepLabel?: string,
): Promise<SandboxResult> {
  const label = stepLabel || `${cmd} ${args.join(" ")}`;

  return runStep(label, async () => {
    const result = await sandbox.runCommand(cmd, args);
    const stdout = await result.stdout();
    const stderr = await result.stderr();

    if (result.exitCode !== 0) {
      throw new Error(
        `Command "${cmd} ${args.join(" ")}" failed (exit ${result.exitCode}): ${stderr || stdout}`,
      );
    }

    return { exitCode: result.exitCode, stdout, stderr };
  }, onStep);
}

/**
 * Screenshot a URL using agent-browser inside a Vercel Sandbox.
 * Returns base64-encoded PNG.
 */
export async function screenshotUrl(
  url: string,
  opts: { fullPage?: boolean; onStep?: OnStep } = {},
): Promise<{ screenshot: string; title: string }> {
  const { onStep } = opts;
  const sandbox = await createSandbox(onStep);

  try {
    await exec(sandbox, "agent-browser", ["open", "about:blank"], onStep, "Starting browser");
    await exec(sandbox, "agent-browser", ["open", url], onStep, `Navigating to ${url}`);

    const titleResult = await exec(
      sandbox,
      "agent-browser",
      ["get", "title", "--json"],
      onStep,
      "Getting page title",
    );
    const title = tryParseJson(titleResult.stdout)?.data?.title || url;

    const screenshotArgs = ["screenshot", "--json"];
    if (opts.fullPage) screenshotArgs.push("--full");
    const ssResult = await exec(
      sandbox,
      "agent-browser",
      screenshotArgs,
      onStep,
      "Taking screenshot",
    );
    const ssData = tryParseJson(ssResult.stdout)?.data;
    const screenshotPath = ssData?.path;

    if (!screenshotPath) {
      throw new Error(
        `Screenshot returned no file path. Raw output: ${ssResult.stdout.slice(0, 500)}`,
      );
    }

    const b64Result = await exec(
      sandbox,
      "base64",
      ["-w", "0", screenshotPath],
      onStep,
      "Encoding screenshot",
    );
    const screenshot = b64Result.stdout.trim();

    if (!screenshot) {
      throw new Error("Failed to read screenshot file from sandbox");
    }

    await exec(sandbox, "agent-browser", ["close"], onStep, "Closing browser");

    return { screenshot, title };
  } finally {
    await runStep("Stopping sandbox", () => sandbox.stop(), onStep);
  }
}

/**
 * Snapshot a URL (accessibility tree) using agent-browser inside a Vercel Sandbox.
 */
export async function snapshotUrl(
  url: string,
  opts: { interactive?: boolean; compact?: boolean; onStep?: OnStep } = {},
): Promise<{ snapshot: string; title: string }> {
  const { onStep } = opts;
  const sandbox = await createSandbox(onStep);

  try {
    await exec(sandbox, "agent-browser", ["open", "about:blank"], onStep, "Starting browser");
    await exec(sandbox, "agent-browser", ["open", url], onStep, `Navigating to ${url}`);

    const titleResult = await exec(
      sandbox,
      "agent-browser",
      ["get", "title", "--json"],
      onStep,
      "Getting page title",
    );
    const title = tryParseJson(titleResult.stdout)?.data?.title || url;

    const snapshotArgs = ["snapshot"];
    if (opts.interactive) snapshotArgs.push("-i");
    if (opts.compact) snapshotArgs.push("-c");
    const snapResult = await exec(
      sandbox,
      "agent-browser",
      snapshotArgs,
      onStep,
      "Taking accessibility snapshot",
    );

    if (!snapResult.stdout.trim()) {
      throw new Error("Snapshot returned empty data");
    }

    await exec(sandbox, "agent-browser", ["close"], onStep, "Closing browser");

    return { snapshot: snapResult.stdout, title };
  } finally {
    await runStep("Stopping sandbox", () => sandbox.stop(), onStep);
  }
}

/**
 * Run arbitrary agent-browser commands inside a Vercel Sandbox.
 * Each command is a string array like ["open", "https://example.com"].
 */
export async function runCommands(
  commands: string[][],
): Promise<SandboxResult[]> {
  const sandbox = await createSandbox();

  try {
    const results: SandboxResult[] = [];
    for (const args of commands) {
      const result = await exec(sandbox, "agent-browser", args);
      results.push(result);
    }
    return results;
  } finally {
    await sandbox.stop();
  }
}

/**
 * Create a reusable snapshot with agent-browser + Chromium pre-installed.
 * Run this once, then set AGENT_BROWSER_SNAPSHOT_ID for fast startup.
 */
export async function createSnapshot(): Promise<string> {
  const sandbox = await Sandbox.create({
    ...getSandboxCredentials(),
    runtime: "node24",
    timeout: 300_000,
  });

  await bootstrapSandbox(sandbox);

  const snapshot = await sandbox.snapshot();
  return snapshot.snapshotId;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function tryParseJson(str: string): any {
  try {
    return JSON.parse(str);
  } catch {
    return null;
  }
}
