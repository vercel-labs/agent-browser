import { agentBrowserRevalidationKey, installAgentBrowser } from "@agent-browser/eve/sandbox";
import { defineSandbox } from "eve/sandbox";
import { vercel } from "eve/sandbox/vercel";

const installOptions = {
  installBrowser: false,
  installSystemDependencies: false,
} as const;

const BROKERED_API_KEY_SENTINEL = "brokered-by-vercel-firewall";

export default defineSandbox({
  // Resolve the secret lazily: Vercel environment variables are guaranteed at
  // runtime/prewarm time, not while the authored module is being compiled.
  backend: () => {
    const apiKey = process.env.BROWSERBASE_API_KEY?.trim();
    if (!apiKey) {
      throw new Error(
        "BROWSERBASE_API_KEY is required. Add it to .env.local for development and to the Vercel project environment for deployments.",
      );
    }

    return vercel({
      resources: { vcpus: 2 },
      env: {
        AGENT_BROWSER_PROVIDER: "browserbase",
        // agent-browser checks for this variable before making its API call.
        // The real value is injected into the outbound header by the firewall.
        BROWSERBASE_API_KEY: BROKERED_API_KEY_SENTINEL,
      },
      networkPolicy: {
        allow: {
          "api.browserbase.com": [
            {
              transform: [
                {
                  headers: {
                    "x-bb-api-key": apiKey,
                  },
                },
              ],
            },
          ],
          // Browser navigation remains unrestricted in this starter template.
          "*": [],
        },
      },
    });
  },
  revalidationKey: () => agentBrowserRevalidationKey(installOptions),
  async bootstrap({ use }) {
    const sandbox = await use();
    await installAgentBrowser(sandbox, installOptions);
  },
});
