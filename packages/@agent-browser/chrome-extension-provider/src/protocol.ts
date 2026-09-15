/** The external provider contract is shared with the native CLI. */
export const PLUGIN_PROTOCOL = "agent-browser.plugin.v1";
export const BRIDGE_PROTOCOL_VERSION = 1;
export const PLUGIN_NAME = "chrome-extension";
export type Params = Record<string, unknown>;
export type Scope = { namespace: string; session: string };
export type BridgeTab = { tabId: number; url: string; title: string };
export type CdpError = { code: number; message: string };
export type CdpRequest = { id: number; method: string; params?: Params; sessionId?: string };

/** One extension socket serves one explicitly approved page in one CLI scope. */
export type ExtensionMessage =
  | { v: 1; kind: "hello"; profileId: string; extensionId: string; chromeVersion: string }
  | { v: 1; kind: "grant"; tab: BridgeTab }
  | { v: 1; kind: "tab-updated"; tab: BridgeTab }
  | { v: 1; kind: "heartbeat" }
  | { v: 1; kind: "revoked"; reason: string }
  | { v: 1; kind: "cdp-result"; reqId: string; result?: unknown; error?: CdpError }
  | { v: 1; kind: "cdp-event"; tabId: number; sessionId?: string; method: string; params: Params };

export type BridgeCommand = {
  v: 1;
  kind: "cdp-command";
  reqId: string;
  tabId: number;
  /** Native child session ID, absent for the approved page itself. */
  sessionId?: string;
  /** Bridge.attach/detach/activate are internal operations; all others are CDP. */
  method: string;
  params: Params;
};
export type RelayMessage =
  | { v: 1; kind: "paired"; requestId: string; scope: Scope }
  | { v: 1; kind: "granted" }
  | { v: 1; kind: "released"; reason: string }
  | { v: 1; kind: "error"; message: string }
  | BridgeCommand;

export type SetupResult = { requestId: string; pairingCode: string; expiresAt: string };
export type LeaseResult = { leaseId: string; cdpUrl: string };
export type ScopeStatus = {
  state: "not-paired" | "awaiting-approval" | "authorized" | "reserved" | "connected";
  scope: Scope;
  tab?: BridgeTab;
};
