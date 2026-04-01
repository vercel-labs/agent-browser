#!/usr/bin/env node

/**
 * Syncs the version from package.json to all other config files.
 * Run this script before building or releasing.
 */

import { execSync } from "child_process";
import { readFileSync, writeFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, "..");
const cliDir = join(rootDir, "cli");

// Read version from package.json (single source of truth)
const packageJson = JSON.parse(
  readFileSync(join(rootDir, "package.json"), "utf-8")
);
const version = packageJson.version;

console.log(`Syncing version ${version} to all config files...`);

// Update Cargo.toml
const cargoTomlPath = join(cliDir, "Cargo.toml");
let cargoToml = readFileSync(cargoTomlPath, "utf-8");
const cargoVersionRegex = /^version\s*=\s*"[^"]*"/m;
const newCargoVersion = `version = "${version}"`;

let cargoTomlUpdated = false;
if (cargoVersionRegex.test(cargoToml)) {
  const oldMatch = cargoToml.match(cargoVersionRegex)?.[0];
  if (oldMatch !== newCargoVersion) {
    cargoToml = cargoToml.replace(cargoVersionRegex, newCargoVersion);
    writeFileSync(cargoTomlPath, cargoToml);
    console.log(`  Updated cli/Cargo.toml: ${oldMatch} -> ${newCargoVersion}`);
    cargoTomlUpdated = true;
  } else {
    console.log(`  cli/Cargo.toml already up to date`);
  }
} else {
  console.error("  Could not find version field in cli/Cargo.toml");
  process.exit(1);
}

// Update packages/dashboard/package.json
const dashboardPkgPath = join(rootDir, "packages", "dashboard", "package.json");
const dashboardPkg = JSON.parse(readFileSync(dashboardPkgPath, "utf-8"));
if (dashboardPkg.version !== version) {
  const oldVersion = dashboardPkg.version;
  dashboardPkg.version = version;
  writeFileSync(dashboardPkgPath, JSON.stringify(dashboardPkg, null, 2) + "\n");
  console.log(`  Updated packages/dashboard/package.json: ${oldVersion} -> ${version}`);
} else {
  console.log(`  packages/dashboard/package.json already up to date`);
}

// Update Cargo.lock to match Cargo.toml
if (cargoTomlUpdated) {
  try {
    execSync("cargo update -p agent-browser --offline", {
      cwd: cliDir,
      stdio: "pipe",
    });
    console.log(`  Updated cli/Cargo.lock`);
  } catch {
    // --offline may fail if package not in cache, try without it
    try {
      execSync("cargo update -p agent-browser", {
        cwd: cliDir,
        stdio: "pipe",
      });
      console.log(`  Updated cli/Cargo.lock`);
    } catch (e) {
      console.error(`  Warning: Could not update Cargo.lock: ${e.message}`);
    }
  }
}

console.log("Version sync complete.");
