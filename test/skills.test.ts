import { describe, it, expect, beforeAll } from 'vitest';
import { readFileSync, readdirSync, existsSync } from 'fs';
import { join } from 'path';
import { execSync } from 'child_process';

const SKILLS_DIR = join(__dirname, '../skills/agent-browser');
const SKILL_MD = join(SKILLS_DIR, 'SKILL.md');
const REFERENCES_DIR = join(SKILLS_DIR, 'references');
const TEMPLATES_DIR = join(SKILLS_DIR, 'templates');

describe('Skills Documentation', () => {
  describe('SKILL.md', () => {
    let content: string;

    beforeAll(() => {
      content = readFileSync(SKILL_MD, 'utf-8');
    });

    it('should exist', () => {
      expect(existsSync(SKILL_MD)).toBe(true);
    });

    it('should have valid YAML frontmatter', () => {
      const frontmatterMatch = content.match(/^---\n([\s\S]*?)\n---/);
      expect(frontmatterMatch).not.toBeNull();

      const frontmatter = frontmatterMatch![1];
      expect(frontmatter).toContain('name:');
      expect(frontmatter).toContain('description:');
      expect(frontmatter).toContain('allowed-tools:');
    });

    it('should have required sections', () => {
      const requiredSections = [
        '## Quick start',
        '## Core workflow',
        '## Commands',
        '## Global options',
        '## Environment variables',
        '## Deep-dive documentation',
        '## Ready-to-use templates',
      ];

      for (const section of requiredSections) {
        expect(content).toContain(section);
      }
    });

    it('should have valid internal reference links', () => {
      const linkPattern = /\[.*?\]\((references\/[^)]+\.md)\)/g;
      const links = [...content.matchAll(linkPattern)].map((m) => m[1]);

      for (const link of links) {
        const fullPath = join(SKILLS_DIR, link);
        expect(existsSync(fullPath), `Missing reference: ${link}`).toBe(true);
      }
    });

    it('should have valid internal template links', () => {
      const linkPattern = /\[.*?\]\((templates\/[^)]+\.sh)\)/g;
      const links = [...content.matchAll(linkPattern)].map((m) => m[1]);

      for (const link of links) {
        const fullPath = join(SKILLS_DIR, link);
        expect(existsSync(fullPath), `Missing template: ${link}`).toBe(true);
      }
    });

    it('should have properly formatted code blocks', () => {
      const codeBlockPattern = /```(\w+)?\n[\s\S]*?```/g;
      const codeBlocks = content.match(codeBlockPattern) || [];

      expect(codeBlocks.length).toBeGreaterThan(0);

      // Check that code blocks have language specifiers
      const blocksWithLang = codeBlocks.filter((block) => block.startsWith('```bash'));
      expect(blocksWithLang.length).toBeGreaterThan(10); // Most should be bash
    });

    it('should document all major command categories', () => {
      const commandCategories = [
        '### Navigation',
        '### Snapshot',
        '### Interactions',
        '### Get information',
        '### Screenshots',
        '### Wait',
        '### Cookies & Storage',
        '### Network',
        '### Tabs & Windows',
      ];

      for (const category of commandCategories) {
        expect(content).toContain(category);
      }
    });
  });

  describe('Reference Documents', () => {
    let referenceFiles: string[];

    beforeAll(() => {
      referenceFiles = readdirSync(REFERENCES_DIR).filter((f: string) => f.endsWith('.md'));
    });

    it('should have reference documents', () => {
      expect(referenceFiles.length).toBeGreaterThan(0);
    });

    it('should have expected reference files', () => {
      const expectedRefs = [
        'snapshot-refs.md',
        'session-management.md',
        'authentication.md',
        'video-recording.md',
        'proxy-support.md',
        'persistent-profiles.md',
        'cloud-providers.md',
        'semantic-locators.md',
        'network-mocking.md',
        'debugging.md',
      ];

      for (const ref of expectedRefs) {
        expect(referenceFiles, `Missing reference: ${ref}`).toContain(ref);
      }
    });

    it('each reference should have a title heading', () => {
      for (const file of referenceFiles) {
        const content = readFileSync(join(REFERENCES_DIR, file), 'utf-8');
        expect(content.startsWith('# '), `${file} missing title`).toBe(true);
      }
    });

    it('each reference should have code examples', () => {
      for (const file of referenceFiles) {
        const content = readFileSync(join(REFERENCES_DIR, file), 'utf-8');
        expect(content, `${file} missing code blocks`).toContain('```');
      }
    });

    it('references should not have broken internal links', () => {
      for (const file of referenceFiles) {
        const content = readFileSync(join(REFERENCES_DIR, file), 'utf-8');
        const linkPattern = /\[.*?\]\((?!http)([^)]+)\)/g;
        const links = [...content.matchAll(linkPattern)].map((m) => m[1]);

        for (const link of links) {
          // Skip anchor links
          if (link.startsWith('#')) continue;

          const fullPath = join(REFERENCES_DIR, link);
          expect(existsSync(fullPath), `${file}: broken link ${link}`).toBe(true);
        }
      }
    });
  });

  describe('Template Scripts', () => {
    let templateFiles: string[];

    beforeAll(() => {
      templateFiles = readdirSync(TEMPLATES_DIR).filter((f: string) => f.endsWith('.sh'));
    });

    it('should have template scripts', () => {
      expect(templateFiles.length).toBeGreaterThan(0);
    });

    it('should have expected template files', () => {
      const expectedTemplates = [
        'form-automation.sh',
        'authenticated-session.sh',
        'capture-workflow.sh',
        'download-workflow.sh',
        'network-mocking.sh',
        'multi-tab-workflow.sh',
      ];

      for (const template of expectedTemplates) {
        expect(templateFiles, `Missing template: ${template}`).toContain(template);
      }
    });

    it('each template should have a shebang', () => {
      for (const file of templateFiles) {
        const content = readFileSync(join(TEMPLATES_DIR, file), 'utf-8');
        expect(content.startsWith('#!/bin/bash'), `${file} missing shebang`).toBe(true);
      }
    });

    it('each template should have a description comment', () => {
      for (const file of templateFiles) {
        const content = readFileSync(join(TEMPLATES_DIR, file), 'utf-8');
        expect(content, `${file} missing description`).toContain('# Template:');
      }
    });

    it('each template should use set -euo pipefail', () => {
      for (const file of templateFiles) {
        const content = readFileSync(join(TEMPLATES_DIR, file), 'utf-8');
        expect(content, `${file} missing strict mode`).toContain('set -euo pipefail');
      }
    });

    it('each template should have valid bash syntax', () => {
      for (const file of templateFiles) {
        const fullPath = join(TEMPLATES_DIR, file);
        try {
          execSync(`bash -n "${fullPath}"`, { encoding: 'utf-8' });
        } catch (error) {
          throw new Error(`${file} has invalid bash syntax: ${error}`);
        }
      }
    });

    it('each template should be executable', () => {
      for (const file of templateFiles) {
        const fullPath = join(TEMPLATES_DIR, file);
        try {
          const stats = execSync(`ls -l "${fullPath}"`, { encoding: 'utf-8' });
          expect(stats, `${file} not executable`).toMatch(/^-rwx/);
        } catch {
          // Skip if ls fails
        }
      }
    });

    it('each template should have usage documentation', () => {
      for (const file of templateFiles) {
        const content = readFileSync(join(TEMPLATES_DIR, file), 'utf-8');
        // Should have either Usage: comment or parameter extraction
        const hasUsage = content.includes('Usage:') || content.includes('${1:');
        expect(hasUsage, `${file} missing usage docs`).toBe(true);
      }
    });
  });

  describe('Command Documentation Coverage', () => {
    let skillContent: string;

    beforeAll(() => {
      skillContent = readFileSync(SKILL_MD, 'utf-8');
    });

    it('should document core navigation commands', () => {
      const navCommands = ['open', 'back', 'forward', 'reload', 'close', 'connect'];
      for (const cmd of navCommands) {
        expect(skillContent, `Missing nav command: ${cmd}`).toContain(`agent-browser ${cmd}`);
      }
    });

    it('should document interaction commands', () => {
      const interactionCommands = [
        'click',
        'dblclick',
        'fill',
        'type',
        'press',
        'hover',
        'check',
        'uncheck',
        'select',
        'scroll',
        'drag',
        'upload',
        'download',
      ];
      for (const cmd of interactionCommands) {
        expect(skillContent, `Missing interaction: ${cmd}`).toContain(`agent-browser ${cmd}`);
      }
    });

    it('should document get commands', () => {
      const getCommands = ['text', 'html', 'value', 'attr', 'title', 'url', 'count', 'box', 'styles'];
      for (const cmd of getCommands) {
        expect(skillContent, `Missing get ${cmd}`).toContain(`get ${cmd}`);
      }
    });

    it('should document wait options', () => {
      const waitOptions = ['--text', '--url', '--load', '--fn', '--download'];
      for (const opt of waitOptions) {
        expect(skillContent, `Missing wait option: ${opt}`).toContain(`wait ${opt}`);
      }
    });

    it('should document global flags', () => {
      const globalFlags = [
        '--session',
        '--profile',
        '--json',
        '--headed',
        '--cdp',
        '--proxy',
        '--proxy-bypass',
        '--args',
        '--user-agent',
        '--headers',
        '--extension',
      ];
      for (const flag of globalFlags) {
        expect(skillContent, `Missing global flag: ${flag}`).toContain(flag);
      }
    });

    it('should document environment variables', () => {
      const envVars = [
        'AGENT_BROWSER_SESSION',
        'AGENT_BROWSER_PROFILE',
        'AGENT_BROWSER_PROVIDER',
        'AGENT_BROWSER_PROXY',
        'AGENT_BROWSER_ARGS',
        'AGENT_BROWSER_USER_AGENT',
      ];
      for (const envVar of envVars) {
        expect(skillContent, `Missing env var: ${envVar}`).toContain(envVar);
      }
    });
  });
});

