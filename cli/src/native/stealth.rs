//! Stealth-mode evasions injected into every page via
//! `Page.addScriptToEvaluateOnNewDocument` when `--stealth` /
//! `AGENT_BROWSER_STEALTH=1` is enabled.
//!
//! The script masks the most common bot-detection signals shipped by stock
//! Chromium: `navigator.webdriver`, missing `chrome.runtime`, empty plugins,
//! identical `navigator.languages`, the WebGL vendor/renderer tuple, and the
//! permissions `Notification` mismatch. It is paired with the
//! `--disable-blink-features=AutomationControlled` launch arg in `chrome.rs`
//! to also drop the corresponding header/feature signals.
//!
//! These evasions defeat the bulk of "is this a headless Chrome" checks
//! (e.g. bot.sannysoft.com). They do NOT defeat detectors that probe deeper
//! CDP signatures (e.g. `Runtime.enable` side-effects); for those we'd need
//! to patch the CDP layer itself, which is out of scope for this flag.

pub const STEALTH_INIT_SCRIPT: &str = r#"
(() => {
  try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      configurable: true,
      enumerable: true,
      get: () => false,
    });
  } catch (_) {}

  try {
    if (!window.chrome) {
      window.chrome = {};
    }
    if (!window.chrome.runtime) {
      window.chrome.runtime = {
        OnInstalledReason: { CHROME_UPDATE: 'chrome_update', INSTALL: 'install', SHARED_MODULE_UPDATE: 'shared_module_update', UPDATE: 'update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
        PlatformArch: { ARM: 'arm', ARM64: 'arm64', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
        PlatformNaclArch: { ARM: 'arm', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
        PlatformOs: { ANDROID: 'android', CROS: 'cros', LINUX: 'linux', MAC: 'mac', OPENBSD: 'openbsd', WIN: 'win' },
        RequestUpdateCheckStatus: { NO_UPDATE: 'no_update', THROTTLED: 'throttled', UPDATE_AVAILABLE: 'update_available' },
      };
    }
  } catch (_) {}

  try {
    const originalQuery = window.navigator.permissions && window.navigator.permissions.query;
    if (originalQuery) {
      window.navigator.permissions.query = (parameters) =>
        parameters && parameters.name === 'notifications'
          ? Promise.resolve({ state: Notification.permission })
          : originalQuery(parameters);
    }
  } catch (_) {}

  try {
    const fakePlugins = [
      { name: 'PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
      { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
      { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
      { name: 'Microsoft Edge PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
      { name: 'WebKit built-in PDF', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
    ];
    Object.defineProperty(Navigator.prototype, 'plugins', {
      configurable: true,
      enumerable: true,
      get: () => fakePlugins,
    });
    Object.defineProperty(Navigator.prototype, 'mimeTypes', {
      configurable: true,
      enumerable: true,
      get: () => [{ type: 'application/pdf', suffixes: 'pdf', description: '' }],
    });
  } catch (_) {}

  try {
    Object.defineProperty(Navigator.prototype, 'languages', {
      configurable: true,
      enumerable: true,
      get: () => ['en-US', 'en'],
    });
  } catch (_) {}

  try {
    const patchGetParameter = (proto) => {
      if (!proto || !proto.getParameter) return;
      const original = proto.getParameter;
      proto.getParameter = function (parameter) {
        // UNMASKED_VENDOR_WEBGL
        if (parameter === 37445) return 'Intel Inc.';
        // UNMASKED_RENDERER_WEBGL
        if (parameter === 37446) return 'Intel Iris OpenGL Engine';
        return original.apply(this, arguments);
      };
    };
    if (typeof WebGLRenderingContext !== 'undefined') {
      patchGetParameter(WebGLRenderingContext.prototype);
    }
    if (typeof WebGL2RenderingContext !== 'undefined') {
      patchGetParameter(WebGL2RenderingContext.prototype);
    }
  } catch (_) {}
})();
"#;
