import { readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';

export interface ActionPolicy {
  default: 'allow' | 'deny';
  allow?: string[];
  deny?: string[];
}

export type PolicyDecision = 'allow' | 'deny' | 'confirm';

const ACTION_CATEGORIES: Record<string, string> = {
  navigate: 'navigate',
  back: 'navigate',
  forward: 'navigate',
  reload: 'navigate',
  tab_new: 'navigate',

  click: 'click',
  dblclick: 'click',
  tap: 'click',

  fill: 'fill',
  type: 'fill',
  // The `keyboard` action is a compound command that dispatches to sub-actions
  // (type, inserttext, press, down, up). Its primary use is text input, so it
  // maps to 'fill'. The interact-like sub-actions (press, down, up) are less
  // common and don't have separate top-level action names in the protocol.
  keyboard: 'fill',
  inserttext: 'fill',
  select: 'fill',
  multiselect: 'fill',
  check: 'fill',
  uncheck: 'fill',
  clear: 'fill',
  selectall: 'fill',
  setvalue: 'fill',

  download: 'download',
  waitfordownload: 'download',

  upload: 'upload',

  evaluate: 'eval',
  evalhandle: 'eval',
  addscript: 'eval',
  addinitscript: 'eval',

  snapshot: 'snapshot',
  screenshot: 'snapshot',
  pdf: 'snapshot',
  diff_snapshot: 'snapshot',
  diff_screenshot: 'snapshot',
  diff_url: 'snapshot',

  scroll: 'scroll',
  scrollintoview: 'scroll',

  wait: 'wait',
  waitforurl: 'wait',
  waitforloadstate: 'wait',
  waitforfunction: 'wait',

  gettext: 'get',
  content: 'get',
  innerhtml: 'get',
  innertext: 'get',
  inputvalue: 'get',
  url: 'get',
  title: 'get',
  getattribute: 'get',
  count: 'get',
  boundingbox: 'get',
  styles: 'get',
  isvisible: 'get',
  isenabled: 'get',
  ischecked: 'get',
  responsebody: 'get',

  route: 'network',
  unroute: 'network',
  requests: 'network',

  state_save: 'state',
  state_load: 'state',
  cookies_set: 'state',
  storage_set: 'state',
  credentials: 'state',

  hover: 'interact',
  focus: 'interact',
  drag: 'interact',
  press: 'interact',
  keydown: 'interact',
  keyup: 'interact',
  mousemove: 'interact',
  mousedown: 'interact',
  mouseup: 'interact',
  wheel: 'interact',
  dispatch: 'interact',

  // These are always allowed (internal/meta operations)
  launch: '_internal',
  close: '_internal',
  tab_list: '_internal',
  tab_switch: '_internal',
  tab_close: '_internal',
  window_new: '_internal',
  frame: '_internal',
  mainframe: '_internal',
  dialog: '_internal',
  session: '_internal',
  console: '_internal',
  errors: '_internal',
  cookies_get: '_internal',
  cookies_clear: '_internal',
  storage_get: '_internal',
  storage_clear: '_internal',
  state_list: '_internal',
  state_show: '_internal',
  state_clear: '_internal',
  state_clean: '_internal',
  state_rename: '_internal',
  highlight: '_internal',
  bringtofront: '_internal',
  trace_start: '_internal',
  trace_stop: '_internal',
  har_start: '_internal',
  har_stop: '_internal',
  video_start: '_internal',
  video_stop: '_internal',
  recording_start: '_internal',
  recording_stop: '_internal',
  recording_restart: '_internal',
  profiler_start: '_internal',
  profiler_stop: '_internal',
  clipboard: '_internal',
  viewport: '_internal',
  useragent: '_internal',
  device: '_internal',
  geolocation: '_internal',
  permissions: '_internal',
  emulatemedia: '_internal',
  offline: '_internal',
  headers: '_internal',
  addstyle: 'eval',
  expose: 'eval',
  timezone: '_internal',
  locale: '_internal',
  pause: '_internal',
  setcontent: 'eval',
  screencast_start: '_internal',
  screencast_stop: '_internal',
  input_mouse: '_internal',
  input_keyboard: '_internal',
  input_touch: '_internal',

  auth_save: '_internal',
  auth_login: '_internal',
  auth_list: '_internal',
  auth_delete: '_internal',
  auth_show: '_internal',
  confirm: '_internal',
  deny: '_internal',

  // Find/semantic locator actions (read-only element resolution)
  getbyrole: 'get',
  getbytext: 'get',
  getbylabel: 'get',
  getbyplaceholder: 'get',
  getbyalttext: 'get',
  getbytitle: 'get',
  getbytestid: 'get',
  nth: 'get',
};

// User-facing categories used in policy files. '_internal' is excluded because
// internal actions always bypass policy. 'unknown' is intentionally not a value
// in ACTION_CATEGORIES -- it is only the fallback return of getActionCategory()
// for unrecognized actions. If a user puts "unknown" in a policy file,
// loadPolicyFile will warn about it as unrecognized, which is correct.
export const KNOWN_CATEGORIES = new Set(
  Object.values(ACTION_CATEGORIES).filter((c) => c !== '_internal')
);

export function getActionCategory(action: string): string {
  return ACTION_CATEGORIES[action] ?? 'unknown';
}

export function loadPolicyFile(policyPath: string): ActionPolicy {
  const resolved = resolve(policyPath);
  const content = readFileSync(resolved, 'utf-8');
  const policy = JSON.parse(content) as ActionPolicy;

  if (policy.default !== 'allow' && policy.default !== 'deny') {
    throw new Error(
      `Invalid action policy: "default" must be "allow" or "deny", got "${policy.default}"`
    );
  }

  for (const list of [policy.allow, policy.deny]) {
    if (!list) continue;
    for (const category of list) {
      if (!KNOWN_CATEGORIES.has(category)) {
        console.warn(
          `[agent-browser] Warning: unrecognized action category "${category}" in policy file. ` +
            `Known categories: ${[...KNOWN_CATEGORIES].sort().join(', ')}`
        );
      }
    }
  }

  return policy;
}

let cachedPolicyPath: string | null = null;
let cachedPolicyMtimeMs = 0;
let cachedPolicy: ActionPolicy | null = null;
const RELOAD_CHECK_INTERVAL_MS = 5_000;
let lastCheckMs = 0;

export function initPolicyReloader(policyPath: string, policy: ActionPolicy): void {
  cachedPolicyPath = resolve(policyPath);
  cachedPolicyMtimeMs = statSync(cachedPolicyPath).mtimeMs;
  cachedPolicy = policy;
}

export function reloadPolicyIfChanged(): ActionPolicy | null {
  if (!cachedPolicyPath) return cachedPolicy;

  const now = Date.now();
  if (now - lastCheckMs < RELOAD_CHECK_INTERVAL_MS) return cachedPolicy;
  lastCheckMs = now;

  try {
    const currentMtime = statSync(cachedPolicyPath).mtimeMs;
    if (currentMtime !== cachedPolicyMtimeMs) {
      cachedPolicy = loadPolicyFile(cachedPolicyPath);
      cachedPolicyMtimeMs = currentMtime;
    }
  } catch {
    // File may have been removed; keep using cached policy
  }

  return cachedPolicy;
}

export function checkPolicy(
  action: string,
  policy: ActionPolicy | null,
  confirmCategories: Set<string>
): PolicyDecision {
  const category = getActionCategory(action);

  // Internal actions are always allowed
  if (category === '_internal') return 'allow';

  // Explicit deny takes precedence over confirmation
  if (policy?.deny?.includes(category)) return 'deny';

  // Check if this category requires confirmation
  if (confirmCategories.has(category)) return 'confirm';

  if (!policy) return 'allow';

  // Explicit allow list
  if (policy.allow?.includes(category)) return 'allow';

  return policy.default;
}

export function describeAction(action: string, command: Record<string, unknown>): string {
  const category = getActionCategory(action);
  switch (action) {
    case 'navigate':
      return `Navigate to ${command.url}`;
    case 'evaluate':
    case 'evalhandle':
      return `Evaluate JavaScript: ${String(command.script ?? '').slice(0, 80)}`;
    case 'fill':
      return `Fill ${command.selector}`;
    case 'type':
      return `Type into ${command.selector}`;
    case 'click':
      return `Click ${command.selector}`;
    case 'dblclick':
      return `Double-click ${command.selector}`;
    case 'tap':
      return `Tap ${command.selector}`;
    case 'download':
      return `Download via ${command.selector} to ${command.path}`;
    case 'upload':
      return `Upload files to ${command.selector}`;
    default:
      return `${category}: ${action}`;
  }
}
