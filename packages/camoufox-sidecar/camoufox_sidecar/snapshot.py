"""Accessibility snapshot for the Camoufox sidecar.

Playwright's ``page.accessibility.snapshot()`` gives us a Firefox-side AX tree,
but it doesn't return ``ElementHandle`` instances, so we can't use it to drive
subsequent ``click``/``fill``/``gettext`` commands.

Instead we run a single ``page.evaluate`` that:

    * walks the DOM;
    * classifies each element using a minimal ARIA role mapping that mirrors
      what Chrome's AX tree produces for the same markup;
    * tags every ref-worthy element with ``data-__ab-ref="eN"``;
    * returns one metadata row per tagged element.

The Python side then resolves each row back to an ``ElementHandle`` via
``page.query_selector("[data-__ab-ref='eN']")`` and populates the
:class:`~camoufox_sidecar.refs.RefCache`. Future ``click``/``fill`` calls by
``@eN`` pull the handle out of the cache; by CSS selector they hit
``page.locator(selector)`` directly.

The public contract is the same JSON shape the Chrome path emits on
``{"success": true, "data": { snapshot, origin, refs }}``: a preformatted text
tree, a navigation origin, and a ``{ref: {role, name}}`` map for agent
consumption. Parity is measured at the ``refs`` level — exact text matches
aren't expected because Firefox's AX tree differs structurally from Chrome's.
"""

from __future__ import annotations

from typing import Any, Optional

from .refs import RefCache


# Interactive roles that always get a ref. Mirrors the Chrome-path
# ``INTERACTIVE_ROLES`` list in ``cli/src/native/snapshot.rs`` so the sidecar
# emits the same agent-facing role names Chrome does.
INTERACTIVE_ROLES: frozenset[str] = frozenset(
    {
        "button",
        "link",
        "textbox",
        "checkbox",
        "radio",
        "combobox",
        "listbox",
        "menuitem",
        "menuitemcheckbox",
        "menuitemradio",
        "option",
        "searchbox",
        "slider",
        "spinbutton",
        "switch",
        "tab",
        "treeitem",
    }
)

# Content roles that get a ref only when they carry a non-empty accessible name.
# Chrome includes ``heading`` + several landmarks here; v1 keeps the list small
# so parity against Firefox is tractable. The list intentionally does *not*
# include ``generic`` / ``group`` — those produce noise without names.
CONTENT_ROLES_WITH_NAMES: frozenset[str] = frozenset(
    {
        "heading",
        "cell",
        "gridcell",
        "columnheader",
        "rowheader",
        "listitem",
        "article",
        "region",
        "main",
        "navigation",
    }
)


