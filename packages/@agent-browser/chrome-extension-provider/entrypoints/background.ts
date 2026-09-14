import { defineBackground } from "wxt/utils/define-background";
import {
  BRIDGE_PROTOCOL_VERSION,
  type BridgeCommand,
  type BridgeTab,
  type CdpError,
  type ExtensionMessage,
  type Params,
  type RelayMessage,
} from "../src/protocol";
import { isAllowedPage, parsePairingCode, type SessionView, type SetupStatus } from "./setup/messages";

type Session = SessionView & {
  socket: WebSocket;
  requestId?: string;
  attached: boolean;
  attaching?: Promise<void>;
  stopping?: Promise<void>;
  children: Map<string, { parent?: string; targetId: string }>;
  pending: Set<string>;
  heartbeat?: ReturnType<typeof setInterval>;
  handshake?: ReturnType<typeof setTimeout>;
};

// Only the profile identity survives a worker restart. Sockets and grants do not.
const sessions = new Map<string, Session>();
const tabOwners = new Map<number, Session>();
let profileId: Promise<string> | undefined;

export default defineBackground(() => {
  chrome.runtime.onInstalled.addListener((details) => {
    if (details.reason === "install") void openSetup();
  });
  chrome.action.onClicked.addListener(() => void openSetup());
  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL("setup.html")) {
      return false;
    }
    void handleSetupMessage(message).then(
      (result) => sendResponse({ ok: true, result }),
      (error) => sendResponse({ ok: false, error: errorMessage(error) }),
    );
    return true;
  });
  chrome.debugger.onEvent.addListener((source, method, params) => {
    void handleDebuggerEvent(source, method, (params ?? {}) as Params);
  });
  chrome.debugger.onDetach.addListener((source, reason) => {
    if (source.tabId === undefined) return;
    const session = tabOwners.get(source.tabId);
    if (!session) return;
    session.attached = false;
    if (session.state === "stopped") {
      tabOwners.delete(source.tabId);
      return;
    }
    void stopSession(session, `Chrome stopped debugging: ${reason}.`);
  });
  chrome.tabs.onRemoved.addListener((tabId) => {
    const session = tabOwners.get(tabId);
    if (session) void stopSession(session, "The authorized tab was closed.");
  });
  chrome.tabs.onReplaced.addListener((_addedTabId, removedTabId) => {
    const session = tabOwners.get(removedTabId);
    if (session) void stopSession(session, "Chrome replaced the authorized tab. Run setup to select it again.");
  });
  chrome.tabs.onUpdated.addListener((tabId, _change, tab) => {
    const session = tabOwners.get(tabId);
    if (!session || session.state === "stopped") return;
    const current = toBridgeTab(tab);
    if (!current || (tab.pendingUrl && !isAllowedPage(tab.pendingUrl))) {
      void stopSession(session, "The authorized tab opened a restricted page.");
      return;
    }
    session.tab = current;
    send(session, { v: BRIDGE_PROTOCOL_VERSION, kind: "tab-updated", tab: current });
  });
  void updateBadge();
});

async function openSetup(): Promise<void> {
  const url = chrome.runtime.getURL("setup.html");
  const existing = (await chrome.tabs.query({})).find((tab) => tab.url === url);
  if (existing?.id !== undefined) {
    await chrome.tabs.update(existing.id, { active: true });
    await chrome.windows.update(existing.windowId, { focused: true });
  } else {
    await chrome.tabs.create({ url });
  }
}

async function handleSetupMessage(message: unknown): Promise<unknown> {
  if (!isRecord(message)) throw new Error("Invalid setup request.");
  if (message.kind === "setup-status") return await status();
  if (message.kind === "setup-pair") return await pair(message.pairingCode);
  const session = typeof message.sessionId === "string" ? sessions.get(message.sessionId) : undefined;
  if (!session) throw new Error("This session has ended. Run setup for a new pairing code.");
  if (message.kind === "setup-stop") {
    await stopSession(session, "Stopped by you.");
    return await status();
  }
  if (message.kind === "setup-grant") {
    if (!Number.isInteger(message.tabId) || typeof message.url !== "string") {
      throw new Error("Select a page to authorize.");
    }
    await grant(session, message.tabId as number, message.url);
    return await status();
  }
  throw new Error("Unknown setup request.");
}

