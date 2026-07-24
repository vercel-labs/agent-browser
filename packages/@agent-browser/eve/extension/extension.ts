import { defineExtension } from "eve/extension";
import { z } from "zod";

export default defineExtension({
  config: z.object({
    /** Domain allowlist passed to every command (wildcards like `*.example.com` supported). */
    allowedDomains: z.array(z.string()).optional(),
    /** Install agent-browser into the sandbox on first tool use when it is missing. */
    autoInstall: z.boolean().default(true),
    /** Binary name or path inside the sandbox. */
    binary: z.string().default("agent-browser"),
    /** Wrap page output in boundary markers so the model can tell tool output from page content. */
    contentBoundaries: z.boolean().default(false),
    /** Download Chromium during auto-install. */
    installBrowser: z.boolean().default(true),
    /** npm spec for auto-install, e.g. `agent-browser@0.31.2`. Defaults to the version matching this package. */
    installSpec: z.string().optional(),
    /** Install Chromium system libraries during auto-install. */
    installSystemDependencies: z.boolean().default(true),
    /** Embed captured screenshots in the tool output as data URLs so channels/UIs can render them inline (hidden from the model). */
    inlineScreenshots: z.boolean().default(true),
    /** Attach active browser-provider metadata to navigation channel output (hidden from the model). */
    includeProviderMetadata: z.boolean().default(false),
    /** Truncate page output to this many characters. */
    maxOutputChars: z.number().int().positive().optional(),
    /** Proxy server URL used by the browser. */
    proxy: z.string().optional(),
    /** Fixed agent-browser session name. Defaults to one derived from the eve sandbox id. */
    session: z.string().optional(),
    /** Prefix for the derived per-sandbox session name. */
    sessionPrefix: z.string().default("eve"),
  }),
});
