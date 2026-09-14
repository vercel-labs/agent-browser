import type { BridgeTab } from "../../src/protocol";
import { parsePairingCode, type SessionView, type SetupStatus } from "./messages";
import "./style.css";

const form = document.querySelector<HTMLFormElement>("#pair-form")!;
const input = document.querySelector<HTMLInputElement>("#pairing-code")!;
const pairButton = document.querySelector<HTMLButtonElement>("#pair-button")!;
const connectionMessage = document.querySelector<HTMLElement>("#connection-message")!;
const errorElement = document.querySelector<HTMLElement>("#error")!;
const container = document.querySelector<HTMLElement>("#sessions")!;
const selected = new Map<string, BridgeTab>();
let lastStatus = "";
let refreshing = false;

async function request<T>(message: object): Promise<T> {
  const response = await chrome.runtime.sendMessage(message) as { ok: boolean; result?: T; error?: string } | undefined;
  if (!response?.ok) throw new Error(response?.error ?? "The extension restarted. Run setup to authorize again.");
  return response.result as T;
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  void connect();
});

async function connect(): Promise<void> {
  if (pairButton.disabled) return;
  setError();
  try {
    const code = input.value.trim();
    const { port } = parsePairingCode(code);
    if (document.visibilityState !== "visible" || !document.hasFocus()) {
      throw new Error("Keep this setup page in front while allowing the local connection.");
    }
    pairButton.disabled = true;
    connectionMessage.textContent = "Connecting to the local service. Allow access if Chrome asks.";
    // A foreground request allows Chrome to show Local Network Access permission.
    // The health endpoint may return 401; no credential is needed for this probe.
    await fetch(`http://127.0.0.1:${port}/health`, {
      mode: "no-cors", cache: "no-store", credentials: "omit", redirect: "error",
      signal: AbortSignal.timeout(60_000),
    }).catch(() => { throw new Error("Could not reach the local service. Keep setup running and allow local access in Chrome, then try again."); });
    await request({ kind: "setup-pair", pairingCode: code });
    input.value = "";
    connectionMessage.textContent = "Choose a page below when the session connects.";
    await refresh();
  } catch (error) {
    connectionMessage.textContent = "";
    setError(error);
  } finally {
    pairButton.disabled = false;
  }
}

document.querySelector("#refresh-button")!.addEventListener("click", () => {
  lastStatus = "";
  void refresh();
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") void refresh();
});

async function refresh(): Promise<void> {
  if (refreshing) return;
  refreshing = true;
  try {
    const status = await request<SetupStatus>({ kind: "setup-status" });
    const signature = JSON.stringify(status);
    if (signature === lastStatus) return;
    lastStatus = signature;
    render(status);
  } catch (error) {
    setError(error);
  } finally {
    refreshing = false;
  }
}

function render(status: SetupStatus): void {
  const focused = document.activeElement instanceof HTMLElement ? document.activeElement.id : "";
  container.replaceChildren();
  if (status.sessions.length === 0) {
    container.append(element("p", "No sessions connected yet."));
    return;
  }
  for (const session of status.sessions) container.append(renderSession(session, status));
  if (focused) document.getElementById(focused)?.focus({ preventScroll: true });
}

function renderSession(session: SessionView, status: SetupStatus): HTMLElement {
  const card = element("section", undefined, "card");
  card.dataset.state = session.state;
  const heading = element("div", undefined, "session-header");
  const scope = element("div");
  scope.append(element("h2", session.scope?.session ?? `Local service on port ${session.port}`, "scope"));
  if (session.scope) scope.append(element("p", `Namespace: ${session.scope.namespace || "default"}`, "namespace"));
  const states: Record<SessionView["state"], string> = {
    connecting: "Connecting", waiting: "Waiting for you", authorizing: "Authorizing",
    authorized: "Authorized", controlled: "Controlled", stopped: "Stopped",
  };
  const state = element("span", states[session.state], "state");
  state.setAttribute("role", "status");
  heading.append(scope, state);
  card.append(heading);
  if (session.tab) {
    const page = element("div", undefined, "selected-page");
    page.append(element("span", session.tab.title, "page-title"), element("span", session.tab.url, "page-url"));
    card.append(page);
  }
  if (session.state === "waiting") {
    card.append(pagePicker(session, status));
  } else {
    const explanations: Partial<Record<SessionView["state"], string>> = {
      connecting: "Waiting for the local CLI session to connect.",
      authorizing: "Waiting for the local service to accept this page.",
      authorized: "This page is authorized. Control starts when the CLI connects.",
      controlled: "The CLI can read and interact with this page. Chrome also shows its debugger notice.",
      stopped: session.reason || "Run setup for a new pairing code to authorize again.",
    };
    card.append(element("p", explanations[session.state], "explanation"));
  }
  if (session.state !== "stopped") {
    const stop = element("button", "Stop", "stop");
    stop.type = "button";
    stop.id = `stop-${session.id}`;
    stop.addEventListener("click", () => void action(stop, { kind: "setup-stop", sessionId: session.id }));
    card.append(stop);
  }
  return card;
}

function pagePicker(session: SessionView, status: SetupStatus): HTMLElement {
  const wrapper = element("div");
  const fieldset = element("fieldset");
  fieldset.append(element("legend", "Select one open page"));
  const choices = element("div", undefined, "pages");
  const authorize = element("button", "Authorize selected page");
  authorize.type = "button";
  authorize.id = `authorize-${session.id}`;
  const selection = selected.get(session.id);
  authorize.disabled = !status.tabs.some((tab) => !tab.busy && tab.tabId === selection?.tabId && tab.url === selection.url);
  for (const tab of status.tabs) {
    const label = element("label", undefined, "page-option");
    const radio = element("input");
    radio.type = "radio";
    radio.name = `tab-${session.id}`;
    radio.id = `tab-${session.id}-${tab.tabId}`;
    radio.value = String(tab.tabId);
    radio.disabled = tab.busy;
    radio.checked = !tab.busy && selection?.tabId === tab.tabId && selection.url === tab.url;
    radio.addEventListener("change", () => {
      selected.set(session.id, tab);
      authorize.disabled = false;
    });
    const details = element("span");
    details.append(element("span", `${tab.title}${tab.busy ? " (in another session)" : ""}`, "page-title"), element("span", tab.url, "page-url"));
    label.append(radio, details);
    choices.append(label);
  }
  if (status.tabs.length === 0) choices.append(element("p", "Open an HTTP or HTTPS page in Chrome, then refresh this list."));
  fieldset.append(choices);
  authorize.addEventListener("click", () => {
    const tab = selected.get(session.id);
    if (tab) void action(authorize, { kind: "setup-grant", sessionId: session.id, tabId: tab.tabId, url: tab.url });
  });
  wrapper.append(fieldset, authorize);
  return wrapper;
}

async function action(button: HTMLButtonElement, message: object): Promise<void> {
  button.disabled = true;
  setError();
  try {
    const status = await request<SetupStatus>(message);
    lastStatus = JSON.stringify(status);
    render(status);
  } catch (error) {
    setError(error);
    button.disabled = false;
    await refresh();
  }
}

function setError(error?: unknown): void {
  errorElement.textContent = error === undefined ? "" : error instanceof Error ? error.message : String(error);
  errorElement.hidden = error === undefined;
}

function element<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

setInterval(() => { if (document.visibilityState === "visible") void refresh(); }, 2_000);
void refresh();
