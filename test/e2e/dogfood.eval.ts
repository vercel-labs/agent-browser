import { describe, it, expect, beforeAll } from 'vitest';
import { query } from '@anthropic-ai/claude-agent-sdk';
import type { SDKMessage, SDKResultMessage } from '@anthropic-ai/claude-agent-sdk';
import { mkdirSync, readFileSync, existsSync, readdirSync, rmSync } from 'node:fs';
import path from 'node:path';

const AI_GATEWAY_URL =
  process.env.ANTHROPIC_BASE_URL || 'https://ai-gateway.vercel.sh';
const API_KEY = process.env.AI_GATEWAY_API_KEY;
const MODEL = process.env.DOGFOOD_MODEL || 'anthropic/claude-haiku-4.5';
const CUSTOM_URL = process.env.DOGFOOD_URL;

const FIXTURE_PATH = path.resolve('test/e2e/fixtures/buggy-app.html');
const SKILL_PATH = path.resolve('skills/dogfood/SKILL.md');
const TARGET_URL = CUSTOM_URL || `file://${FIXTURE_PATH}`;
const IS_FIXTURE = !CUSTOM_URL;

const OUTPUT_DIR = path.resolve('test/e2e/.dogfood-output');
const EVAL_TIMEOUT = 10 * 60 * 1000;

async function runDogfood(outputDir: string): Promise<{
  result: SDKResultMessage | null;
  messages: SDKMessage[];
  toolsUsed: Set<string>;
}> {
  const instruction = [
    `Read the dogfood skill at ${SKILL_PATH} and follow its workflow.`,
    `Target URL: ${TARGET_URL}`,
    `Session: dogfood-eval`,
    `Output directory: ${outputDir}`,
    `No authentication needed.`,
    `Scope: full page.`,
    `Aim to find at least 5 issues, then wrap up.`,
    `When done, make sure the report.md summary counts match the actual issues found.`,
  ].join(' ');

  const messages: SDKMessage[] = [];
  const toolsUsed = new Set<string>();
  let result: SDKResultMessage | null = null;

  const conversation = query({
    prompt: instruction,
    options: {
      model: MODEL,
      cwd: process.cwd(),
      allowedTools: ['Bash', 'Read', 'Write', 'Edit', 'Glob', 'Grep'],
      permissionMode: 'bypassPermissions',
      allowDangerouslySkipPermissions: true,
      maxTurns: 40,
      settingSources: ['project'],
      persistSession: false,
      env: {
        ...process.env,
        ANTHROPIC_BASE_URL: AI_GATEWAY_URL,
        ANTHROPIC_API_KEY: API_KEY,
      },
    },
  });

  const verbose = process.env.DOGFOOD_VERBOSE !== '0';
  const log = verbose ? (msg: string) => process.stderr.write(`  [dogfood] ${msg}\n`) : () => {};

  for await (const message of conversation) {
    messages.push(message);

    if (message.type === 'system' && message.subtype === 'init') {
      log(`session started (model: ${message.model})`);
    }

    if (message.type === 'assistant' && message.message?.content) {
      for (const block of message.message.content) {
        if ('type' in block && block.type === 'tool_use') {
          toolsUsed.add(block.name);
          const input = block.input as Record<string, unknown>;
          const preview = block.name === 'Bash'
            ? String(input.command ?? '').slice(0, 120)
            : block.name === 'Write'
              ? String(input.file_path ?? input.path ?? '')
              : block.name === 'Read'
                ? String(input.file_path ?? input.path ?? '')
                : JSON.stringify(input).slice(0, 80);
          log(`${block.name}: ${preview}`);
        }
        if ('type' in block && block.type === 'text' && block.text) {
          const line = block.text.split('\n')[0].slice(0, 120);
          if (line.trim()) log(line);
        }
      }
    }

    if (message.type === 'result') {
      result = message;
      if (result.subtype === 'success') {
        log(`done (${result.num_turns} turns, $${result.total_cost_usd.toFixed(4)})`);
      } else {
        log(`failed: ${result.subtype}`);
      }
    }
  }

  return { result, messages, toolsUsed };
}

function findFiles(dir: string, ext: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { recursive: true })
    .map(String)
    .filter((f) => f.endsWith(ext));
}

