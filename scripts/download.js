import { createWriteStream } from 'node:fs';
import { chmod, rename, rm, stat } from 'node:fs/promises';
import { get as httpsGet } from 'node:https';
import { get as httpGet } from 'node:http';
import { extname } from 'node:path';
import { randomUUID } from 'node:crypto';
import { pipeline } from 'node:stream/promises';

/**
 * Download to a unique sibling file and publish only after validation.
 * One absolute deadline covers every redirect and the entire response body.
 */
export async function downloadFile(
  source,
  destination,
  { timeoutMs = 120_000, idleTimeoutMs = 15_000, maxRedirects = 5, validate = async () => {} } = {}
) {
  const temporary = `${destination}.${randomUUID()}.download${extname(destination)}`;
  const controller = new AbortController();
  const timer = setTimeout(
    () => controller.abort(new Error('Native binary download timed out')),
    timeoutMs
  );
  timer.unref();

  function request(url, redirects = 0) {
    return new Promise((resolve, reject) => {
      if (!['http:', 'https:'].includes(url.protocol)) {
        reject(new Error('Unsupported download protocol'));
        return;
      }
      const get = url.protocol === 'https:' ? httpsGet : httpGet;
      const req = get(url, { signal: controller.signal }, (response) => {
        response.on('error', reject);
        if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
          response.destroy();
          if (!response.headers.location || redirects >= maxRedirects) {
            reject(new Error('Invalid or excessive download redirects'));
            return;
          }
          try {
            const next = new URL(response.headers.location, url);
            if (url.protocol === 'https:' && next.protocol !== 'https:') {
              reject(new Error('Refusing an insecure download redirect'));
              return;
            }
            resolve(request(next, redirects + 1));
          } catch (error) {
            reject(error);
          }
          return;
        }
        if (response.statusCode !== 200) {
          response.destroy();
          reject(new Error(`Failed to download: HTTP ${response.statusCode}`));
          return;
        }
        resolve(response);
      });
      req.setTimeout(idleTimeoutMs, () => req.destroy(new Error('Native binary download stalled')));
      req.on('error', reject);
    });
  }

  try {
    const response = await request(new URL(source));
    await pipeline(response, createWriteStream(temporary, { flags: 'wx', mode: 0o755 }), {
      signal: controller.signal,
    });
    if ((await stat(temporary)).size === 0) {
      throw new Error('Downloaded native binary is empty');
    }
    await chmod(temporary, 0o755);
    await validate(temporary);
    controller.signal.throwIfAborted();
    await rename(temporary, destination);
  } finally {
    clearTimeout(timer);
    controller.abort();
    await rm(temporary, { force: true }).catch(() => {});
  }
}
