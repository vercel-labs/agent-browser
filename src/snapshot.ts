/**
 * Enhanced snapshot with element refs for deterministic element selection.
 *
 * This module generates accessibility snapshots with embedded refs that can be
 * used to click/fill/interact with elements without re-querying the DOM.
 *
 * Example output:
 *   - heading "Example Domain" [ref=e1] [level=1]
 *   - paragraph: Some text content
 *   - button "Submit" [ref=e2]
 *   - textbox "Email" [ref=e3]
 *
 * Usage:
 *   agent-browser snapshot              # Full snapshot
 *   agent-browser snapshot -i           # Interactive elements only
 *   agent-browser snapshot --depth 3    # Limit depth
 *   agent-browser click @e2             # Click element by ref
 */

import type { Page, Locator } from 'playwright-core';

export interface RefMap {
  [ref: string]: {
    selector: string;
    role: string;
    name?: string;
    /** Index for disambiguation when multiple elements have same role+name */
    nth?: number;
  };
}

export interface EnhancedSnapshot {
  tree: string;
  refs: RefMap;
}

export interface SnapshotOptions {
  /** Only include interactive elements (buttons, links, inputs, etc.) */
  interactive?: boolean;
  /** Maximum depth of tree to include (0 = root only) */
  maxDepth?: number;
  /** Remove structural elements without meaningful content */
  compact?: boolean;
  /** CSS selector to scope the snapshot */
  selector?: string;
}

// Counter for generating refs
let refCounter = 0;

/**
 * Reset ref counter (call at start of each snapshot)
 */
export function resetRefs(): void {
  refCounter = 0;
}

/**
 * Generate next ref ID
 */
function nextRef(): string {
  return `e${++refCounter}`;
}

/**
 * Roles that are interactive and should get refs
 */
const INTERACTIVE_ROLES = new Set([
  'button',
  'link',
  'textbox',
  'checkbox',
  'radio',
  'combobox',
  'listbox',
  'menuitem',
  'menuitemcheckbox',
  'menuitemradio',
  'option',
  'searchbox',
  'slider',
  'spinbutton',
  'switch',
  'tab',
  'treeitem',
]);

/**
 * Roles that provide structure/context (get refs for text extraction)
 */
const CONTENT_ROLES = new Set([
  'heading',
  'cell',
  'gridcell',
  'columnheader',
  'rowheader',
  'listitem',
  'article',
  'region',
  'main',
  'navigation',
]);

/**
 * Roles that are purely structural (can be filtered in compact mode)
 */
const STRUCTURAL_ROLES = new Set([
  'generic',
  'group',
  'list',
  'table',
  'row',
  'rowgroup',
  'grid',
  'treegrid',
  'menu',
  'menubar',
  'toolbar',
  'tablist',
  'tree',
  'directory',
  'document',
  'application',
  'presentation',
  'none',
]);

/**
 * Build a selector string for storing in ref map
 */
