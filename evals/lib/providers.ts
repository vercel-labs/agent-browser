import type { Provider, ProviderName } from "./types.ts";
import { claudeProvider } from "./claude.ts";
import { codexProvider } from "./codex.ts";

const providers: Record<ProviderName, Provider> = {
  claude: claudeProvider,
  codex: codexProvider,
};

export function getProvider(name: ProviderName): Provider {
  const provider = providers[name];
  if (!provider) {
    throw new Error(`Unknown provider: ${name}. Use "claude" or "codex".`);
  }
  return provider;
}