describe.skipIf(!API_KEY)('Dogfood e2e eval (Agent SDK)', () => {
  const outputDir = OUTPUT_DIR;
  let evalResult: Awaited<ReturnType<typeof runDogfood>>;

  beforeAll(async () => {
    if (existsSync(outputDir)) {
      rmSync(outputDir, { recursive: true, force: true });
    }
    mkdirSync(outputDir, { recursive: true });
    evalResult = await runDogfood(outputDir);
  }, EVAL_TIMEOUT);

  it('completes successfully', () => {
    expect(evalResult.result, 'No result message received').toBeTruthy();
    expect(
      evalResult.result!.subtype,
      `Agent failed: ${evalResult.result!.subtype}`
    ).toBe('success');
  });

  it('used agent-browser via Bash tool', () => {
    expect(
      evalResult.toolsUsed.has('Bash'),
      'Agent never used Bash (needed for agent-browser commands)'
    ).toBe(true);
  });

  it('produced a report file', () => {
    const reportPath = path.join(outputDir, 'report.md');
    expect(existsSync(reportPath), 'report.md not found in output dir').toBe(
      true
    );
  });

  it('found a minimum number of issues', () => {
    const reportPath = path.join(outputDir, 'report.md');
    if (!existsSync(reportPath)) return;
    const report = readFileSync(reportPath, 'utf-8');

    const issueBlocks = report.match(/###\s+ISSUE-\d+/g) || [];
    if (IS_FIXTURE) {
      expect(
        issueBlocks.length,
        `Expected >=3 issues from fixture, found ${issueBlocks.length}`
      ).toBeGreaterThanOrEqual(3);
    } else {
      expect(issueBlocks.length).toBeGreaterThanOrEqual(1);
    }
  });

  it('each issue has required fields and repro evidence', () => {
    const reportPath = path.join(outputDir, 'report.md');
    if (!existsSync(reportPath)) return;
    const report = readFileSync(reportPath, 'utf-8');

    const issueSections = report.split(/(?=###\s+ISSUE-\d+)/).slice(1);
    for (const section of issueSections) {
      const issueId = section.match(/ISSUE-\d+/)?.[0] ?? 'unknown';

      expect(section, `${issueId}: missing Severity`).toMatch(
        /\*\*Severity\*\*/i
      );

      const sevMatch = section.match(
        /\*\*Severity\*\*\s*\|?\s*(critical|high|medium|low)/i
      );
      expect(sevMatch, `${issueId}: invalid severity value`).toBeTruthy();

      expect(section, `${issueId}: missing Category`).toMatch(
        /\*\*Category\*\*/i
      );

      expect(section, `${issueId}: missing URL`).toMatch(/\*\*URL\*\*/i);

      expect(section, `${issueId}: missing Repro Video`).toMatch(
        /\*\*Repro Video\*\*/i
      );

      expect(section, `${issueId}: missing Repro Steps`).toMatch(
        /\*\*Repro Steps\*\*/i
      );

      expect(
        section,
        `${issueId}: no screenshot refs in repro steps`
      ).toMatch(/!\[.*?\]\(.*?\)/);
    }
  });

  it('has a summary table with non-zero total', () => {
    const reportPath = path.join(outputDir, 'report.md');
    if (!existsSync(reportPath)) return;
    const report = readFileSync(reportPath, 'utf-8');

    expect(report, 'Missing Summary section').toContain('## Summary');
    const totalMatch = report.match(/\*\*Total\*\*\s*\|?\s*\*\*(\d+)\*\*/);
    expect(totalMatch, 'Summary Total not found').toBeTruthy();
    if (totalMatch) {
      const total = parseInt(totalMatch[1], 10);
      expect(total, 'Summary Total should be > 0').toBeGreaterThan(0);
    }
  });

  it('produced screenshot files', () => {
    const screenshotsDir = path.join(outputDir, 'screenshots');
    const screenshots = findFiles(screenshotsDir, '.png');
    expect(
      screenshots.length,
      'No screenshot files found in output'
    ).toBeGreaterThan(0);
  });

  it('produced video files', () => {
    const videosDir = path.join(outputDir, 'videos');
    const videos = findFiles(videosDir, '.webm');
    expect(
      videos.length,
      'No video files found in output'
    ).toBeGreaterThan(0);
  });
});
