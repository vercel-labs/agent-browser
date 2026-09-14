import { defineConfig } from "wxt";

export default defineConfig({
  manifest: {
    name: "Agent Browser",
    version: "0.1.0",
    description: "Let agent-browser control a tab you explicitly approve.",
    minimum_chrome_version: "125",
    permissions: ["debugger", "tabs", "storage"],
    host_permissions: ["http://127.0.0.1/*"],
    action: { default_title: "Agent Browser: authorize or stop control" },
    content_security_policy: {
      extension_pages: "script-src 'self'; object-src 'self'; connect-src 'self' http://127.0.0.1:* ws://127.0.0.1:*",
    },
  },
});
