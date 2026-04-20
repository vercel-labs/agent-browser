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

// Convert the npm-style version (which may contain multiple `-` separators
// like `0.26.0-celeria-stealth.2`) into a PEP 440-compliant local version
// identifier (`0.26.0+celeria.stealth.2`). PEP 440 does not allow hyphens
// inside the public release segment, but `+` followed by dot-separated
// alphanumerics is valid and preserves our pre-release labeling intent.
function toPep440(npmVersion) {
  const dashIdx = npmVersion.indexOf("-");
  if (dashIdx === -1) return npmVersion;
  const release = npmVersion.slice(0, dashIdx);
  const local = npmVersion.slice(dashIdx + 1).replace(/-/g, ".");
  return `${release}+${local}`;
}

const pep440Version = toPep440(version);

// Update packages/camoufox-sidecar/pyproject.toml
const sidecarPyprojectPath = join(
  rootDir,
  "packages",
  "camoufox-sidecar",
  "pyproject.toml"
);
let sidecarPyproject = readFileSync(sidecarPyprojectPath, "utf-8");
const sidecarVersionRegex = /^version\s*=\s*"[^"]*"/m;
const newSidecarVersion = `version = "${pep440Version}"`;
if (sidecarVersionRegex.test(sidecarPyproject)) {
  const oldMatch = sidecarPyproject.match(sidecarVersionRegex)?.[0];
  if (oldMatch !== newSidecarVersion) {
    sidecarPyproject = sidecarPyproject.replace(
      sidecarVersionRegex,
      newSidecarVersion
    );
    writeFileSync(sidecarPyprojectPath, sidecarPyproject);
    console.log(
      `  Updated packages/camoufox-sidecar/pyproject.toml: ${oldMatch} -> ${newSidecarVersion}`
    );
  } else {
    console.log(`  packages/camoufox-sidecar/pyproject.toml already up to date`);
  }
} else {
  console.error(
    "  Could not find version field in packages/camoufox-sidecar/pyproject.toml"
  );
  process.exit(1);
}

// Update packages/camoufox-sidecar/camoufox_sidecar/__init__.py
const sidecarInitPath = join(
  rootDir,
  "packages",
  "camoufox-sidecar",
  "camoufox_sidecar",
  "__init__.py"
);
let sidecarInit = readFileSync(sidecarInitPath, "utf-8");
const sidecarInitRegex = /^__version__\s*=\s*"[^"]*"/m;
const newSidecarInit = `__version__ = "${pep440Version}"`;
if (sidecarInitRegex.test(sidecarInit)) {
  const oldMatch = sidecarInit.match(sidecarInitRegex)?.[0];
  if (oldMatch !== newSidecarInit) {
    sidecarInit = sidecarInit.replace(sidecarInitRegex, newSidecarInit);
    writeFileSync(sidecarInitPath, sidecarInit);
    console.log(
      `  Updated packages/camoufox-sidecar/camoufox_sidecar/__init__.py: ${oldMatch} -> ${newSidecarInit}`
    );
  } else {
    console.log(
      `  packages/camoufox-sidecar/camoufox_sidecar/__init__.py already up to date`
    );
  }
} else {
  console.error(
    "  Could not find __version__ in packages/camoufox-sidecar/camoufox_sidecar/__init__.py"
  );
  process.exit(1);
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
