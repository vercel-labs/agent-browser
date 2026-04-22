//! Browser-side evaluation scripts for React/web introspection.
//!
//! These are JavaScript strings evaluated in the page context via
//! `Runtime.evaluate`. They assume the React DevTools hook is already
//! installed (via `--enable react-devtools`) except for `VITALS_INIT` and
//! `PUSHSTATE`, which only use standard Web APIs.
//!
//! Kept as raw strings rather than TS/JS files because the daemon is a single
//! Rust binary with no filesystem vendor step at runtime.

/// Build a no-argument async IIFE page-eval that returns the component tree as
/// JSON.
pub const TREE_SNAPSHOT: &str = r#"
(async () => {
  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!hook) throw new Error("React DevTools hook not installed - relaunch with --enable react-devtools");
  const ri = hook.rendererInterfaces && hook.rendererInterfaces.get && hook.rendererInterfaces.get(1);
  if (!ri) throw new Error("No React renderer attached - the page has not booted React yet");

  const batches = await new Promise((resolve) => {
    const out = [];
    const origEmit = hook.emit;
    hook.emit = function (event, payload) {
      if (event === "operations") out.push(Array.from(payload));
      return origEmit.apply(hook, arguments);
    };
    ri.flushInitialOperations();
    setTimeout(() => {
      hook.emit = origEmit;
      resolve(out);
    }, 50);
  });

  const nodes = batches.flatMap((ops) => {
    let i = 2;
    const strings = [null];
    const tableEnd = ++i + ops[i - 1];
    while (i < tableEnd) {
      const len = ops[i++];
      strings.push(String.fromCodePoint(...ops.slice(i, i + len)));
      i += len;
    }
    const out = [];
    while (i < ops.length) {
      const op = ops[i];
      if (op === 1) {
        const id = ops[i + 1];
        const type = ops[i + 2];
        i += 3;
        if (type === 11) {
          out.push({ id, type, name: null, key: null, parent: 0 });
          i += 4;
        } else {
          out.push({
            id,
            type,
            name: strings[ops[i + 2]] || null,
            key: strings[ops[i + 3]] || null,
            parent: ops[i],
          });
          i += 5;
        }
      } else {
        i += skip(op, ops, i);
      }
    }
    return out;

    function skip(op, ops, i) {
      if (op === 2) return 2 + ops[i + 1];
      if (op === 3) return 3 + ops[i + 2];
      if (op === 4) return 3;
      if (op === 5) return 4;
      if (op === 6) return 1;
      if (op === 7) return 3;
      if (op === 8) return 6 + rects(ops[i + 5]);
      if (op === 9) return 2 + ops[i + 1];
      if (op === 10) return 3 + ops[i + 2];
      if (op === 11) return 3 + rects(ops[i + 2]);
      if (op === 12) return suspenders(ops, i);
      if (op === 13) return 2;
      return 1;
    }
    function rects(n) {
      return n === -1 ? 0 : n * 4;
    }
    function suspenders(ops, i) {
      let j = i + 2;
      for (let c = 0; c < ops[i + 1]; c++) j += 5 + ops[j + 4];
      return j - i;
    }
  });

  return JSON.stringify(nodes);
})()
"#;

