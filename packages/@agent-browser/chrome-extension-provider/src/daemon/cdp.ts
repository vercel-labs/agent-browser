import type { BridgeTab, CdpError, CdpRequest, Params } from "../protocol.js";

export class ProtocolError extends Error {
  constructor(message: string, readonly code = -32000) { super(message); }
  toJSON(): CdpError { return { code: this.code, message: this.message }; }
}

export function record(value: unknown): value is Params {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function parseRequest(raw: string): CdpRequest {
  const value: unknown = JSON.parse(raw);
  if (!record(value) || !Number.isSafeInteger(value.id) || typeof value.method !== "string" ||
      !/^[A-Za-z]+\.[A-Za-z]+$/.test(value.method) ||
      (value.params !== undefined && !record(value.params)) ||
      (value.sessionId !== undefined && typeof value.sessionId !== "string")) {
    throw new ProtocolError("Invalid CDP request", -32600);
  }
  return value as CdpRequest;
}

export function validTab(value: unknown): value is BridgeTab {
  return record(value) && Number.isSafeInteger(value.tabId) && Number(value.tabId) >= 0 &&
    typeof value.title === "string" && value.title.length <= 4096 &&
    typeof value.url === "string" && value.url.length <= 32768 && isAutomatableUrl(value.url);
}

export function isAutomatableUrl(url: string): boolean {
  return url === "about:blank" || /^(https?|file|data|blob):/i.test(url);
}

// Preserve the normal CDP target-info shape while exposing only the approved page.
export function targetInfoFor(targetId: string, tab: BridgeTab, attached: boolean): Params {
  return { targetId, type: "page", title: tab.title, url: tab.url, attached, canAccessOpener: false };
}

export const CHILD_FILTER = [{ type: "iframe", exclude: false }, { type: "worker", exclude: false }, { exclude: true }];
export function autoAttachParams(params: Params): Params {
  if (typeof params.autoAttach !== "boolean" || typeof params.waitForDebuggerOnStart !== "boolean" || params.flatten !== true) {
    throw new ProtocolError("Auto-attach requires booleans and flatten: true", -32602);
  }
  if (params.filter !== undefined && JSON.stringify(params.filter) !== JSON.stringify(CHILD_FILTER)) {
    throw new ProtocolError("Only iframe and dedicated-worker auto-attach is supported");
  }
  return { autoAttach: params.autoAttach, waitForDebuggerOnStart: params.waitForDebuggerOnStart, flatten: true, filter: CHILD_FILTER };
}

const PAGE_DOMAINS = new Set([
  "Accessibility", "Animation", "Audits", "Console", "CSS", "Debugger", "DOM", "DOMDebugger",
  "DOMSnapshot", "Emulation", "Fetch", "Input", "Inspector", "Log", "Network", "Overlay",
  "Page", "Performance", "PerformanceTimeline", "Profiler", "Runtime", "WebAudio", "WebAuthn", "WebMCP",
]);
const DENIED_METHODS = new Set([
  "Page.close", "Page.crash", "Network.getAllCookies", "Network.clearBrowserCookies", "Network.clearBrowserCache",
  "Network.setCookieControls", "Network.setIPProtectionProxyBypassEnabled",
]);
function httpUrl(value: unknown): URL {
  try {
    const url = new URL(String(value));
    if (!/^https?:$/.test(url.protocol) || url.username || url.password) throw new Error();
    return url;
  } catch { throw new ProtocolError("Cookie and storage operations require an HTTP(S) page URL"); }
}
function scopedUrl(value: unknown, current: URL): void {
  if (typeof value !== "string" || httpUrl(value).origin !== current.origin) {
    throw new ProtocolError("URL is outside the authorized target origin");
  }
}
function partition(value: unknown, topUrl: string): void {
  if (!record(value) || Object.keys(value).some(key => !["topLevelSite", "hasCrossSiteAncestor"].includes(key)) ||
      typeof value.hasCrossSiteAncestor !== "boolean" || value.topLevelSite !== httpUrl(topUrl).origin) {
    throw new ProtocolError("Cookie partition is outside the authorized page");
  }
}
function cookie(value: unknown, current: URL, topUrl: string): Params {
  if (!record(value)) throw new ProtocolError("Invalid cookie", -32602);
  if (value.url !== undefined) scopedUrl(value.url, current);
  if (value.domain !== undefined && value.domain !== current.hostname) {
    throw new ProtocolError("Cookie domain must exactly match the authorized target host");
  }
  if (value.partitionKey !== undefined) partition(value.partitionKey, topUrl);
  if (value.sourceScheme !== undefined && value.sourceScheme !== (current.protocol === "https:" ? "Secure" : "NonSecure")) {
    throw new ProtocolError("Cookie source scheme is outside the authorized origin");
  }
  if (value.sourcePort !== undefined && value.sourcePort !== Number(current.port || (current.protocol === "https:" ? 443 : 80))) {
    throw new ProtocolError("Cookie source port is outside the authorized origin");
  }
  if (record(value.partitionKey) && current.origin === httpUrl(topUrl).origin && value.partitionKey.hasCrossSiteAncestor !== false) {
    throw new ProtocolError("Cookie partition is outside the authorized page");
  }
  if (value.partitionKeyOpaque !== undefined || value.browserContextId !== undefined) {
    throw new ProtocolError("Unscoped cookie parameters are not supported");
  }
  // URL also confines domain-only deletion to the approved origin.
  return { ...value, url: value.url ?? current.href };
}

/** Validate request parameters before Chrome sees them; response filtering is insufficient. */
export function scopedPageParams(method: string, params: Params, url: string, topUrl: string): Params {
  if (params.browserContextId !== undefined || params.targetId !== undefined || params.sessionId !== undefined) {
    throw new ProtocolError("Cross-target routing parameters are not supported");
  }
  if (method === "Network.getCookies") {
    const current = httpUrl(url);
    if (Object.keys(params).some(key => key !== "urls")) throw new ProtocolError("Unscoped cookie parameters are not supported");
    const urls = params.urls ?? [current.href];
    if (!Array.isArray(urls) || urls.length === 0 || urls.length > 100) throw new ProtocolError("Provide scoped cookie URLs", -32602);
    for (const item of urls) scopedUrl(item, current);
    return { urls };
  }
  if (method === "Network.setCookie" || method === "Network.deleteCookies") return cookie(params, httpUrl(url), topUrl);
  if (method === "Network.setCookies") {
    if (Object.keys(params).some(key => key !== "cookies") || !Array.isArray(params.cookies) || params.cookies.length > 100) {
      throw new ProtocolError("Invalid scoped cookies", -32602);
    }
    const current = httpUrl(url);
    return { cookies: params.cookies.map(item => cookie(item, current, topUrl)) };
  }
  if (method.startsWith("DOMStorage.")) {
    if (method === "DOMStorage.enable" || method === "DOMStorage.disable") return {};
    const storage = params.storageId;
    if (!record(storage) || storage.storageKey !== undefined || storage.securityOrigin !== httpUrl(url).origin) {
      throw new ProtocolError("Storage origin is outside the authorized target");
    }
    return params;
  }
  if (method === "Storage.clearDataForOrigin" || method === "Storage.getUsageAndQuota") {
    if (params.origin !== httpUrl(url).origin || Object.keys(params).some(key => !["origin", "storageTypes"].includes(key)) ||
        (method === "Storage.clearDataForOrigin" && (typeof params.storageTypes !== "string" ||
         params.storageTypes.split(",").some(type => !["local_storage", "indexeddb", "cache_storage"].includes(type))))) {
      throw new ProtocolError("Storage operation is outside the authorized target origin");
    }
    return params;
  }
  if (DENIED_METHODS.has(method) || !PAGE_DOMAINS.has(method.split(".")[0])) {
    throw new ProtocolError("This method is not supported for an authorized existing tab", -32601);
  }
  return params;
}

/** Chrome may return multiple partition variants for a URL; do not expose unrelated ones. */
export function scopedCookiesResult(result: unknown, params: Params, topUrl: string): unknown {
  if (!record(result) || !Array.isArray(result.cookies) || !Array.isArray(params.urls)) return result;
  const urls = params.urls.map(httpUrl);
  return { ...result, cookies: result.cookies.filter(value => {
    if (!record(value) || typeof value.domain !== "string" || typeof value.path !== "string" || value.partitionKeyOpaque === true) return false;
    if (value.partitionKey !== undefined) {
      try { partition(value.partitionKey, topUrl); } catch { return false; }
    }
    return urls.some(url => {
      const domain = String(value.domain).replace(/^\./, "").toLowerCase();
      const path = String(value.path);
      const hostMatches = url.hostname === domain || (String(value.domain).startsWith(".") && url.hostname.endsWith(`.${domain}`));
      const pathMatches = url.pathname === path || (url.pathname.startsWith(path) && (path.endsWith("/") || url.pathname[path.length] === "/"));
      return hostMatches && pathMatches && (!value.secure || url.protocol === "https:") &&
        !(record(value.partitionKey) && url.origin === httpUrl(topUrl).origin && value.partitionKey.hasCrossSiteAncestor !== false);
    });
  }) };
}
