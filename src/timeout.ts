// Default Playwright action timeout (milliseconds)
const DEFAULT_ACTION_TIMEOUT = 10000;

// Get action timeout from env var or use default
export function getActionTimeout(): number {
  const envTimeout = process.env.AGENT_BROWSER_ACTION_TIMEOUT;
  if (envTimeout) {
    const parsed = parseInt(envTimeout, 10);
    if (!isNaN(parsed) && parsed > 0) {
      return parsed;
    }
  }
  return DEFAULT_ACTION_TIMEOUT;
}