/// Template for `inspect` — replace {{ID}} with the numeric fiber id.
pub const TREE_INSPECT: &str = r#"
(() => {
  const id = {{ID}};
  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  const ri = hook && hook.rendererInterfaces && hook.rendererInterfaces.get && hook.rendererInterfaces.get(1);
  if (!ri) throw new Error("No React renderer attached");
  if (!ri.hasElementWithId(id)) throw new Error("element " + id + " not found (page reloaded?)");
  const result = ri.inspectElement(1, id, null, true);
  if (!result || result.type !== "full-data") {
    throw new Error("inspect failed: " + (result && result.type));
  }
  const v = result.value;
  const name = ri.getDisplayNameForElementID(id);
  const lines = [name + " #" + id];
  if (v.key != null) lines.push("key: " + JSON.stringify(v.key));
  section("props", v.props);
  section("hooks", v.hooks);
  section("state", v.state);
  section("context", v.context);
  if (v.owners && v.owners.length) {
    lines.push("rendered by: " + v.owners.map((o) => o.displayName).join(" > "));
  }
  const source = Array.isArray(v.source)
    ? [v.source[1], v.source[2], v.source[3]]
    : null;
  return JSON.stringify({ text: lines.join("\n"), source });

  function section(label, payload) {
    const data = (payload && payload.data) || payload;
    if (data == null) return;
    if (Array.isArray(data)) {
      if (data.length === 0) return;
      lines.push(label + ":");
      for (const h of data) lines.push("  " + hookLine(h));
    } else if (typeof data === "object") {
      const entries = Object.entries(data);
      if (entries.length === 0) return;
      lines.push(label + ":");
      for (const [k, val] of entries) lines.push("  " + k + ": " + preview(val));
    }
  }
  function hookLine(h) {
    const idx = h.id != null ? "[" + h.id + "] " : "";
    const sub = h.subHooks && h.subHooks.length ? " (" + h.subHooks.length + " sub)" : "";
    return idx + h.name + ": " + preview(h.value) + sub;
  }
  function preview(v) {
    if (v == null) return String(v);
    if (typeof v !== "object") return JSON.stringify(v);
    if (v.type === "undefined") return "undefined";
    if (v.preview_long) return v.preview_long;
    if (v.preview_short) return v.preview_short;
    if (Array.isArray(v)) return "[" + v.map(preview).join(", ") + "]";
    const entries = Object.entries(v).map((e) => e[0] + ": " + preview(e[1]));
    return "{" + entries.join(", ") + "}";
  }
})()
"#;

