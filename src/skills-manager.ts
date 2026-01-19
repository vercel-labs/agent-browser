/**
 * Skills Management System for Agent Browser Worker
 * Manages loading, registering, and executing skills/plugins
 */

export interface Skill {
  id: string;
  name: string;
  version: string;
  description: string;
  enabled: boolean;
  execute: (params: Record<string, unknown>) => Promise<unknown>;
}

export interface Plugin {
  id: string;
  name: string;
  version: string;
  description: string;
  enabled: boolean;
  skills: Skill[];
  initialize?: () => Promise<void>;
  destroy?: () => Promise<void>;
}

export interface SkillRegistry {
  [skillId: string]: Skill;
}

export interface PluginRegistry {
  [pluginId: string]: Plugin;
}

/**
 * Skills Manager - manages skills and plugins lifecycle
 */
export class SkillsManager {
  private skills: SkillRegistry = {};
  private plugins: PluginRegistry = {};
  private initialized: Set<string> = new Set();

  /**
   * Register a skill
   */
  registerSkill(skill: Skill): void {
    if (this.skills[skill.id]) {
      console.warn(`Skill ${skill.id} already registered, overwriting`);
    }
    this.skills[skill.id] = skill;
  }

  /**
   * Unregister a skill
   */
  unregisterSkill(skillId: string): boolean {
    if (this.skills[skillId]) {
      delete this.skills[skillId];
      return true;
    }
    return false;
  }

  /**
   * Register a plugin
   */
  async registerPlugin(plugin: Plugin): Promise<void> {
    if (this.plugins[plugin.id]) {
      console.warn(`Plugin ${plugin.id} already registered, overwriting`);
    }

    this.plugins[plugin.id] = plugin;

    // Register all skills from the plugin
    for (const skill of plugin.skills) {
      this.registerSkill(skill);
    }

    // Initialize plugin if it has initialization
    if (plugin.initialize && !this.initialized.has(plugin.id)) {
      await plugin.initialize();
      this.initialized.add(plugin.id);
    }
  }

  /**
   * Unregister a plugin
   */
  async unregisterPlugin(pluginId: string): Promise<boolean> {
    const plugin = this.plugins[pluginId];
    if (!plugin) {
      return false;
    }

    // Destroy plugin if it has cleanup
    if (plugin.destroy && this.initialized.has(pluginId)) {
      await plugin.destroy();
      this.initialized.delete(pluginId);
    }

    // Unregister all skills from the plugin
    for (const skill of plugin.skills) {
      this.unregisterSkill(skill.id);
    }

    delete this.plugins[pluginId];
    return true;
  }

  /**
   * Execute a skill
   */
  async executeSkill(skillId: string, params: Record<string, unknown>): Promise<unknown> {
    const skill = this.skills[skillId];

    if (!skill) {
      throw new Error(`Skill ${skillId} not found`);
    }

    if (!skill.enabled) {
      throw new Error(`Skill ${skillId} is disabled`);
    }

    return skill.execute(params);
  }

  /**
   * Get skill by ID
   */
  getSkill(skillId: string): Skill | undefined {
    return this.skills[skillId];
  }

  /**
   * Get all skills
   */
  getAllSkills(): Skill[] {
    return Object.values(this.skills);
  }

  /**
   * Get enabled skills
   */
  getEnabledSkills(): Skill[] {
    return Object.values(this.skills).filter((s) => s.enabled);
  }

  /**
   * Enable skill
   */
  enableSkill(skillId: string): boolean {
    const skill = this.skills[skillId];
    if (skill) {
      skill.enabled = true;
      return true;
    }
    return false;
  }

  /**
   * Disable skill
   */
  disableSkill(skillId: string): boolean {
    const skill = this.skills[skillId];
    if (skill) {
      skill.enabled = false;
      return true;
    }
    return false;
  }

  /**
   * Get plugin by ID
   */
  getPlugin(pluginId: string): Plugin | undefined {
    return this.plugins[pluginId];
  }

  /**
   * Get all plugins
   */
  getAllPlugins(): Plugin[] {
    return Object.values(this.plugins);
  }

