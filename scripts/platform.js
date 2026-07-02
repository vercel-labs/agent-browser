import { execSync } from 'child_process';
import { existsSync } from 'fs';
import { arch as currentArch, platform as currentPlatform } from 'os';

export function isMusl(os = currentPlatform(), deps = {}) {
  if (os !== 'linux') return false;

  const exec = deps.execSync ?? execSync;
  const exists = deps.existsSync ?? existsSync;

  try {
    const result = exec('ldd --version 2>&1 || true', { encoding: 'utf8' });
    return result.toLowerCase().includes('musl');
  } catch {
    return exists('/lib/ld-musl-x86_64.so.1') || exists('/lib/ld-musl-aarch64.so.1');
  }
}

export function getOsKey(os = currentPlatform(), musl = isMusl(os)) {
  switch (os) {
    case 'darwin':
      return 'darwin';
    case 'linux':
      return musl ? 'linux-musl' : 'linux';
    case 'android':
      return 'linux';
    case 'win32':
      return 'win32';
    default:
      return null;
  }
}

export function getArchKey(cpuArch = currentArch(), options = {}) {
  switch (cpuArch) {
    case 'x64':
    case 'x86_64':
      return 'x64';
    case 'arm64':
    case 'aarch64':
      return options.winArm64Fallback ? 'x64' : 'arm64';
    default:
      return null;
  }
}

export function getPlatformKey(options = {}) {
  const os = options.platform ?? currentPlatform();
  const cpuArch = options.arch ?? currentArch();
  const osKey = getOsKey(os, options.isMusl ?? isMusl(os));
  const archKey = getArchKey(cpuArch, {
    winArm64Fallback: os === 'win32' && options.winArm64Fallback === true,
  });

  if (!osKey || !archKey) return null;
  return `${osKey}-${archKey}`;
}

export function getBinaryName(options = {}) {
  const os = options.platform ?? currentPlatform();
  const platformKey = getPlatformKey(options);
  if (!platformKey) return null;

  const ext = os === 'win32' ? '.exe' : '';
  return `agent-browser-${platformKey}${ext}`;
}

export function getExecutableCommand(binaryPath, args, os = currentPlatform()) {
  if (os === 'android') {
    return {
      executable: 'grun',
      args: [binaryPath, ...args],
    };
  }

  return {
    executable: binaryPath,
    args,
  };
}

export function shouldOptimizeGlobalBin(os = currentPlatform()) {
  return os !== 'android';
}
