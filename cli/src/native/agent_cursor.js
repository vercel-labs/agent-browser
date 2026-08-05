(() => {
  const key = "__agentBrowserCursor";
  const current = Object.getOwnPropertyDescriptor(globalThis, key)?.value;
  if (
    current?.version === 1 &&
    typeof current.configure === "function" &&
    typeof current.moveTo === "function" &&
    typeof current.placeAt === "function" &&
    typeof current.pulse === "function"
  ) {
    return current;
  }

  const host = document.createElement("div");
  host.setAttribute("data-agent-browser-cursor", "");
  host.setAttribute("aria-hidden", "true");
  host.setAttribute("popover", "manual");
  host.inert = true;
  Object.assign(host.style, {
    position: "fixed",
    left: "0",
    top: "0",
    right: "auto",
    bottom: "auto",
    margin: "0",
    padding: "0",
    border: "0",
    background: "transparent",
    width: "30px",
    height: "34px",
    zIndex: "2147483647",
    pointerEvents: "none",
    opacity: "0",
    transform: "translate3d(-100px, -100px, 0)",
    transformOrigin: "3px 2px",
    transition: "opacity 80ms linear",
    willChange: "transform, opacity",
  });
  const protect = (property, value) =>
    host.style.setProperty(property, value, "important");
  protect("position", "fixed");
  protect("left", "0px");
  protect("top", "0px");
  protect("right", "auto");
  protect("bottom", "auto");
  protect("display", "block");
  protect("visibility", "visible");
  protect("width", "30px");
  protect("height", "34px");
  protect("margin", "0px");
  protect("padding", "0px");
  protect("border", "0px");
  protect("background", "transparent");
  protect("z-index", "2147483647");
  protect("pointer-events", "none");
  protect("opacity", "0");
  protect("transform-origin", "3px 2px");
  protect("transition", "opacity 80ms linear");

  const shadow = host.attachShadow({ mode: "closed" });
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 28");
  svg.setAttribute("width", "24");
  svg.setAttribute("height", "28");
  svg.setAttribute("aria-hidden", "true");
  Object.assign(svg.style, {
    display: "block",
    overflow: "visible",
    filter:
      "drop-shadow(0 1px 1px rgba(0,0,0,.42)) drop-shadow(0 0 3px rgba(124,58,237,.86)) drop-shadow(0 0 9px rgba(124,58,237,.52))",
  });

  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M3 2.25 20.15 14.1l-7.12 1.3 4.05 7.62-4.2 2.23-4.04-7.6-4.76 5.52L3 2.25Z");
  path.setAttribute("fill", "rgba(250,248,255,.92)");
  path.setAttribute("stroke", "#7c3aed");
  path.setAttribute("stroke-width", "1.45");
  path.setAttribute("stroke-linejoin", "round");
  svg.appendChild(path);
  const ring = document.createElementNS("http://www.w3.org/2000/svg", "circle");
  ring.setAttribute("cx", "10");
  ring.setAttribute("cy", "10");
  ring.setAttribute("r", "5.25");
  ring.setAttribute("fill", "rgba(250,248,255,.18)");
  ring.setAttribute("stroke-width", "1.65");
  ring.setAttribute("display", "none");
  svg.appendChild(ring);
  shadow.appendChild(svg);

  const defaults = Object.freeze({
    shape: "arrow",
    accent: "#7c3aed",
    glow: "soft",
    scale: 1,
    motion: "smooth",
  });
  let theme = { ...defaults };
  let position = null;
  let frame = 0;
  let fallbackTimer = 0;
  let finishPending = null;
  let pulseAnimation = null;

  const reducedMotion = () =>
    matchMedia("(prefers-reduced-motion: reduce)").matches;

  const validAccent = (value) =>
    typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value);

  const applyTheme = () => {
    const arrow = theme.shape === "arrow";
    path.setAttribute("display", arrow ? "block" : "none");
    ring.setAttribute("display", arrow ? "none" : "block");
    path.setAttribute("stroke", theme.accent);
    ring.setAttribute("stroke", theme.accent);
    const shadows = {
      none: "drop-shadow(0 1px 1px rgba(0,0,0,.42))",
      soft: `drop-shadow(0 1px 1px rgba(0,0,0,.42)) drop-shadow(0 0 2px ${theme.accent}bb) drop-shadow(0 0 6px ${theme.accent}44)`,
      strong: `drop-shadow(0 1px 1px rgba(0,0,0,.42)) drop-shadow(0 0 3px ${theme.accent}) drop-shadow(0 0 8px ${theme.accent}66)`,
    };
    svg.style.filter = shadows[theme.glow];
    svg.style.transform = `scale(${theme.scale})`;
    svg.style.transformOrigin = arrow ? "3px 2px" : "10px 10px";
    protect("transform-origin", arrow ? "3px 2px" : "10px 10px");
  };

  const configure = (options = {}) => {
    const next = { ...theme };
    if (options.shape === "arrow" || options.shape === "ring") {
      next.shape = options.shape;
    }
    if (validAccent(options.accent)) next.accent = options.accent;
    if (["none", "soft", "strong"].includes(options.glow)) {
      next.glow = options.glow;
    }
    const scale = Number(options.scale);
    if (Number.isFinite(scale) && scale >= 0.75 && scale <= 1.5) {
      next.scale = scale;
    }
    if (options.motion === "smooth" || options.motion === "direct") {
      next.motion = options.motion;
    }
    theme = next;
    applyTheme();
    return { ...theme };
  };

  const ensureAttached = (promote = false) => {
    if (!host.isConnected) {
      const parent = document.documentElement || document.body;
      if (parent) parent.appendChild(host);
    }
    if (typeof host.showPopover !== "function") return;
    try {
      if (promote && host.matches(":popover-open")) host.hidePopover();
      if (!host.matches(":popover-open")) host.showPopover();
    } catch {
      // A detached or unsupported document still gets the stacking fallback.
    }
  };

  const place = (x, y) => {
    position = { x, y };
    const hotspot = theme.shape === "arrow" ? { x: 3, y: 2 } : { x: 10, y: 10 };
    protect(
      "transform",
      `translate3d(${x - hotspot.x}px, ${y - hotspot.y}px, 0)`,
    );
  };

  const settlePending = () => {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    if (fallbackTimer) clearTimeout(fallbackTimer);
    fallbackTimer = 0;
    if (finishPending) finishPending();
    finishPending = null;
  };

  const moveTo = (rawX, rawY, quick = false) => {
    ensureAttached(true);
    const x = Number(rawX);
    const y = Number(rawY);
    if (!Number.isFinite(x) || !Number.isFinite(y)) return Promise.resolve();

    settlePending();
    protect("opacity", "1");
    protect("transition", reducedMotion() ? "none" : "opacity 80ms linear");
    if (!position || reducedMotion() || theme.motion === "direct") {
      place(x, y);
      return Promise.resolve();
    }

    const start = position;
    const dx = x - start.x;
    const dy = y - start.y;
    const distance = Math.hypot(dx, dy);
    if (distance < 2) {
      place(x, y);
      return Promise.resolve();
    }

    const duration = quick
      ? Math.min(120, Math.max(60, 35 + distance * 0.1))
      : Math.min(430, Math.max(150, 125 + distance * 0.16));
    const controlX = start.x + dx * 0.5;
    const controlY = start.y + dy * 0.5 - Math.min(quick ? 16 : 56, distance * 0.12);
    const startedAt = performance.now();

    return new Promise((resolve) => {
      finishPending = resolve;
      fallbackTimer = setTimeout(() => {
        if (!finishPending) return;
        place(x, y);
        settlePending();
      }, duration + 120);
      const tick = (now) => {
        const linear = Math.min(1, (now - startedAt) / duration);
        const t = linear * linear * linear * (linear * (linear * 6 - 15) + 10);
        const inverse = 1 - t;
        place(
          inverse * inverse * start.x + 2 * inverse * t * controlX + t * t * x,
          inverse * inverse * start.y + 2 * inverse * t * controlY + t * t * y,
        );

        if (linear < 1) {
          frame = requestAnimationFrame(tick);
          return;
        }

        frame = 0;
        finishPending = null;
        clearTimeout(fallbackTimer);
        fallbackTimer = 0;
        resolve();
      };
      frame = requestAnimationFrame(tick);
    });
  };

  const placeAt = (rawX, rawY) => {
    ensureAttached(true);
    const x = Number(rawX);
    const y = Number(rawY);
    if (!Number.isFinite(x) || !Number.isFinite(y)) return;
    settlePending();
    protect("transition", reducedMotion() ? "none" : "opacity 80ms linear");
    protect("opacity", "1");
    place(x, y);
  };

  const pulse = () => {
    ensureAttached();
    pulseAnimation?.cancel();
    pulseAnimation = null;
    if (reducedMotion()) return;
    pulseAnimation = host.animate(
      [
        { scale: "1", offset: 0 },
        { scale: ".88", offset: 0.38 },
        { scale: "1.04", offset: 0.72 },
        { scale: "1", offset: 1 },
      ],
      { duration: 180, easing: "cubic-bezier(.2,.8,.2,1)" },
    );
  };

  const hide = () => {
    settlePending();
    protect("transition", reducedMotion() ? "none" : "opacity 80ms linear");
    protect("opacity", "0");
  };

  const api = Object.freeze({
    version: 1,
    configure,
    moveTo,
    placeAt,
    pulse,
    hide,
    get position() {
      return position ? { ...position } : null;
    },
  });
  Object.defineProperty(globalThis, key, {
    value: api,
    configurable: false,
  });

  if (!document.documentElement) {
    document.addEventListener("DOMContentLoaded", ensureAttached, { once: true });
  }
  return api;
})()
