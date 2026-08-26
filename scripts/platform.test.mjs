import assert from 'node:assert/strict';
import test from 'node:test';

import {
  getBinaryName,
  getExecutableCommand,
  getPlatformKey,
  shouldOptimizeGlobalBin,
} from './platform.js';

test('maps Android to the Linux binary artifact', () => {
  assert.equal(
    getBinaryName({ platform: 'android', arch: 'arm64', isMusl: false }),
    'agent-browser-linux-arm64'
  );
  assert.equal(
    getBinaryName({ platform: 'android', arch: 'arm64', isMusl: false, winArm64Fallback: true }),
    'agent-browser-linux-arm64'
  );
  assert.equal(
    getPlatformKey({ platform: 'android', arch: 'aarch64', isMusl: false }),
    'linux-arm64'
  );
});

test('runs Android binaries through grun', () => {
  assert.deepEqual(getExecutableCommand('/pkg/bin/agent-browser-linux-arm64', ['--version'], 'android'), {
    executable: 'grun',
    args: ['/pkg/bin/agent-browser-linux-arm64', '--version'],
  });
});

test('keeps the JavaScript wrapper active on Android global installs', () => {
  assert.equal(shouldOptimizeGlobalBin('android'), false);
});

test('preserves existing Linux and macOS naming', () => {
  assert.equal(
    getBinaryName({ platform: 'linux', arch: 'x64', isMusl: false }),
    'agent-browser-linux-x64'
  );
  assert.equal(
    getBinaryName({ platform: 'linux', arch: 'arm64', isMusl: true }),
    'agent-browser-linux-musl-arm64'
  );
  assert.equal(
    getBinaryName({ platform: 'darwin', arch: 'arm64', isMusl: false }),
    'agent-browser-darwin-arm64'
  );
});

test('preserves Windows ARM64 postinstall fallback behavior when requested', () => {
  assert.equal(
    getBinaryName({ platform: 'win32', arch: 'arm64', isMusl: false, winArm64Fallback: true }),
    'agent-browser-win32-x64.exe'
  );
  assert.equal(
    getBinaryName({ platform: 'win32', arch: 'arm64', isMusl: false }),
    'agent-browser-win32-arm64.exe'
  );
});

test('returns null for unsupported platforms and architectures', () => {
  assert.equal(getBinaryName({ platform: 'freebsd', arch: 'x64', isMusl: false }), null);
  assert.equal(getBinaryName({ platform: 'linux', arch: 'riscv64', isMusl: false }), null);
});