/// Fiber profiler init script. Registered via `addScriptToEvaluateOnNewDocument`
/// so it survives navigations; also evaluated immediately on the current page
/// by `react renders start`.
pub const RENDERS_INIT: &str = r#"
(() => {
  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!hook || window.__AB_RENDERS_ACTIVE__) return;

  const MAX_COMPONENTS = 200;
  const data = {};
  const fps = { frames: [], last: 0, rafId: 0 };

  window.__AB_RENDERS__ = data;
  window.__AB_RENDERS_FPS__ = fps;
  window.__AB_RENDERS_START__ = performance.now();
  window.__AB_RENDERS_ACTIVE__ = true;

  function fpsLoop(now) {
    if (fps.last > 0) fps.frames.push(now - fps.last);
    fps.last = now;
    fps.rafId = requestAnimationFrame(fpsLoop);
  }
  fps.rafId = requestAnimationFrame(fpsLoop);

  const origOnCommit = hook.onCommitFiberRoot;
  window.__AB_RENDERS_ORIG_COMMIT__ = origOnCommit;

  hook.onCommitFiberRoot = function (rendererID, root) {
    try { walkFiber(root.current); } catch {}
    if (typeof origOnCommit === "function") {
      return origOnCommit.apply(hook, arguments);
    }
  };

  function getName(fiber) {
    if (!fiber.type || typeof fiber.type === "string") return null;
    return fiber.type.displayName || fiber.type.name || null;
  }

  function brief(val) {
    if (val === undefined) return "undefined";
    if (val === null) return "null";
    if (typeof val === "function") return "fn()";
    if (typeof val === "string") return val.length > 60 ? '"' + val.slice(0, 57) + '..."' : '"' + val + '"';
    if (typeof val === "number" || typeof val === "boolean") return String(val);
    if (Array.isArray(val)) return "Array(" + val.length + ")";
    if (typeof val === "object") {
      try {
        const keys = Object.keys(val);
        return keys.length <= 3 ? "{" + keys.join(", ") + "}" : "{" + keys.slice(0, 3).join(", ") + ", ...}";
      } catch { return "{...}"; }
    }
    return String(val).slice(0, 40);
  }

  function getChanges(fiber) {
    const changes = [];
    const alt = fiber.alternate;
    if (!alt) { changes.push({ type: "mount" }); return changes; }
    if (fiber.memoizedProps !== alt.memoizedProps) {
      const curr = fiber.memoizedProps || {};
      const prev = alt.memoizedProps || {};
      const allKeys = new Set([...Object.keys(curr), ...Object.keys(prev)]);
      for (const k of allKeys) {
        if (k !== "children" && curr[k] !== prev[k]) {
          changes.push({ type: "props", name: k, prev: brief(prev[k]), next: brief(curr[k]) });
        }
      }
    }
    if (fiber.memoizedState !== alt.memoizedState) {
      let curr = fiber.memoizedState;
      let prev = alt.memoizedState;
      let hookIdx = 0;
      while (curr || prev) {
        if ((curr && curr.memoizedState) !== (prev && prev.memoizedState)) {
          changes.push({
            type: "state",
            name: "hook #" + hookIdx,
            prev: brief(prev && prev.memoizedState),
            next: brief(curr && curr.memoizedState),
          });
        }
        curr = curr && curr.next;
        prev = prev && prev.next;
        hookIdx++;
      }
    }
    if (fiber.dependencies && fiber.dependencies.firstContext) {
      let ctx = fiber.dependencies.firstContext;
      let altCtx = alt.dependencies && alt.dependencies.firstContext;
      while (ctx) {
        if (!altCtx || ctx.memoizedValue !== (altCtx && altCtx.memoizedValue)) {
          const ctxName =
            (ctx.context && ctx.context.displayName) ||
            (ctx.context && ctx.context.Provider && ctx.context.Provider.displayName) ||
            "unknown";
          changes.push({
            type: "context",
            name: ctxName,
            prev: brief(altCtx && altCtx.memoizedValue),
            next: brief(ctx.memoizedValue),
          });
        }
        ctx = ctx.next;
        altCtx = altCtx && altCtx.next;
      }
    }
    if (changes.length === 0) {
      let parent = fiber.return;
      while (parent) {
        const pName = getName(parent);
        if (pName) {
          const suffix = !parent.alternate ? " (mount)" : "";
          changes.push({ type: "parent", name: pName + suffix });
          break;
        }
        parent = parent.return;
      }
      if (changes.length === 0) changes.push({ type: "parent", name: "unknown" });
    }
    return changes;
  }

  function childrenTime(fiber) {
    let t = 0;
    let child = fiber.child;
    while (child) {
      if (typeof child.actualDuration === "number") t += child.actualDuration;
      child = child.sibling;
    }
    return t;
  }

  function hasDomMutation(fiber) {
    if (!fiber.alternate) return true;
    let child = fiber.child;
    while (child) {
      if (typeof child.type === "string" && (child.flags & 6) > 0) return true;
      child = child.sibling;
    }
    return false;
  }

  function walkFiber(fiber) {
    if (!fiber) return;
    const tag = fiber.tag;
    if (tag === 0 || tag === 1 || tag === 2 || tag === 11 || tag === 15) {
      const didRender =
        fiber.alternate === null ||
        fiber.flags > 0 ||
        fiber.memoizedProps !== (fiber.alternate && fiber.alternate.memoizedProps) ||
        fiber.memoizedState !== (fiber.alternate && fiber.alternate.memoizedState);
      if (didRender) {
        const name = getName(fiber);
        if (name) {
          if (!(name in data) && Object.keys(data).length >= MAX_COMPONENTS) {
            // at cap - skip
          } else {
            if (!data[name]) {
              data[name] = {
                count: 0, mounts: 0, totalTime: 0, selfTime: 0,
                domMutations: 0, changes: [], _instances: new Set(),
              };
            }
            data[name].count++;
            if (!fiber.alternate) data[name].mounts++;
            if (!data[name]._instances.has(fiber)) {
              data[name]._instances.add(fiber);
              if (fiber.alternate) data[name]._instances.add(fiber.alternate);
            }
            if (typeof fiber.actualDuration === "number") {
              data[name].totalTime += fiber.actualDuration;
              data[name].selfTime += Math.max(0, fiber.actualDuration - childrenTime(fiber));
            }
            if (hasDomMutation(fiber)) data[name].domMutations++;
            const ch = getChanges(fiber);
            for (const c of ch) {
              if (data[name].changes.length < 50) data[name].changes.push(c);
            }
          }
        }
      }
    }
    walkFiber(fiber.child);
    walkFiber(fiber.sibling);
  }
})()
"#;