function buildSelector(role: string, name?: string): string {
  if (name) {
    const escapedName = name.replace(/"/g, '\\"');
    return `getByRole('${role}', { name: "${escapedName}", exact: true })`;
  }
  return `getByRole('${role}')`;
}

/**
 * Get enhanced snapshot with refs and optional filtering
 */
export async function getEnhancedSnapshot(
  page: Page,
  options: SnapshotOptions = {}
): Promise<EnhancedSnapshot> {
  resetRefs();
  const refs: RefMap = {};

  // Get ARIA snapshot from Playwright
  const locator = options.selector ? page.locator(options.selector) : page.locator(':root');
  const ariaTree = await locator.ariaSnapshot();

  if (!ariaTree) {
    return {
      tree: '(empty)',
      refs: {},
    };
  }

  // Parse and enhance the ARIA tree
  const enhancedTree = processAriaTree(ariaTree, refs, options);

  return { tree: enhancedTree, refs };
}

/**
 * Track role+name combinations to detect duplicates
 */
interface RoleNameTracker {
  counts: Map<string, number>;
  /** Maps role+name key to array of ref IDs that use it */
  refsByKey: Map<string, string[]>;
  getKey(role: string, name?: string): string;
  getNextIndex(role: string, name?: string): number;
  trackRef(role: string, name: string | undefined, ref: string): void;
  /** Get all role+name keys that have duplicates */
  getDuplicateKeys(): Set<string>;
}

function createRoleNameTracker(): RoleNameTracker {
  const counts = new Map<string, number>();
  const refsByKey = new Map<string, string[]>();
  return {
    counts,
    refsByKey,
    getKey(role: string, name?: string): string {
      return `${role}:${name ?? ''}`;
    },
    getNextIndex(role: string, name?: string): number {
      const key = this.getKey(role, name);
      const current = counts.get(key) ?? 0;
      counts.set(key, current + 1);
      return current;
    },
    trackRef(role: string, name: string | undefined, ref: string): void {
      const key = this.getKey(role, name);
      const refs = refsByKey.get(key) ?? [];
      refs.push(ref);
      refsByKey.set(key, refs);
    },
    getDuplicateKeys(): Set<string> {
      const duplicates = new Set<string>();
      for (const [key, refs] of refsByKey) {
        if (refs.length > 1) {
          duplicates.add(key);
        }
      }
      return duplicates;
    },
  };
}

/**
 * Process ARIA snapshot: add refs and apply filters
 */
function processAriaTree(ariaTree: string, refs: RefMap, options: SnapshotOptions): string {
  const lines = ariaTree.split('\n');
  const result: string[] = [];
  const tracker = createRoleNameTracker();

  // For interactive-only mode, we collect just interactive elements
  if (options.interactive) {
    for (const line of lines) {
      const match = line.match(/^(\s*-\s*)(\w+)(?:\s+"([^"]*)")?(.*)$/);
      if (!match) continue;

      const [, , role, name, suffix] = match;
      const roleLower = role.toLowerCase();

      if (INTERACTIVE_ROLES.has(roleLower)) {
        const ref = nextRef();
        const nth = tracker.getNextIndex(roleLower, name);
        tracker.trackRef(roleLower, name, ref);
        refs[ref] = {
          selector: buildSelector(roleLower, name),
          role: roleLower,
          name,
          nth, // Always store nth, we'll use it for duplicates
        };

        let enhanced = `- ${role}`;
        if (name) enhanced += ` "${name}"`;
        enhanced += ` [ref=${ref}]`;
        // Only show nth in output if it's > 0 (for readability)
        if (nth > 0) enhanced += ` [nth=${nth}]`;
        if (suffix && suffix.includes('[')) enhanced += suffix;

        result.push(enhanced);
      }
    }

    // Post-process: remove nth from refs that don't have duplicates
    removeNthFromNonDuplicates(refs, tracker);

    return result.join('\n') || '(no interactive elements)';
  }

  // Normal processing with depth/compact filters
  for (const line of lines) {
    const processed = processLine(line, refs, options, tracker);
    if (processed !== null) {
      result.push(processed);
    }
  }

  // Post-process: remove nth from refs that don't have duplicates
  removeNthFromNonDuplicates(refs, tracker);

  // If compact mode, remove empty structural elements
  if (options.compact) {
    return compactTree(result.join('\n'));
  }

  return result.join('\n');
}

/**
 * Remove nth from refs that ended up not having duplicates
 * This keeps single-element locators simple (no unnecessary .nth(0))
 */
function removeNthFromNonDuplicates(refs: RefMap, tracker: RoleNameTracker): void {
  const duplicateKeys = tracker.getDuplicateKeys();

  for (const [ref, data] of Object.entries(refs)) {
    const key = tracker.getKey(data.role, data.name);
    if (!duplicateKeys.has(key)) {
      // Not a duplicate, remove nth to keep locator simple
      delete refs[ref].nth;
    }
  }
}

/**
 * Get indentation level (number of spaces / 2)
 */
function getIndentLevel(line: string): number {
  const match = line.match(/^(\s*)/);
  return match ? Math.floor(match[1].length / 2) : 0;
}

/**
 * Process a single line: add ref if needed, filter if requested
 */
function processLine(
  line: string,
  refs: RefMap,
  options: SnapshotOptions,
  tracker: RoleNameTracker
): string | null {
  const depth = getIndentLevel(line);

  // Check max depth
  if (options.maxDepth !== undefined && depth > options.maxDepth) {
    return null;
  }

  // Match lines like:
  //   - button "Submit"
  //   - heading "Title" [level=1]
  //   - link "Click me":
  const match = line.match(/^(\s*-\s*)(\w+)(?:\s+"([^"]*)")?(.*)$/);

  if (!match) {
    // Metadata lines (like /url:) or text content
    if (options.interactive) {
      // In interactive mode, only keep metadata under interactive elements
      return null;
    }
    return line;
  }

  const [, prefix, role, name, suffix] = match;
  const roleLower = role.toLowerCase();

  // Skip metadata lines (like /url:)
  if (role.startsWith('/')) {
    return line;
  }

  const isInteractive = INTERACTIVE_ROLES.has(roleLower);
  const isContent = CONTENT_ROLES.has(roleLower);
  const isStructural = STRUCTURAL_ROLES.has(roleLower);

  // In interactive-only mode, filter non-interactive elements
  if (options.interactive && !isInteractive) {
    return null;
  }

  // In compact mode, skip unnamed structural elements
  if (options.compact && isStructural && !name) {
    return null;
  }

  // Add ref for interactive or named content elements
  const shouldHaveRef = isInteractive || (isContent && name);

  if (shouldHaveRef) {
    const ref = nextRef();
    const nth = tracker.getNextIndex(roleLower, name);
    tracker.trackRef(roleLower, name, ref);

    refs[ref] = {
      selector: buildSelector(roleLower, name),
      role: roleLower,
      name,
      nth, // Always store nth, we'll clean up non-duplicates later
    };

    // Build enhanced line with ref
    let enhanced = `${prefix}${role}`;
    if (name) enhanced += ` "${name}"`;
    enhanced += ` [ref=${ref}]`;
    // Only show nth in output if it's > 0 (for readability)
    if (nth > 0) enhanced += ` [nth=${nth}]`;
    if (suffix) enhanced += suffix;

    return enhanced;
  }

  return line;
}

/**
 * Remove empty structural branches in compact mode
 */
function compactTree(tree: string): string {
  const lines = tree.split('\n');
  const result: string[] = [];

  // Simple pass: keep lines that have content or refs
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // Always keep lines with refs
    if (line.includes('[ref=')) {
      result.push(line);
      continue;
    }

    // Keep lines with text content (after :)
    if (line.includes(':') && !line.endsWith(':')) {
      result.push(line);
      continue;
    }

    // Check if this structural element has children with refs
    const currentIndent = getIndentLevel(line);
    let hasRelevantChildren = false;

    for (let j = i + 1; j < lines.length; j++) {
      const childIndent = getIndentLevel(lines[j]);
      if (childIndent <= currentIndent) break;
      if (lines[j].includes('[ref=')) {
        hasRelevantChildren = true;
        break;
      }
    }

    if (hasRelevantChildren) {
      result.push(line);
    }
  }

  return result.join('\n');
}

/**
 * Parse a ref from command argument (e.g., "@e1" -> "e1")
 */
export function parseRef(arg: string): string | null {
  if (arg.startsWith('@')) {
    return arg.slice(1);
  }
  if (arg.startsWith('ref=')) {
    return arg.slice(4);
  }
  if (/^e\d+$/.test(arg)) {
    return arg;
  }
  return null;
}

/**
 * Get snapshot statistics
 */
export function getSnapshotStats(
  tree: string,
  refs: RefMap
): {
  lines: number;
  chars: number;
  tokens: number;
  refs: number;
  interactive: number;
} {
  const interactive = Object.values(refs).filter((r) => INTERACTIVE_ROLES.has(r.role)).length;

  return {
    lines: tree.split('\n').length,
    chars: tree.length,
    tokens: Math.ceil(tree.length / 4),
    refs: Object.keys(refs).length,
    interactive,
  };
}