# Executed inside the browser context. Returns a list of metadata dicts, one
# per ref-worthy element. The element retains a ``data-__ab-ref`` attribute so
# Python can re-resolve an ``ElementHandle`` for each ref via
# ``page.query_selector("[data-__ab-ref='eN']")``. The attribute is deliberately
# left in place until the next snapshot — Playwright ``Locator`` objects built
# from a ref selector stay valid as long as the page doesn't mutate the
# attribute away, and any mutation (navigation, innerHTML overwrite) is covered
# by the ``framenavigated`` invalidation.
_SNAPSHOT_JS = r"""
(({ interactiveRoles, contentRolesWithNames }) => {
  const IMPLICIT_ROLES = {
    'a': 'link',
    'button': 'button',
    'select': 'combobox',
    'textarea': 'textbox',
    'h1': 'heading', 'h2': 'heading', 'h3': 'heading', 'h4': 'heading',
    'h5': 'heading', 'h6': 'heading',
    'nav': 'navigation',
    'main': 'main',
    'article': 'article',
    'li': 'listitem',
  };
  const INTERACTIVE = new Set(interactiveRoles);
  const CONTENT_WITH_NAMES = new Set(contentRolesWithNames);

  const roleFor = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit.trim().toLowerCase();
    const tag = el.tagName.toLowerCase();
    if (tag === 'a') return el.hasAttribute('href') ? 'link' : null;
    if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'button' || t === 'submit' || t === 'reset') return 'button';
      if (t === 'range') return 'slider';
      if (t === 'search') return 'searchbox';
      if (t === 'number') return 'spinbutton';
      if (t === 'hidden' || t === 'file') return null;
      return 'textbox';
    }
    return IMPLICIT_ROLES[tag] || null;
  };

  const stripRefAttr = (node) => {
    // Clone and strip our own marker attribute plus any nested inputs so the
    // wrapping <label> doesn't pick up the input's current value as its name.
    const clone = node.cloneNode(true);
    clone.querySelectorAll('input, textarea, select').forEach((n) => n.remove());
    return (clone.textContent || '').replace(/\s+/g, ' ').trim();
  };

  const nameFor = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const labelledby = el.getAttribute('aria-labelledby');
    if (labelledby) {
      const refEl = document.getElementById(labelledby);
      if (refEl) return (refEl.textContent || '').replace(/\s+/g, ' ').trim();
    }
    if (el.id) {
      const forLabel = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
      if (forLabel) return stripRefAttr(forLabel);
    }
    const wrappingLabel = el.closest('label');
    if (wrappingLabel && wrappingLabel !== el) return stripRefAttr(wrappingLabel);
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
      if (el.placeholder) return el.placeholder.trim();
      if (el.value) return String(el.value).trim();
      return '';
    }
    return (el.textContent || '').replace(/\s+/g, ' ').trim();
  };

  const levelFor = (el) => {
    if (/^H[1-6]$/.test(el.tagName)) return parseInt(el.tagName.slice(1), 10);
    const explicit = el.getAttribute('aria-level');
    if (explicit) {
      const n = parseInt(explicit, 10);
      if (!Number.isNaN(n)) return n;
    }
    return null;
  };

  // Clear previous ref markers so nth call produces deterministic results.
  document.querySelectorAll('[data-__ab-ref]').forEach((n) => n.removeAttribute('data-__ab-ref'));

  const results = [];
  let counter = 0;
  const all = document.querySelectorAll('*');
  for (const el of all) {
    const role = roleFor(el);
    if (!role) continue;
    const isInteractive = INTERACTIVE.has(role);
    const isContent = CONTENT_WITH_NAMES.has(role);
    if (!isInteractive && !isContent) continue;

    // Skip zero-size/hidden elements (parity with Chrome's snapshot path,
    // which filters inert elements out of the AX tree).
    const style = el.ownerDocument.defaultView.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    const rect = el.getBoundingClientRect();
    if (isInteractive && (rect.width === 0 || rect.height === 0)) {
      // Off-screen invisible inputs (display:none covered above, but zero-size
      // via CSS is still possible). Skip to keep the ref list honest.
      continue;
    }

    const name = nameFor(el);
    if (isContent && !name) continue;

    counter += 1;
    const ref = 'e' + counter;
    el.setAttribute('data-__ab-ref', ref);

    const attrs = {};
    if (role === 'heading') {
      const level = levelFor(el);
      if (level !== null) attrs.level = level;
    }
    if (role === 'checkbox' || role === 'radio') {
      attrs.checked = !!el.checked;
    }
    if (el.hasAttribute('disabled')) attrs.disabled = true;
    if (el.hasAttribute('required')) attrs.required = true;

    results.push({
      ref,
      role,
      name,
      tag: el.tagName.toLowerCase(),
      attrs,
    });
  }
  return results;
})
"""