/// Stop script for fiber profiler. Returns the collected profile as JSON.
pub const RENDERS_STOP: &str = r#"
(() => {
  const active = window.__AB_RENDERS_ACTIVE__;
  if (!active) throw new Error("renders recording not active - run `react renders start` first");

  const data = window.__AB_RENDERS__;
  const startTime = window.__AB_RENDERS_START__;
  const elapsed = performance.now() - startTime;

  const fpsData = window.__AB_RENDERS_FPS__;
  let fpsStats = { avg: 0, min: 0, max: 0, drops: 0 };
  if (fpsData) {
    cancelAnimationFrame(fpsData.rafId);
    if (fpsData.frames.length > 0) {
      const fpsSamples = fpsData.frames.map((dt) => (dt > 0 ? 1000 / dt : 0));
      const sum = fpsSamples.reduce((a, b) => a + b, 0);
      fpsStats = {
        avg: Math.round(sum / fpsSamples.length),
        min: Math.round(Math.min(...fpsSamples)),
        max: Math.round(Math.max(...fpsSamples)),
        drops: fpsSamples.filter((f) => f < 30).length,
      };
    }
  }

  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  const orig = window.__AB_RENDERS_ORIG_COMMIT__;
  if (hook) hook.onCommitFiberRoot = orig || undefined;

  delete window.__AB_RENDERS__;
  delete window.__AB_RENDERS_START__;
  delete window.__AB_RENDERS_ACTIVE__;
  delete window.__AB_RENDERS_ORIG_COMMIT__;
  delete window.__AB_RENDERS_FPS__;

  if (!data) {
    return JSON.stringify({
      elapsed: 0, fps: fpsStats, totalRenders: 0, totalMounts: 0,
      totalReRenders: 0, totalComponents: 0, components: [],
    });
  }

  const round = (n) => Math.round(n * 100) / 100;
  const components = Object.entries(data)
    .map(([name, entry]) => {
      const summary = {};
      for (const c of entry.changes) {
        const key = c.type === "props" ? "props." + c.name
          : c.type === "state" ? "state (" + c.name + ")"
          : c.type === "context" ? "context (" + c.name + ")"
          : c.type === "parent" ? "parent (" + c.name + ")"
          : c.type;
        summary[key] = (summary[key] || 0) + 1;
      }
      return {
        name,
        count: entry.count,
        mounts: entry.mounts,
        reRenders: entry.count - entry.mounts,
        instanceCount: entry._instances.size,
        totalTime: round(entry.totalTime),
        selfTime: round(entry.selfTime),
        domMutations: entry.domMutations,
        changes: entry.changes,
        changeSummary: summary,
      };
    })
    .sort((a, b) => b.totalTime - a.totalTime || b.count - a.count);

  return JSON.stringify({
    elapsed: round(elapsed / 1000),
    fps: fpsStats,
    totalRenders: components.reduce((s, c) => s + c.count, 0),
    totalMounts: components.reduce((s, c) => s + c.mounts, 0),
    totalReRenders: components.reduce((s, c) => s + c.reRenders, 0),
    totalComponents: components.length,
    components,
  });
})()
"#;

