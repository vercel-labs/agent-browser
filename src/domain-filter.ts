import type { BrowserContext, Route } from 'playwright-core';

/**
 * Checks whether a hostname matches one of the allowed domain patterns.
 * Patterns support exact match ("example.com") and wildcard prefix ("*.example.com").
 */
export function isDomainAllowed(hostname: string, allowedDomains: string[]): boolean {
  for (const pattern of allowedDomains) {
    if (pattern.startsWith('*.')) {
      const suffix = pattern.slice(1); // ".example.com"
      if (hostname === pattern.slice(2) || hostname.endsWith(suffix)) {
        return true;
      }
    } else if (hostname === pattern) {
      return true;
    }
  }
  return false;
}

export function parseDomainList(raw: string): string[] {
  return raw
    .split(',')
    .map((d) => d.trim().toLowerCase())
    .filter((d) => d.length > 0);
}

/**
 * Build the init script source that monkey-patches WebSocket, EventSource,
 * and navigator.sendBeacon to block connections to non-allowed domains.
 * Exported for testing.
 */
export function buildWebSocketFilterScript(allowedDomains: string[]): string {
  const serialized = JSON.stringify(allowedDomains);
  return `(function() {
  var _allowedDomains = ${serialized};
  function _isDomainAllowed(hostname) {
    hostname = hostname.toLowerCase();
    for (var i = 0; i < _allowedDomains.length; i++) {
      var pattern = _allowedDomains[i];
      if (pattern.indexOf('*.') === 0) {
        var suffix = pattern.slice(1);
        if (hostname === pattern.slice(2) || hostname.slice(-suffix.length) === suffix) {
          return true;
        }
      } else if (hostname === pattern) {
        return true;
      }
    }
    return false;
  }
  function _checkUrl(url) {
    try {
      var parsed = new URL(url);
      return _isDomainAllowed(parsed.hostname);
    } catch(e) {
      return false;
    }
  }
  if (typeof WebSocket !== 'undefined') {
    var _OrigWS = WebSocket;
    WebSocket = function(url, protocols) {
      if (!_checkUrl(url)) {
        throw new DOMException(
          'WebSocket connection to ' + url + ' blocked by domain allowlist',
          'SecurityError'
        );
      }
      if (protocols !== undefined) {
        return new _OrigWS(url, protocols);
      }
      return new _OrigWS(url);
    };
    WebSocket.prototype = _OrigWS.prototype;
    WebSocket.CONNECTING = _OrigWS.CONNECTING;
    WebSocket.OPEN = _OrigWS.OPEN;
    WebSocket.CLOSING = _OrigWS.CLOSING;
    WebSocket.CLOSED = _OrigWS.CLOSED;
  }
  if (typeof EventSource !== 'undefined') {
    var _OrigES = EventSource;
    EventSource = function(url, opts) {
      if (!_checkUrl(url)) {
        throw new DOMException(
          'EventSource connection to ' + url + ' blocked by domain allowlist',
          'SecurityError'
        );
      }
      return new _OrigES(url, opts);
    };
    EventSource.prototype = _OrigES.prototype;
    EventSource.CONNECTING = _OrigES.CONNECTING;
    EventSource.OPEN = _OrigES.OPEN;
    EventSource.CLOSED = _OrigES.CLOSED;
  }
  if (typeof navigator !== 'undefined' && typeof navigator.sendBeacon === 'function') {
    var _origSendBeacon = navigator.sendBeacon.bind(navigator);
    navigator.sendBeacon = function(url, data) {
      if (!_checkUrl(url)) {
        return false;
      }
      return _origSendBeacon(url, data);
    };
  }
})();`;
}

/**
 * Installs a context-level route that enforces the domain allowlist.
 * Both document navigations and sub-resource requests (scripts, images, fetch, etc.)
 * to non-allowed domains are blocked, preventing data exfiltration.
 * Non-http(s) schemes (data:, blob:, etc.) are allowed for sub-resources
 * but blocked for document navigations.
 *
 * Also installs an init script that patches WebSocket, EventSource, and
 * navigator.sendBeacon to block connections to non-allowed domains. This is
 * a best-effort defense: if eval is permitted by action policy, page scripts
 * could theoretically restore the originals. Denying the eval action
 * category closes that loophole.
 */
export async function installDomainFilter(
  context: BrowserContext,
  allowedDomains: string[]
): Promise<void> {
  if (allowedDomains.length === 0) return;

  await context.addInitScript(buildWebSocketFilterScript(allowedDomains));

  await context.route('**/*', async (route: Route) => {
    const request = route.request();
    const urlStr = request.url();

    if (!urlStr.startsWith('http://') && !urlStr.startsWith('https://')) {
      if (request.resourceType() === 'document') {
        await route.abort('blockedbyclient');
      } else {
        await route.continue();
      }
      return;
    }

    let hostname: string;
    try {
      hostname = new URL(urlStr).hostname.toLowerCase();
    } catch {
      await route.abort('blockedbyclient');
      return;
    }

    if (isDomainAllowed(hostname, allowedDomains)) {
      await route.continue();
    } else {
      await route.abort('blockedbyclient');
    }
  });
}