async function status(): Promise<SetupStatus> {
  const tabs = (await chrome.tabs.query({})).flatMap((tab) => {
    const page = toBridgeTab(tab);
    return page ? [{ ...page, busy: tabOwners.has(page.tabId) }] : [];
  });
  return {
    sessions: [...sessions.values()].map(({ id, port, state, scope, tab, reason }) => ({
      id, port, state, scope, tab, reason,
    })),
    tabs,
  };
}

async function pair(code: unknown): Promise<{ sessionId: string }> {
  const { port, token } = parsePairingCode(code);
  const identity = await getProfileId();
  const socket = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${token}`);
  const session: Session = {
    id: crypto.randomUUID(), port, state: "connecting", socket,
    attached: false, children: new Map(), pending: new Set(),
  };
  sessions.set(session.id, session);
  socket.addEventListener("open", () => {
    if (session.state === "stopped") return;
    send(session, {
      v: BRIDGE_PROTOCOL_VERSION, kind: "hello", profileId: identity,
      extensionId: chrome.runtime.id, chromeVersion: /Chrome\/([^ ]+)/.exec(navigator.userAgent)?.[1] ?? "unknown",
    });
    session.heartbeat = setInterval(() => {
      send(session, { v: BRIDGE_PROTOCOL_VERSION, kind: "heartbeat" });
    }, 20_000);
  });
  socket.addEventListener("message", (event) => {
    if (session.state === "stopped") return;
    try {
      const message: unknown = JSON.parse(String(event.data));
      if (!isRecord(message) || message.v !== BRIDGE_PROTOCOL_VERSION || typeof message.kind !== "string") {
        throw new Error("Invalid local relay message.");
      }
      handleRelayMessage(session, message as RelayMessage);
    } catch {
      void stopSession(session, "The local relay sent an invalid message.");
    }
  });
  socket.addEventListener("close", () => void stopSession(session, "The local relay disconnected. Run setup to authorize again."));
  socket.addEventListener("error", () => void stopSession(session, "Could not connect to the local relay. Run setup for a new pairing code."));
  session.handshake = setTimeout(() => {
    void stopSession(session, "Pairing timed out. Run setup for a new pairing code.");
  }, 15_000);
  return { sessionId: session.id };
}

function handleRelayMessage(session: Session, message: RelayMessage): void {
  switch (message.kind) {
    case "paired":
      if (session.state !== "connecting" || typeof message.requestId !== "string" ||
          !isRecord(message.scope) || typeof message.scope.namespace !== "string" || typeof message.scope.session !== "string") {
        throw new Error("Invalid pairing response.");
      }
      clearTimeout(session.handshake);
      session.requestId = message.requestId;
      session.scope = message.scope;
      session.state = "waiting";
      break;
    case "granted":
      if (session.state !== "authorizing" || !session.tab) throw new Error("Unexpected authorization response.");
      session.state = "authorized";
      void updateBadge();
      break;
    case "released":
      void stopSession(session, message.reason || "The session ended. Run setup to authorize again.");
      break;
    case "error":
      void stopSession(session, message.message || "The local relay rejected this session.");
      break;
    case "cdp-command":
      if (typeof message.reqId !== "string" || typeof message.method !== "string" ||
          !Number.isInteger(message.tabId) || !isRecord(message.params) ||
          (message.sessionId !== undefined && typeof message.sessionId !== "string")) {
        throw new Error("Invalid command.");
      }
      void runCommand(session, message);
      break;
    default:
      throw new Error("Unknown local relay message.");
  }
}

async function grant(session: Session, tabId: number, expectedUrl: string): Promise<void> {
  if (session.state !== "waiting") throw new Error("This session is not waiting for authorization.");
  const page = toBridgeTab(await chrome.tabs.get(tabId));
  if (!page || page.url !== expectedUrl) throw new Error("This page changed. Refresh the page list and select it again.");
  if (session.state !== "waiting" || session.socket.readyState !== WebSocket.OPEN) {
    throw new Error("The local relay disconnected. Run setup again.");
  }
  if (tabOwners.has(tabId)) throw new Error("Another session already controls this tab. Stop that session first.");
  tabOwners.set(tabId, session);
  session.tab = page;
  session.state = "authorizing";
  send(session, { v: BRIDGE_PROTOCOL_VERSION, kind: "grant", tab: page });
}

async function runCommand(session: Session, command: BridgeCommand): Promise<void> {
  if (session.pending.has(command.reqId)) {
    void stopSession(session, "The local relay reused a command identifier.");
    return;
  }
  session.pending.add(command.reqId);
  try {
    assertGrant(session, command);
    if (command.method === "Bridge.detach") {
      if (command.sessionId) throw new Error("Only the authorized page can end the session.");
      await stopSession(session, "The session ended. Run setup to authorize again.", command.reqId);
      return;
    }
    let result: unknown;
    if (command.method === "Bridge.attach") {
      if (command.sessionId) throw new Error("Only the authorized page can be attached.");
      if (!session.attached) {
        session.attaching ??= chrome.debugger.attach({ tabId: command.tabId }, "1.3").then(() => { session.attached = true; });
        await session.attaching;
        assertGrant(session, command);
        session.state = "controlled";
        void updateBadge();
      }
      result = { attached: true };
    } else {
      if (!session.attached || session.state !== "controlled") throw new Error("The authorized page is not attached.");
      if (command.method === "Bridge.activate") {
        if (command.sessionId) throw new Error("Only the authorized page can be activated.");
        const tab = await chrome.tabs.update(command.tabId, { active: true });
        assertGrant(session, command);
        if (!tab) throw new Error("The authorized tab is no longer available.");
        await chrome.windows.update(tab.windowId, { focused: true });
        result = { activated: true };
      } else {
        validateTargetCommand(session, command);
        result = await chrome.debugger.sendCommand(
          { tabId: command.tabId, ...(command.sessionId ? { sessionId: command.sessionId } : {}) },
          command.method, command.params,
        );
      }
    }
    assertGrant(session, command);
    reply(session, command.reqId, result ?? {});
  } catch (error) {
    reply(session, command.reqId, undefined, cdpError(error));
    if (command.method === "Bridge.attach") void stopSession(session, `Could not attach: ${errorMessage(error)}`);
  }
}

function assertGrant(session: Session, command: BridgeCommand): void {
  if ((session.state !== "authorized" && session.state !== "controlled") ||
      !session.tab || command.tabId !== session.tab.tabId || tabOwners.get(command.tabId) !== session) {
    throw new Error("This tab is not authorized. Run setup to authorize again.");
  }
  if (command.sessionId && !session.children.has(command.sessionId)) throw new Error("The child session is not part of the authorized page.");
}

function validateTargetCommand(session: Session, command: BridgeCommand): void {
  if (!command.method.startsWith("Target.")) return;
  if (command.method === "Target.setAutoAttach") {
    const { autoAttach, flatten, filter } = command.params;
    if (typeof autoAttach !== "boolean" || flatten !== true || !Array.isArray(filter) ||
        !filter.some((entry) => isRecord(entry) && entry.exclude === true && entry.type === undefined) ||
        filter.some((entry) => !isRecord(entry) || (entry.exclude !== true && entry.type !== "iframe" && entry.type !== "worker"))) {
      throw new Error("Auto-attach must use flat sessions limited to iframes and dedicated workers.");
    }
    return;
  }
  if (command.method === "Target.detachFromTarget" && typeof command.params.sessionId === "string" &&
      session.children.has(command.params.sessionId)) return;
  throw new Error(`${command.method} is not supported by the tab debugger transport.`);
}

async function handleDebuggerEvent(source: chrome.debugger.DebuggerSession, method: string, params: Params): Promise<void> {
  if (source.tabId === undefined) return;
  const session = tabOwners.get(source.tabId);
  if (!session || session.state !== "controlled" || (source.sessionId && !session.children.has(source.sessionId))) return;
  if (method === "Target.attachedToTarget") {
    const child = params.sessionId;
    const info = params.targetInfo;
    if (typeof child !== "string" || !isRecord(info) || typeof info.targetId !== "string") return;
    if (info.type !== "iframe" && info.type !== "worker") {
      // The grant covers page children, never shared workers or other tabs.
      try {
        await chrome.debugger.sendCommand(source, "Target.detachFromTarget", { sessionId: child });
      } catch {
        void stopSession(session, "Chrome attached a target outside the authorized page.");
      }
      return;
    }
    session.children.set(child, { parent: source.sessionId, targetId: info.targetId });
  } else if (method === "Target.detachedFromTarget" && typeof params.sessionId === "string") {
    if (!session.children.has(params.sessionId)) return;
    removeChild(session, params.sessionId);
  }
  send(session, {
    v: BRIDGE_PROTOCOL_VERSION, kind: "cdp-event", tabId: source.tabId,
    ...(source.sessionId ? { sessionId: source.sessionId } : {}), method, params,
  });
}

function removeChild(session: Session, id: string): void {
  for (const [child, value] of session.children) {
    if (value.parent === id) removeChild(session, child);
  }
  session.children.delete(id);
}

function stopSession(session: Session, reason: string, detachRequest?: string): Promise<void> {
  if (session.stopping) return session.stopping;
  session.state = "stopped";
  session.reason = reason;
  clearInterval(session.heartbeat);
  clearTimeout(session.handshake);
  for (const id of session.pending) {
    if (id !== detachRequest) reply(session, id, undefined, { code: -32000, message: "The page authorization ended." });
  }
  session.children.clear();
  void updateBadge();
  session.stopping = (async () => {
    // Keep ownership until a pending attach has settled and its debugger is detached.
    // This prevents a late attach callback from taking over a newly authorized session.
    await session.attaching?.catch(() => undefined);
    let detachError: CdpError | undefined;
    if (session.attached && session.tab) {
      try {
        await chrome.debugger.detach({ tabId: session.tab.tabId });
        session.attached = false;
      } catch (error) {
        detachError = cdpError(error);
        session.reason = `Control stopped, but Chrome could not detach its debugger: ${detachError.message}. Close Chrome's debugger notice to release the tab.`;
      }
    }
    if (!session.attached && session.tab && tabOwners.get(session.tab.tabId) === session) tabOwners.delete(session.tab.tabId);
    if (detachRequest) reply(session, detachRequest, detachError ? undefined : { detached: true }, detachError);
    send(session, { v: BRIDGE_PROTOCOL_VERSION, kind: "revoked", reason });
    session.socket.close();
  })();
  return session.stopping;
}