/// Suspense boundary walker. Returns boundaries with suspendedBy metadata as JSON.
pub const SUSPENSE_WALK: &str = r#"
(async () => {
  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!hook) throw new Error("React DevTools hook not installed - relaunch with --enable react-devtools");
  const ri = hook.rendererInterfaces && hook.rendererInterfaces.get && hook.rendererInterfaces.get(1);
  if (!ri) throw new Error("No React renderer attached");

  const batches = await new Promise((resolve) => {
    const out = [];
    const origEmit = hook.emit;
    hook.emit = function (event, payload) {
      if (event === "operations") out.push(payload);
      return origEmit.apply(this, arguments);
    };
    ri.flushInitialOperations();
    setTimeout(() => {
      hook.emit = origEmit;
      resolve(out);
    }, 50);
  });

  const boundaryMap = new Map();
  for (const ops of batches) decodeSuspenseOps(ops, boundaryMap);

  const results = [];
  for (const b of boundaryMap.values()) {
    if (b.parentID === 0) continue;
    const boundary = {
      id: b.id,
      parentID: b.parentID,
      name: b.name,
      isSuspended: b.isSuspended,
      environments: b.environments,
      suspendedBy: [],
      unknownSuspenders: null,
      owners: [],
      jsxSource: null,
    };
    if (ri.hasElementWithId(b.id)) {
      const displayName = ri.getDisplayNameForElementID(b.id);
      if (displayName) boundary.name = displayName;
      const result = ri.inspectElement(1, b.id, null, true);
      if (result && result.type === "full-data") {
        parseInspection(boundary, result.value);
      }
    }
    results.push(boundary);
  }
  return JSON.stringify(results);

  function decodeSuspenseOps(ops, map) {
    let i = 2;
    const strings = [null];
    const tableEnd = ++i + ops[i - 1];
    while (i < tableEnd) {
      const len = ops[i++];
      strings.push(String.fromCodePoint(...ops.slice(i, i + len)));
      i += len;
    }
    while (i < ops.length) {
      const op = ops[i];
      if (op === 1) {
        const type = ops[i + 2];
        i += 3 + (type === 11 ? 4 : 5);
      } else if (op === 2) {
        i += 2 + ops[i + 1];
      } else if (op === 3) {
        i += 3 + ops[i + 2];
      } else if (op === 4) {
        i += 3;
      } else if (op === 5) {
        i += 4;
      } else if (op === 6) {
        i++;
      } else if (op === 7) {
        i += 3;
      } else if (op === 8) {
        const id = ops[i + 1];
        const parentID = ops[i + 2];
        const nameStrID = ops[i + 3];
        const isSuspended = ops[i + 4] === 1;
        const numRects = ops[i + 5];
        i += 6;
        if (numRects !== -1) i += numRects * 4;
        map.set(id, { id, parentID, name: strings[nameStrID] || null, isSuspended, environments: [] });
      } else if (op === 9) {
        i += 2 + ops[i + 1];
      } else if (op === 10) {
        i += 3 + ops[i + 2];
      } else if (op === 11) {
        const numRects = ops[i + 2];
        i += 3;
        if (numRects !== -1) i += numRects * 4;
      } else if (op === 12) {
        i++;
        const changeLen = ops[i++];
        for (let c = 0; c < changeLen; c++) {
          const id = ops[i++];
          i++;
          i++;
          const isSuspended = ops[i++] === 1;
          const envLen = ops[i++];
          const envs = [];
          for (let e = 0; e < envLen; e++) {
            const n = strings[ops[i++]];
            if (n != null) envs.push(n);
          }
          const node = map.get(id);
          if (node) {
            node.isSuspended = isSuspended;
            for (const env of envs) {
              if (!node.environments.includes(env)) node.environments.push(env);
            }
          }
        }
      } else if (op === 13) {
        i += 2;
      } else {
        i++;
      }
    }
  }

  function parseInspection(boundary, data) {
    const rawSuspendedBy = data.suspendedBy;
    const rawSuspenders = Array.isArray(rawSuspendedBy)
      ? rawSuspendedBy
      : rawSuspendedBy && Array.isArray(rawSuspendedBy.data) ? rawSuspendedBy.data : null;
    if (rawSuspenders) {
      for (const entry of rawSuspenders) {
        const awaited = entry && entry.awaited;
        if (!awaited) continue;
        const desc = preview(awaited.description) || preview(awaited.value);
        boundary.suspendedBy.push({
          name: awaited.name || "unknown",
          description: desc,
          duration: awaited.end && awaited.start ? Math.round(awaited.end - awaited.start) : 0,
          env: awaited.env || (entry && entry.env) || null,
          ownerName: (awaited.owner && awaited.owner.displayName) || null,
          ownerStack: parseStack((awaited.owner && awaited.owner.stack) || awaited.stack),
          awaiterName: (entry && entry.owner && entry.owner.displayName) || null,
          awaiterStack: parseStack((entry && entry.owner && entry.owner.stack) || (entry && entry.stack)),
        });
      }
    }
    if (data.unknownSuspenders && data.unknownSuspenders !== 0) {
      const reasons = {
        1: "production build (no debug info)",
        2: "old React version (missing tracking)",
        3: "thrown Promise (library using throw instead of use())",
      };
      boundary.unknownSuspenders = reasons[data.unknownSuspenders] || "unknown reason";
    }
    if (Array.isArray(data.owners)) {
      for (const o of data.owners) {
        if (o && o.displayName) {
          const src = Array.isArray(o.stack) && o.stack.length > 0 && Array.isArray(o.stack[0])
            ? [o.stack[0][1] || "(unknown)", o.stack[0][2], o.stack[0][3]]
            : null;
          boundary.owners.push({ name: o.displayName, env: o.env || null, source: src });
        }
      }
    }
    if (Array.isArray(data.stack) && data.stack.length > 0) {
      const frame = data.stack[0];
      if (Array.isArray(frame) && frame.length >= 4) {
        boundary.jsxSource = [frame[1] || "(unknown)", frame[2], frame[3]];
      }
    }
  }

  function parseStack(raw) {
    if (!Array.isArray(raw) || raw.length === 0) return null;
    return raw
      .filter((f) => Array.isArray(f) && f.length >= 4)
      .map((f) => [f[0] || "", f[1] || "", f[2] || 0, f[3] || 0]);
  }

  function preview(v) {
    if (v == null) return "";
    if (typeof v === "string") return v;
    if (typeof v !== "object") return String(v);
    if (typeof v.preview_long === "string") return v.preview_long;
    if (typeof v.preview_short === "string") return v.preview_short;
    if (typeof v.value === "string") return v.value;
    try {
      const s = JSON.stringify(v);
      return s.length > 80 ? s.slice(0, 77) + "..." : s;
    } catch {
      return "";
    }
  }
})()
"#;

