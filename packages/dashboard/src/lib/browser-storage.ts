/** Read optional browser state without making storage access a prerequisite for the UI. */
export function readBrowserStorage(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeBrowserStorage(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // The in-memory state remains usable when storage is unavailable or full.
  }
}

export function removeBrowserStorage(key: string): void {
  try {
    window.localStorage.removeItem(key);
  } catch {
    // Forgetting in-memory state must not depend on persistent storage access.
  }
}