  /**
   * Get enabled plugins
   */
  getEnabledPlugins(): Plugin[] {
    return Object.values(this.plugins).filter((p) => p.enabled);
  }

  /**
   * Enable plugin
   */
  enablePlugin(pluginId: string): boolean {
    const plugin = this.plugins[pluginId];
    if (plugin) {
      plugin.enabled = true;
      // Enable all its skills
      for (const skill of plugin.skills) {
        this.enableSkill(skill.id);
      }
      return true;
    }
    return false;
  }

  /**
   * Disable plugin
   */
  disablePlugin(pluginId: string): boolean {
    const plugin = this.plugins[pluginId];
    if (plugin) {
      plugin.enabled = false;
      // Disable all its skills
      for (const skill of plugin.skills) {
        this.disableSkill(skill.id);
      }
      return true;
    }
    return false;
  }

  /**
   * Get skills summary
   */
  getSkillsSummary(): Array<{
    id: string;
    name: string;
    version: string;
    description: string;
    enabled: boolean;
    plugin?: string;
  }> {
    const summary: Array<{
      id: string;
      name: string;
      version: string;
      description: string;
      enabled: boolean;
      plugin?: string;
    }> = [];

    for (const [pluginId, plugin] of Object.entries(this.plugins)) {
      for (const skill of plugin.skills) {
        summary.push({
          id: skill.id,
          name: skill.name,
          version: skill.version,
          description: skill.description,
          enabled: skill.enabled,
          plugin: pluginId,
        });
      }
    }

    return summary;
  }

  /**
   * Get plugins summary
   */
  getPluginsSummary(): Array<{
    id: string;
    name: string;
    version: string;
    description: string;
    enabled: boolean;
    skillCount: number;
  }> {
    return Object.entries(this.plugins).map(([, plugin]) => ({
      id: plugin.id,
      name: plugin.name,
      version: plugin.version,
      description: plugin.description,
      enabled: plugin.enabled,
      skillCount: plugin.skills.length,
    }));
  }
}

/**
 * Built-in plugins for core functionality
 */

/**
 * Create screenshot skill plugin
 */
export function createScreenshotPlugin(browserManager: any): Plugin {
  return {
    id: 'screenshot',
    name: 'Screenshot Plugin',
    version: '1.0.0',
    description: 'Take screenshots of the browser viewport',
    enabled: true,
    skills: [
      {
        id: 'take-screenshot',
        name: 'Take Screenshot',
        version: '1.0.0',
        description: 'Capture a screenshot of the current page',
        enabled: true,
        execute: async (params) => {
          const path = params.path as string || 'screenshot.png';
          const fullPage = params.fullPage as boolean || false;
          return await browserManager.takeScreenshot(path, { fullPage });
        },
      },
    ],
  };
}

/**
 * Create PDF export skill plugin
 */
export function createPdfPlugin(browserManager: any): Plugin {
  return {
    id: 'pdf',
    name: 'PDF Export Plugin',
    version: '1.0.0',
    description: 'Export pages as PDF documents',
    enabled: true,
    skills: [
      {
        id: 'export-pdf',
        name: 'Export to PDF',
        version: '1.0.0',
        description: 'Convert current page to PDF',
        enabled: true,
        execute: async (params) => {
          const path = params.path as string;
          const format = params.format as string || 'A4';
          return await browserManager.pdf(path, { format });
        },
      },
    ],
  };
}

/**
 * Create content extraction skill plugin
 */
export function createContentPlugin(): Plugin {
  return {
    id: 'content',
    name: 'Content Extraction Plugin',
    version: '1.0.0',
    description: 'Extract content from the page',
    enabled: true,
    skills: [
      {
        id: 'extract-text',
        name: 'Extract Text',
        version: '1.0.0',
        description: 'Extract all text content from the page',
        enabled: true,
        execute: async (params) => {
          // This would be implemented with actual text extraction logic
          return { text: 'Page content' };
        },
      },
      {
        id: 'extract-html',
        name: 'Extract HTML',
        version: '1.0.0',
        description: 'Extract HTML structure of the page',
        enabled: true,
        execute: async (params) => {
          // This would be implemented with actual HTML extraction logic
          return { html: '<html></html>' };
        },
      },
    ],
  };
}