/// Init script for Core Web Vitals + React hydration timing capture. Installs
/// PerformanceObservers for LCP/CLS and intercepts `console.timeStamp` to
/// capture React's profiling reconciler timings. Idempotent.
pub const VITALS_INIT: &str = r#"
(() => {
  if (window.__AB_VITALS_INSTALLED__) return;
  window.__AB_VITALS_INSTALLED__ = true;

  const cwv = { lcp: null, cls: 0, clsEntries: [], fcp: null, inp: null };
  window.__AB_VITALS__ = cwv;

  try {
    new PerformanceObserver((list) => {
      const entries = list.getEntries();
      if (entries.length > 0) {
        const last = entries[entries.length - 1];
        cwv.lcp = {
          startTime: Math.round(last.startTime * 100) / 100,
          size: last.size,
          element: last.element && last.element.tagName ? last.element.tagName.toLowerCase() : null,
          url: last.url || null,
        };
      }
    }).observe({ type: "largest-contentful-paint", buffered: true });
  } catch {}

  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (!entry.hadRecentInput) {
          cwv.cls += entry.value;
          cwv.clsEntries.push({
            value: Math.round(entry.value * 10000) / 10000,
            startTime: Math.round(entry.startTime * 100) / 100,
          });
        }
      }
    }).observe({ type: "layout-shift", buffered: true });
  } catch {}

  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.name === "first-contentful-paint") {
          cwv.fcp = Math.round(entry.startTime * 100) / 100;
        }
      }
    }).observe({ type: "paint", buffered: true });
  } catch {}

  try {
    new PerformanceObserver((list) => {
      let worst = cwv.inp || 0;
      for (const entry of list.getEntries()) {
        if (entry.duration > worst) worst = entry.duration;
      }
      if (worst > 0) cwv.inp = Math.round(worst * 100) / 100;
    }).observe({ type: "event", buffered: true, durationThreshold: 40 });
  } catch {}

  // React profiling build emits console.timeStamp(label, start, end, track, trackGroup, color)
  // for reconciler phases and per-component hydration timing. Intercept and collect.
  const timing = [];
  window.__AB_REACT_TIMING__ = timing;
  const orig = console.timeStamp;
  console.timeStamp = function (label) {
    const args = arguments;
    if (typeof label === "string" && args.length >= 3 && typeof args[1] === "number") {
      timing.push({
        label,
        startTime: args[1],
        endTime: args[2],
        track: args[3] || "",
        trackGroup: args[4] || "",
        color: args[5] || "",
      });
    }
    return orig.apply(console, args);
  };
})()
"#;

/// Read script for vitals — collects observed metrics plus Navigation Timing
/// TTFB and any React hydration phases. Returns JSON.
pub const VITALS_READ: &str = r#"
(() => {
  const cwv = window.__AB_VITALS__ || {};
  const timing = window.__AB_REACT_TIMING__ || [];
  const nav = performance.getEntriesByType("navigation")[0];
  const ttfb = nav
    ? Math.round((nav.responseStart - nav.requestStart) * 100) / 100
    : null;
  return JSON.stringify({ cwv, timing, ttfb });
})()
"#;

/// SPA client-side navigation. Tries the framework router first so Next.js
/// app/pages router triggers an RSC fetch (pure `history.pushState` would
/// be shallow routing and bypass data loading). Falls back to
/// `history.pushState` + popstate/navigate events for vanilla pages and
/// routers that listen to history events (React Router, TanStack Router,
/// Solid Router, Vue Router).
pub const PUSHSTATE: &str = r#"
((url) => {
  const before = location.href;
  const absolute = new URL(url, before).href;
  if (absolute === before) return before;

  // Next.js pages + app router expose window.next.router with a `push`
  // method that triggers the RSC fetch and re-render pipeline.
  const r = typeof window.next === "object" && window.next && window.next.router;
  if (r && typeof r.push === "function") {
    try { r.push(url); return location.href; } catch {}
  }

  history.pushState(null, "", absolute);
  try { dispatchEvent(new PopStateEvent("popstate", { state: null })); } catch {}
  try { dispatchEvent(new Event("navigate")); } catch {}
  return location.href;
})({{URL}})
"#;
