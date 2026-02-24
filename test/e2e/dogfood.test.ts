import { describe, it, expect } from 'vitest';
import { readFileSync, existsSync } from 'node:fs';
import path from 'node:path';

const SKILL_DIR = path.resolve('skills/dogfood');
const SKILL_MD = path.join(SKILL_DIR, 'SKILL.md');
const TAXONOMY_MD = path.join(SKILL_DIR, 'references', 'issue-taxonomy.md');
const TEMPLATE_MD = path.join(SKILL_DIR, 'templates', 'dogfood-report-template.md');

function readSkillFile(filePath: string): string {
  return readFileSync(filePath, 'utf-8');
}

function parseFrontmatter(content: string): Record<string, string> {
  const match = content.match(/^---\n([\s\S]*?)\n---/);
  if (!match) return {};
  const fields: Record<string, string> = {};
  for (const line of match[1].split('\n')) {
    const colonIdx = line.indexOf(':');
    if (colonIdx > 0) {
      fields[line.slice(0, colonIdx).trim()] = line.slice(colonIdx + 1).trim();
    }
  }
  return fields;
}

describe('Dogfood skill: file structure', () => {
  it('SKILL.md exists', () => {
    expect(existsSync(SKILL_MD)).toBe(true);
  });

  it('references/issue-taxonomy.md exists', () => {
    expect(existsSync(TAXONOMY_MD)).toBe(true);
  });

  it('templates/dogfood-report-template.md exists', () => {
    expect(existsSync(TEMPLATE_MD)).toBe(true);
  });
});

describe('Dogfood skill: SKILL.md frontmatter', () => {
  const content = readSkillFile(SKILL_MD);
  const frontmatter = parseFrontmatter(content);

  it('has name field', () => {
    expect(frontmatter.name).toBe('dogfood');
  });

  it('has description field', () => {
    expect(frontmatter.description).toBeTruthy();
    expect(frontmatter.description!.length).toBeGreaterThan(50);
  });

  it('has allowed-tools field', () => {
    expect(frontmatter['allowed-tools']).toBeTruthy();
    expect(frontmatter['allowed-tools']).toContain('agent-browser');
  });
});

describe('Dogfood skill: SKILL.md body references', () => {
  const content = readSkillFile(SKILL_MD);

  it('references issue-taxonomy.md', () => {
    expect(content).toContain('references/issue-taxonomy.md');
  });

  it('references dogfood-report-template.md', () => {
    expect(content).toContain('templates/dogfood-report-template.md');
  });

  it('referenced files exist on disk', () => {
    const refPattern = /\[.*?\]\((references\/.*?\.md|templates\/.*?\.md)\)/g;
    const refs = [...content.matchAll(refPattern)].map((m) => m[1]);
    expect(refs.length).toBeGreaterThan(0);
    for (const ref of refs) {
      const fullPath = path.join(SKILL_DIR, ref);
      expect(existsSync(fullPath), `Missing: ${ref}`).toBe(true);
    }
  });
});

describe('Dogfood skill: report template', () => {
  const template = readSkillFile(TEMPLATE_MD);

  it('has ISSUE- prefix in issue blocks', () => {
    expect(template).toContain('ISSUE-');
  });

  it('has Severity field', () => {
    expect(template).toContain('**Severity**');
  });

  it('has Category field', () => {
    expect(template).toContain('**Category**');
  });

  it('has URL field', () => {
    expect(template).toContain('**URL**');
  });

  it('has Repro Video field', () => {
    expect(template).toContain('**Repro Video**');
  });

  it('has Repro Steps section', () => {
    expect(template).toContain('**Repro Steps**');
  });

  it('has screenshot image references in repro steps', () => {
    expect(template).toMatch(/!\[.*?\]\(screenshots\//);
  });

  it('lists all valid severity values', () => {
    expect(template).toMatch(/critical\s*\/\s*high\s*\/\s*medium\s*\/\s*low/);
  });

  it('lists all valid category values', () => {
    const categoryLine = template
      .split('\n')
      .find((l) => l.includes('**Category**'));
    expect(categoryLine).toBeTruthy();
    for (const cat of [
      'visual',
      'functional',
      'ux',
      'content',
      'performance',
      'console',
      'accessibility',
    ]) {
      expect(categoryLine!.toLowerCase()).toContain(cat);
    }
  });

  it('has Summary table with severity counts', () => {
    expect(template).toContain('## Summary');
    for (const sev of ['Critical', 'High', 'Medium', 'Low', 'Total']) {
      expect(template).toContain(sev);
    }
  });
});

describe('Dogfood skill: issue taxonomy', () => {
  const taxonomy = readSkillFile(TAXONOMY_MD);

  it('has severity level definitions', () => {
    expect(taxonomy).toContain('## Severity Levels');
    for (const sev of ['critical', 'high', 'medium', 'low']) {
      expect(taxonomy.toLowerCase()).toContain(`**${sev}**`);
    }
  });

  it('has all 7 category sections', () => {
    const expectedCategories = [
      'Visual',
      'Functional',
      'UX',
      'Content',
      'Performance',
      'Console',
      'Accessibility',
    ];
    for (const cat of expectedCategories) {
      expect(taxonomy).toMatch(new RegExp(`###\\s+.*${cat}`, 'i'));
    }
  });

  it('has exploration checklist', () => {
    expect(taxonomy).toContain('## Exploration Checklist');
  });

  it('checklist has numbered items', () => {
    const checklistSection = taxonomy.split('## Exploration Checklist')[1];
    expect(checklistSection).toBeTruthy();
    const numberedItems = checklistSection!.match(/^\d+\./gm);
    expect(numberedItems!.length).toBeGreaterThanOrEqual(5);
  });
});

describe('Dogfood skill: cross-consistency', () => {
  const template = readSkillFile(TEMPLATE_MD);
  const taxonomy = readSkillFile(TAXONOMY_MD);

  it('every category in template exists in taxonomy', () => {
    const categoryLine = template
      .split('\n')
      .find((l) => l.includes('**Category**'));
    expect(categoryLine).toBeTruthy();

    const categories = categoryLine!
      .split('|')
      .pop()!
      .split('/')
      .map((c) => c.trim().toLowerCase())
      .filter(Boolean);

    for (const cat of categories) {
      expect(
        taxonomy.toLowerCase(),
        `Category "${cat}" from template not found in taxonomy`
      ).toMatch(new RegExp(`###\\s+.*${cat}`));
    }
  });

  it('every severity in template exists in taxonomy', () => {
    for (const sev of ['critical', 'high', 'medium', 'low']) {
      expect(taxonomy.toLowerCase()).toContain(`**${sev}**`);
    }
  });
});
