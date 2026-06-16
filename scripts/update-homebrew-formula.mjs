#!/usr/bin/env node

import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { get } from "node:https";

const ASSETS = [
  {
    key: "darwinArm64",
    name: "agent-browser-priv-darwin-arm64",
  },
  {
    key: "linuxArm64",
    name: "agent-browser-priv-linux-arm64",
  },
  {
    key: "linuxX64",
    name: "agent-browser-priv-linux-x64",
  },
];

function argValue(name) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? undefined : process.argv[index + 1];
}

const version = argValue("version");
const formulaPath = argValue("formula");

if (!version || !formulaPath) {
  console.error("Usage: node scripts/update-homebrew-formula.mjs --version <version> --formula <path>");
  process.exit(1);
}

function assetUrl(name) {
  return `https://github.com/liuwen/agent-browser-priv/releases/download/v${version}/${name}`;
}

function fetchBuffer(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    get(url, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        if (!response.headers.location || redirects > 5) {
          reject(new Error(`Too many redirects for ${url}`));
          response.resume();
          return;
        }
        response.resume();
        resolve(fetchBuffer(response.headers.location, redirects + 1));
        return;
      }

      if (response.statusCode !== 200) {
        reject(new Error(`GET ${url} returned HTTP ${response.statusCode}`));
        response.resume();
        return;
      }

      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve(Buffer.concat(chunks)));
    }).on("error", reject);
  });
}

async function withRetries(label, fn) {
  let lastError;
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      const delayMs = attempt * 2000;
      console.error(`${label} failed on attempt ${attempt}: ${error.message}`);
      if (attempt < 5) {
        await new Promise((resolve) => setTimeout(resolve, delayMs));
      }
    }
  }
  throw lastError;
}

function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

function renderFormula(checksums) {
  return `class AgentBrowser < Formula
  desc "Browser automation CLI for AI agents with Patchright as the default backend"
  homepage "https://github.com/liuwen/agent-browser-priv"
  version "${version}"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "${assetUrl("agent-browser-priv-darwin-arm64")}"
      sha256 "${checksums.darwinArm64}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${assetUrl("agent-browser-priv-linux-arm64")}"
      sha256 "${checksums.linuxArm64}"
    elsif Hardware::CPU.intel?
      url "${assetUrl("agent-browser-priv-linux-x64")}"
      sha256 "${checksums.linuxX64}"
    end
  end

  def install
    unsupported = "agent-browser Homebrew binary is published for macOS ARM64 and Linux x86_64/ARM64"
    odie unsupported unless supported_platform?

    binary = if OS.mac? && Hardware::CPU.arm?
      "agent-browser-priv-darwin-arm64"
    elsif OS.linux? && Hardware::CPU.arm?
      "agent-browser-priv-linux-arm64"
    elsif OS.linux? && Hardware::CPU.intel?
      "agent-browser-priv-linux-x64"
    else
      odie unsupported
    end

    bin.install binary => "agent-browser"
    bin.install_symlink bin/"agent-browser" => "agent-browser-priv"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/agent-browser --version")
    assert_match version.to_s, shell_output("#{bin}/agent-browser-priv --version")
  end

  def supported_platform?
    (OS.mac? && Hardware::CPU.arm?) || (OS.linux? && (Hardware::CPU.arm? || Hardware::CPU.intel?))
  end
end
`;
}

const checksums = {};
for (const asset of ASSETS) {
  const url = assetUrl(asset.name);
  const data = await withRetries(asset.name, () => fetchBuffer(url));
  if (data.length < 100_000) {
    throw new Error(`${asset.name} is too small (${data.length} bytes)`);
  }
  checksums[asset.key] = sha256(data);
  console.log(`${asset.name}: ${checksums[asset.key]}`);
}

writeFileSync(formulaPath, renderFormula(checksums));
console.log(`Updated ${formulaPath}`);
