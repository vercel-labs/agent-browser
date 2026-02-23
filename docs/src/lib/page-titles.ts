export const PAGE_TITLES: Record<string, string> = {
  "": "Headless Browser\nAutomation for AI",
  installation: "Installation",
  "quick-start": "Quick Start",
  commands: "Commands",
  configuration: "Configuration",
  selectors: "Selectors",
  snapshots: "Snapshots",
  sessions: "Sessions",
  diffing: "Diffing",
  "cdp-mode": "CDP Mode",
  streaming: "Streaming",
  profiler: "Profiler",
  ios: "iOS Simulator",
  changelog: "Changelog",
};

export function getPageTitle(slug: string): string | null {
  return slug in PAGE_TITLES ? PAGE_TITLES[slug]! : null;
}
