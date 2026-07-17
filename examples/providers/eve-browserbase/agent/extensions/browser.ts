import browser from "@agent-browser/eve";

// Mounts the agent-browser tool set under the `browser` namespace:
// browser__navigate, browser__snapshot, browser__click, browser__fill, ...
export default browser({
  // The Vercel Sandbox template pre-installs the matching agent-browser CLI.
  // Browserbase supplies the browser, so Chromium and its Linux libraries are
  // intentionally not installed in the sandbox.
  autoInstall: false,
  installBrowser: false,
  installSystemDependencies: false,
  contentBoundaries: true,
  includeProviderMetadata: true,
  inlineScreenshots: true,
  maxOutputChars: 50_000,

  // Add an allowlist when adapting this general-purpose template to a fixed
  // set of sites.
  // allowedDomains: ["example.com", "*.example.com"],
});