function reply(session: Session, reqId: string, result?: unknown, error?: CdpError): void {
  if (!session.pending.delete(reqId)) return;
  send(session, { v: BRIDGE_PROTOCOL_VERSION, kind: "cdp-result", reqId, ...(error ? { error } : { result }) });
}

function send(session: Session, message: ExtensionMessage): void {
  if (session.socket.readyState === WebSocket.OPEN) session.socket.send(JSON.stringify(message));
}

async function updateBadge(): Promise<void> {
  const active = [...sessions.values()].filter((session) => session.state === "controlled").length;
  const authorized = [...sessions.values()].some((session) => session.state === "authorized");
  await chrome.action.setBadgeBackgroundColor({ color: active ? "#b91c1c" : "#1d4ed8" });
  await chrome.action.setBadgeText({ text: active ? String(active) : authorized ? "OK" : "" });
}

function getProfileId(): Promise<string> {
  profileId ??= (async () => {
    const stored = await chrome.storage.local.get("profileId");
    if (typeof stored.profileId === "string" && /^[0-9a-f-]{36}$/i.test(stored.profileId)) return stored.profileId;
    const value = crypto.randomUUID();
    await chrome.storage.local.set({ profileId: value });
    return value;
  })();
  return profileId;
}

function toBridgeTab(tab: chrome.tabs.Tab): BridgeTab | undefined {
  if (tab.id === undefined || !tab.url || !isAllowedPage(tab.url) || (tab.pendingUrl && !isAllowedPage(tab.pendingUrl))) return;
  return { tabId: tab.id, url: tab.url, title: tab.title ?? "Untitled page" };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
  return isRecord(error) && typeof error.message === "string" ? error.message : String(error);
}

function cdpError(error: unknown): CdpError {
  const message = errorMessage(error);
  // Chrome wraps protocol errors as Error messages containing the original JSON.
  try {
    const parsed: unknown = JSON.parse(message);
    if (isRecord(parsed) && typeof parsed.code === "number" && typeof parsed.message === "string") {
      return { code: parsed.code, message: parsed.message };
    }
  } catch { /* Non-protocol Chrome errors retain their original message. */ }
  return { code: isRecord(error) && typeof error.code === "number" ? error.code : -32000, message };
}