def _format_line(entry: dict[str, Any]) -> str:
    """Render a single ref entry in the agent-browser text-tree shape.

    The Chrome path emits multi-level indented output; the sidecar produces a
    flat list instead because Playwright/Firefox's AX tree is structurally
    different from Chrome's and forcing pseudo-indentation adds noise without
    improving agent comprehension. The parity test compares the ``refs`` map,
    not the rendered text.
    """
    name = entry.get("name") or ""
    attrs: dict[str, Any] = entry.get("attrs") or {}
    attr_bits: list[str] = []
    if "level" in attrs:
        attr_bits.append(f"level={attrs['level']}")
    if "checked" in attrs:
        attr_bits.append(f"checked={'true' if attrs['checked'] else 'false'}")
    if attrs.get("disabled"):
        attr_bits.append("disabled")
    if attrs.get("required"):
        attr_bits.append("required")
    attr_bits.append(f"ref={entry['ref']}")

    # JSON-encoded name, matching the Chrome path (cli/src/native/snapshot.rs
    # render_tree uses serde_json::to_string on the display name).
    import json

    name_fragment = f" {json.dumps(name)}" if name else ""
    return f"- {entry['role']}{name_fragment} [{', '.join(attr_bits)}]"


async def take_snapshot(
    page: Any,
    ref_cache: RefCache,
    *,
    interactive_only: bool = False,
    selector: Optional[str] = None,
) -> dict[str, Any]:
    """Snapshot ``page`` and repopulate ``ref_cache`` from scratch.

    Returns the ``{snapshot, origin, refs}`` shape the Rust side relays
    verbatim to the CLI consumer, so the sidecar is authoritative for
    agent-facing wording/formatting on the Camoufox engine.
    """
    ref_cache.invalidate()

    if selector:
        # Scope the walker to the subtree rooted at ``selector``. We inject a
        # marker attribute on the root and have the JS restrict ``all`` to its
        # descendants. This avoids evaluating a second JS function across the
        # whole tree.
        scope_root = await page.query_selector(selector)
        if scope_root is None:
            raise SnapshotError(
                "selector-not-found",
                f"Selector {selector!r} did not match any element",
            )
        entries = await scope_root.evaluate(
            f"(root) => ({_SNAPSHOT_JS})({{ interactiveRoles: {_role_list(INTERACTIVE_ROLES)}, contentRolesWithNames: {_role_list(CONTENT_ROLES_WITH_NAMES)} }})",
        )
    else:
        entries = await page.evaluate(
            f"() => ({_SNAPSHOT_JS})({{ interactiveRoles: {_role_list(INTERACTIVE_ROLES)}, contentRolesWithNames: {_role_list(CONTENT_ROLES_WITH_NAMES)} }})",
        )

    if interactive_only:
        entries = [e for e in entries if e.get("role") in INTERACTIVE_ROLES]
        # Re-assign refs so the agent-facing numbering stays contiguous.
        for new_idx, entry in enumerate(entries, start=1):
            entry["ref"] = f"e{new_idx}"

    # Resolve each ref back to a live ElementHandle so subsequent click/fill
    # calls can reach the element without re-running the JS walker.
    for entry in entries:
        ref_id = entry["ref"]
        handle = await page.query_selector(f"[data-__ab-ref='{ref_id}']")
        if handle is None:
            # Element vanished between the walker and this query_selector —
            # extremely rare but possible under a script that re-renders
            # synchronously. Drop the entry silently rather than emit a
            # dangling ref to the agent.
            continue
        ref_cache.put(ref_id, handle, role=entry["role"], name=entry["name"])

    lines = [_format_line(entry) for entry in entries]
    if not lines:
        snapshot_text = "(no interactive elements)" if interactive_only else "(empty page)"
    else:
        snapshot_text = "\n".join(lines)

    origin = ""
    try:
        origin = page.url
    except Exception:  # noqa: BLE001 - Playwright raises when page is closed
        origin = ""

    refs_map: dict[str, dict[str, Any]] = {
        e["ref"]: {"role": e["role"], "name": e["name"]} for e in entries
    }

    return {"snapshot": snapshot_text, "origin": origin, "refs": refs_map}


def _role_list(roles: frozenset[str]) -> str:
    """Serialise a Python frozenset into a JS array literal."""
    import json

    return json.dumps(sorted(roles))


class SnapshotError(Exception):
    """Structured snapshot failure (e.g. selector-not-found)."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
