/**
 * Create a Vercel Sandbox snapshot with agent-browser + Chromium pre-installed.
 *
 * Run once:   npx tsx scripts/create-snapshot.ts
 * Then set:   AGENT_BROWSER_SNAPSHOT_ID=<output id>
 *
 * Authentication (one of):
 *   - VERCEL_TOKEN + VERCEL_TEAM_ID + VERCEL_PROJECT_ID
 *   - VERCEL_OIDC_TOKEN (automatically available on Vercel deployments)
 *
 * This makes sandbox creation sub-second instead of ~30s.
 */

import "dotenv/config";
import { createSnapshot, getSandboxCredentials } from "../lib/agent-browser-sandbox";

const hasExplicitCreds = !!(
  process.env.VERCEL_TOKEN &&
  process.env.VERCEL_TEAM_ID &&
  process.env.VERCEL_PROJECT_ID
);
const hasOidc = !!process.env.VERCEL_OIDC_TOKEN;

if (!hasExplicitCreds && !hasOidc) {
  console.error(
    "Missing sandbox credentials. Provide either:\n" +
      "  1. VERCEL_TOKEN + VERCEL_TEAM_ID + VERCEL_PROJECT_ID\n" +
      "  2. VERCEL_OIDC_TOKEN",
  );
  process.exit(1);
}

const creds = getSandboxCredentials();
console.log(
  creds.token
    ? `Authenticating with explicit credentials (team: ${creds.teamId})`
    : "Authenticating via VERCEL_OIDC_TOKEN",
);

async function main() {
  console.log("Creating Vercel Sandbox with agent-browser + Chromium...");
  console.log("This takes ~30-60 seconds on first run.\n");

  const snapshotId = await createSnapshot();

  console.log("\nSnapshot created successfully!");
  console.log(`\n  AGENT_BROWSER_SNAPSHOT_ID=${snapshotId}\n`);
  console.log("Add this to your .env.local or Vercel environment variables.");
}

main().catch((err) => {
  console.error("Failed to create snapshot:", err.message || err);
  process.exit(1);
});
