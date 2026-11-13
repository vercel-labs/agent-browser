import { defineConfig } from '@playwright/test';

// Generated artifacts are copied into `generated/` before they run, so they
// resolve `@playwright/test` from this package. Pinning `testDir` keeps
// Playwright away from the Node test files that sit next to this config.
export default defineConfig({
  testDir: 'generated',
});
