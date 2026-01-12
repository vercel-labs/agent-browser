#!/usr/bin/env node

/**
 * Syncs the version from package.json to all other config files.
 * Run this script before building or releasing.
 */

import { readFileSync, writeFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, "..");

// Read version from package.json (single source of truth)
const packageJson = JSON.parse(
  readFileSync(join(rootDir, "package.json"), "utf-8")
);
const version = packageJson.version;

console.log(`Syncing version ${version} to all config files...`);

// Update Cargo.toml
const cargoTomlPath = join(rootDir, "cli/Cargo.toml");
let cargoToml = readFileSync(cargoTomlPath, "utf-8");
const cargoVersionRegex = /^version\s*=\s*"[^"]*"/m;
const newCargoVersion = `version = "${version}"`;

if (cargoVersionRegex.test(cargoToml)) {
  const oldMatch = cargoToml.match(cargoVersionRegex)?.[0];
  if (oldMatch !== newCargoVersion) {
    cargoToml = cargoToml.replace(cargoVersionRegex, newCargoVersion);
    writeFileSync(cargoTomlPath, cargoToml);
    console.log(`  Updated cli/Cargo.toml: ${oldMatch} -> ${newCargoVersion}`);
  } else {
    console.log(`  cli/Cargo.toml already up to date`);
  }
} else {
  console.error("  Could not find version field in cli/Cargo.toml");
  process.exit(1);
}

console.log("Version sync complete.");
