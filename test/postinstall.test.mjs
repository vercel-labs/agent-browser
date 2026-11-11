import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import test from 'node:test';

const run = promisify(execFile);
const postinstallPath = fileURLToPath(new URL('../scripts/postinstall.js', import.meta.url));

for (const failure of ['HTTP 404', 'an interrupted response', 'an empty response']) {
  test(`postinstall can retry after ${failure}`, async (t) => {
    const packageRoot = await mkdtemp(join(tmpdir(), 'agent-browser-postinstall-'));
    t.after(() => rm(packageRoot, { recursive: true, force: true }));
    const binDirectory = join(packageRoot, 'bin');
    const binary = join(binDirectory, 'agent-browser-win32-x64.exe');
    const script = join(packageRoot, 'scripts', 'postinstall.js');
    const bootstrap = join(packageRoot, 'bootstrap.mjs');
    await mkdir(dirname(script), { recursive: true });
    await copyFile(postinstallPath, script);
    for (const name of ['native.js', 'link-native.js']) {
      await copyFile(join(dirname(postinstallPath), name), join(dirname(script), name));
    }
    await writeFile(join(packageRoot, 'package.json'), '{"type":"module","version":"0.0.0"}');
    await writeFile(bootstrap, `
import { createRequire, syncBuiltinESMExports } from 'node:module';
const require = createRequire(import.meta.url);
const os = require('node:os');
os.platform = () => 'win32';
os.arch = () => 'x64';
require('node:https').get = (url, callback) => {
  const requested = new URL(url);
  const fixture = new URL(process.argv[2]);
  return require('node:http').get(requested.origin === fixture.origin ? requested : fixture, callback);
};
require('node:child_process').execSync = () => process.cwd();
syncBuiltinESMExports();
await import('./scripts/postinstall.js');
`);

    let attempts = 0;
    const server = createServer((request, response) => {
      if (request.url === '/complete') {
        response.end('complete binary');
      } else if (++attempts > 1) {
        response.writeHead(302, { Location: `http://${request.headers.host}/complete` }).end();
      } else if (failure === 'HTTP 404') {
        response.writeHead(404, { Connection: 'close' }).end('missing');
      } else if (failure === 'an interrupted response') {
        response.writeHead(200, { 'Content-Length': 100 });
        response.write('partial', () => response.destroy());
      } else {
        response.writeHead(200, { Connection: 'close' }).end();
      }
    });
    t.after(() => server.close());
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    const install = () => run(process.execPath, [
      bootstrap, `http://127.0.0.1:${server.address().port}/binary`,
    ], { cwd: packageRoot, timeout: 5000 });
    await install();
    const files = await readdir(binDirectory);
    assert.deepEqual(files.filter((name) => name !== '.install-method'), []);
    await install();
    assert.equal(attempts, 2);
    assert.equal(await readFile(binary, 'utf8'), 'complete binary');
  });
}
