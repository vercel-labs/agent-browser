//! Stealth-mode evasions injected into every page via
//! `Page.addScriptToEvaluateOnNewDocument` when `--stealth` /
//! `AGENT_BROWSER_STEALTH=1` is enabled.
//!
//! The script masks the most common bot-detection signals shipped by stock
//! Chromium: `navigator.webdriver`, missing `chrome.runtime`, empty /
//! plain-array `navigator.plugins` (bot.sannysoft checks
//! `navigator.plugins.constructor.name === 'PluginArray'`), missing
//! `deviceMemory`/`hardwareConcurrency`, identical `navigator.languages`,
//! the WebGL vendor/renderer tuple, and the permissions `Notification`
//! mismatch. It is paired with the
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
    const makeMimeType = (type, description, suffixes, enabledPlugin) => {
      const mt = Object.create(MimeType.prototype);
      Object.defineProperties(mt, {
        type:          { value: type,          enumerable: true },
        description:   { value: description,   enumerable: true },
        suffixes:      { value: suffixes,      enumerable: true },
        enabledPlugin: { value: enabledPlugin, enumerable: true },
      });
      return mt;
    };

    const makePlugin = (name, filename, description, mimeSpecs) => {
      const plugin = Object.create(Plugin.prototype);
      Object.defineProperties(plugin, {
        name:        { value: name,        enumerable: true },
        filename:    { value: filename,    enumerable: true },
        description: { value: description, enumerable: true },
      });
      const mimes = mimeSpecs.map((s) => makeMimeType(s.type, s.description, s.suffixes, plugin));
      mimes.forEach((m, i) => Object.defineProperty(plugin, i, { value: m, enumerable: true }));
      mimes.forEach((m) => { if (!(m.type in plugin)) Object.defineProperty(plugin, m.type, { value: m }); });
      Object.defineProperty(plugin, 'length', { value: mimes.length });
      Object.defineProperty(plugin, 'item', { value: function (i) { return this[i] != null ? this[i] : null; } });
      Object.defineProperty(plugin, 'namedItem', { value: function (n) { return this[n] != null ? this[n] : null; } });
      return { plugin, mimes };
    };

    const pdfSpecs = [
      { type: 'application/pdf',       description: 'Portable Document Format', suffixes: 'pdf' },
      { type: 'text/pdf',              description: 'Portable Document Format', suffixes: 'pdf' },
    ];

    const built = [
      makePlugin('PDF Viewer',                 'internal-pdf-viewer', 'Portable Document Format', pdfSpecs),
      makePlugin('Chrome PDF Viewer',          'internal-pdf-viewer', 'Portable Document Format', pdfSpecs),
      makePlugin('Chromium PDF Viewer',        'internal-pdf-viewer', 'Portable Document Format', pdfSpecs),
      makePlugin('Microsoft Edge PDF Viewer',  'internal-pdf-viewer', 'Portable Document Format', pdfSpecs),
      makePlugin('WebKit built-in PDF',        'internal-pdf-viewer', 'Portable Document Format', pdfSpecs),
    ];
    const plugins = built.map((b) => b.plugin);
    const allMimes = [];
    built.forEach((b) => b.mimes.forEach((m) => { if (!allMimes.some((x) => x.type === m.type)) allMimes.push(m); }));

    const pluginArray = Object.create(PluginArray.prototype);
    plugins.forEach((p, i) => Object.defineProperty(pluginArray, i, { value: p, enumerable: true }));
    plugins.forEach((p) => { if (!(p.name in pluginArray)) Object.defineProperty(pluginArray, p.name, { value: p }); });
    Object.defineProperty(pluginArray, 'length',    { value: plugins.length, enumerable: true });
    Object.defineProperty(pluginArray, 'item',      { value: function (i) { return this[i] != null ? this[i] : null; } });
    Object.defineProperty(pluginArray, 'namedItem', { value: function (n) { return this[n] != null ? this[n] : null; } });
    Object.defineProperty(pluginArray, 'refresh',   { value: function () {} });

    const mimeTypeArray = Object.create(MimeTypeArray.prototype);
    allMimes.forEach((m, i) => Object.defineProperty(mimeTypeArray, i, { value: m, enumerable: true }));
    allMimes.forEach((m) => { if (!(m.type in mimeTypeArray)) Object.defineProperty(mimeTypeArray, m.type, { value: m }); });
    Object.defineProperty(mimeTypeArray, 'length',    { value: allMimes.length, enumerable: true });
    Object.defineProperty(mimeTypeArray, 'item',      { value: function (i) { return this[i] != null ? this[i] : null; } });
    Object.defineProperty(mimeTypeArray, 'namedItem', { value: function (n) { return this[n] != null ? this[n] : null; } });

    Object.defineProperty(Navigator.prototype, 'plugins', {
      configurable: true,
      enumerable: true,
      get: () => pluginArray,
    });
    Object.defineProperty(Navigator.prototype, 'mimeTypes', {
      configurable: true,
      enumerable: true,
      get: () => mimeTypeArray,
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
    Object.defineProperty(Navigator.prototype, 'deviceMemory', {
      configurable: true,
      enumerable: true,
      get: () => 8,
    });
  } catch (_) {}

  try {
    Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
      configurable: true,
      enumerable: true,
      get: () => 8,
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