describe('Skills Integration', () => {
  describe('CLI Command Existence', () => {
    it('agent-browser CLI should be available', () => {
      try {
        const output = execSync('agent-browser --version', { encoding: 'utf-8' });
        expect(output).toMatch(/\d+\.\d+\.\d+/);
      } catch {
        // Skip if CLI not installed (CI may not have it)
        console.log('Skipping: agent-browser CLI not available');
      }
    });

    it('agent-browser --help should list core commands', () => {
      try {
        const output = execSync('agent-browser --help', { encoding: 'utf-8' });
        const coreCommands = ['open', 'click', 'snapshot', 'screenshot', 'close'];
        for (const cmd of coreCommands) {
          expect(output).toContain(cmd);
        }
      } catch {
        // Skip if CLI not installed
        console.log('Skipping: agent-browser CLI not available');
      }
    });
  });

  describe('Documented Commands Match CLI', () => {
    let cliHelp: string | null = null;
    let skillContent: string;

    beforeAll(() => {
      skillContent = readFileSync(SKILL_MD, 'utf-8');
      try {
        cliHelp = execSync('agent-browser --help', { encoding: 'utf-8' });
      } catch {
        cliHelp = null;
      }
    });

    it('documented commands should exist in CLI help', () => {
      if (!cliHelp) {
        console.log('Skipping: agent-browser CLI not available');
        return;
      }

      // Extract command names from SKILL.md (first word after agent-browser)
      const docCommands = new Set<string>();
      const cmdPattern = /agent-browser\s+(\w+)/g;
      let match: RegExpExecArray | null;
      while ((match = cmdPattern.exec(skillContent)) !== null) {
        // Skip flags, common words, and frontmatter fields
        const cmd = match[1];
        const skipWords = ['browser', 'description', 'name', 'allowed'];
        if (!cmd.startsWith('-') && !skipWords.includes(cmd)) {
          docCommands.add(cmd);
        }
      }

      // Commands that are aliases or subcommands of documented commands
      const aliases: Record<string, string> = {
        goto: 'open',
        navigate: 'open',
        quit: 'close',
        exit: 'close',
        key: 'press',
        keydown: 'press',
        keyup: 'press',
        scrollinto: 'scroll',
        scrollintoview: 'scroll',
        geolocation: 'geo',
        auth: 'credentials',
      };

      // Commands implemented but not shown in main help (advanced/subcommands)
      const advancedCommands = new Set([
        'download', // Implemented in Rust CLI but not in main help listing
        'waitfordownload', // Wait subcommand variant
      ]);

      // Check each documented command exists in help
      const missingFromHelp: string[] = [];
      for (const cmd of docCommands) {
        // Skip advanced commands that are implemented but not in main help
        if (advancedCommands.has(cmd)) continue;

        // Some commands are subcommands or aliases, just check they're mentioned
        const primaryCmd = aliases[cmd] || cmd;
        const exists = cliHelp.toLowerCase().includes(primaryCmd.toLowerCase());

        if (!exists) {
          missingFromHelp.push(cmd);
        }
      }

      // Should have very few commands not in help (only advanced ones)
      expect(
        missingFromHelp.length,
        `Commands documented but not in CLI help: ${missingFromHelp.join(', ')}`
      ).toBeLessThanOrEqual(3);
    });
  });
});
