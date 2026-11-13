export interface CookieEntry {
  name: string;
  value: string;
  domain?: string;
  path?: string;
  expires?: number;
  httpOnly?: boolean;
  secure?: boolean;
  sameSite?: string;
}

export interface StorageEntry {
  key: string;
  value: string;
}

function responseData(stdout: string): unknown {
  try {
    const response = JSON.parse(stdout);
    return response && typeof response === "object" ? response.data : undefined;
  } catch {
    return undefined;
  }
}

export function parseCookies(stdout: string): CookieEntry[] {
  const data = responseData(stdout);
  if (!data || typeof data !== "object" || !("cookies" in data) || !Array.isArray(data.cookies)) {
    return [];
  }
  return data.cookies
    .filter((cookie): cookie is Record<string, unknown> =>
      cookie !== null && typeof cookie === "object" &&
      typeof cookie.name === "string" && typeof cookie.value === "string")
    .map((cookie) => ({
      name: cookie.name as string,
      value: cookie.value as string,
      domain: typeof cookie.domain === "string" ? cookie.domain : undefined,
      path: typeof cookie.path === "string" ? cookie.path : undefined,
      expires: typeof cookie.expires === "number" ? cookie.expires : undefined,
      httpOnly: typeof cookie.httpOnly === "boolean" ? cookie.httpOnly : undefined,
      secure: typeof cookie.secure === "boolean" ? cookie.secure : undefined,
      sameSite: typeof cookie.sameSite === "string" ? cookie.sameSite : undefined,
    }));
}

export function parseStorageEntries(stdout: string): StorageEntry[] {
  const data = responseData(stdout);
  if (!data || typeof data !== "object" || !("data" in data)) return [];
  const entries = data.data;
  if (!entries || typeof entries !== "object" || Array.isArray(entries)) return [];
  return Object.entries(entries).map(([key, value]) => ({ key, value: String(value) }));
}

export function cookieKey(cookie: CookieEntry): string {
  return JSON.stringify([cookie.domain ?? "", cookie.path ?? "", cookie.name]);
}
