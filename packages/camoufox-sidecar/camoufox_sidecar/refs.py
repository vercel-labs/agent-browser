"""`@eN` ref cache for the Camoufox sidecar.

The CDP path in agent-browser hands agents ``@e1``, ``@e2`` … tokens that
survive beyond the snapshot that created them, backed by Chrome's cross-tree
``backend_node_id`` identity. Playwright exposes no equivalent — an
``ElementHandle`` is the closest thing, and it has narrower semantics: handles
only remain valid while their element stays attached to the same document.

Per the plan's Key Technical Decisions, the sidecar therefore:

    * caches an ``ElementHandle`` per ``@eN`` during ``page.snapshot``;
    * clears the cache on ``frame.navigated`` so cross-navigation refs become
      structurally unavailable rather than silently pointing at a new element;
    * surfaces ``{"code": "ref-stale"}`` when a caller reaches for a ref that
      is either missing from the cache or whose handle Playwright reports as
      detached.

This is a narrower semantic than Chrome's ``backend_node_id``. The narrower
shape is documented in ``docs/engines/camoufox.md`` (planned Unit 8) and
surfaces as ``ref-stale`` rather than a silent cross-navigation mismatch.
"""

from __future__ import annotations

import re
from typing import Any, Optional


# Recognise ``@e1``, ``e1``, or ``ref=e1`` — mirrors ``parse_ref`` on the
# Rust side (``cli/src/native/element.rs``) so both engines accept the same
# agent-facing token shapes.
_REF_RE = re.compile(r"^(?:@|ref=)?(e[0-9]+)$")


def parse_ref(selector_or_ref: str) -> Optional[str]:
    """Return ``"eN"`` if the input looks like an agent-browser ref, else ``None``."""
    if not isinstance(selector_or_ref, str):
        return None
    match = _REF_RE.match(selector_or_ref.strip())
    return match.group(1) if match else None


class RefStale(Exception):
    """Raised by ``RefCache.require`` when a ref is missing or detached."""

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


class RefCache:
    """ElementHandle cache keyed by ``@eN``.

    Cheap to construct; no background tasks. The owner is expected to call
    :meth:`invalidate` whenever the browser navigates so callers see an honest
    ``ref-stale`` error instead of a silently-rebound handle.
    """

    def __init__(self) -> None:
        self._handles: dict[str, Any] = {}
        self._metadata: dict[str, dict[str, Any]] = {}
        self._next_id: int = 1

    def __contains__(self, ref_id: str) -> bool:
        return ref_id in self._handles

    def invalidate(self) -> None:
        """Drop all cached handles.

        We don't ``await handle.dispose()`` here because callers hit this on
        the sync ``framenavigated`` event path; Playwright cleans up detached
        handles on its own. This method is cheap to call more than once.
        """
        self._handles.clear()
        self._metadata.clear()
        self._next_id = 1

    def next_ref_id(self) -> str:
        ref_id = f"e{self._next_id}"
        self._next_id += 1
        return ref_id

    def put(self, ref_id: str, handle: Any, *, role: str, name: str) -> None:
        self._handles[ref_id] = handle
        self._metadata[ref_id] = {"role": role, "name": name}

    def get(self, ref_id: str) -> Optional[Any]:
        return self._handles.get(ref_id)

    def metadata(self, ref_id: str) -> Optional[dict[str, Any]]:
        return self._metadata.get(ref_id)

    def entries(self) -> dict[str, dict[str, Any]]:
        """Return a ``{ref_id: {role, name}}`` view suitable for the ``refs`` response field."""
        return {k: dict(v) for k, v in self._metadata.items()}

    def require(self, ref_id: str) -> Any:
        """Return the handle for ``ref_id`` or raise :class:`RefStale`.

        Callers should catch Playwright errors when *using* the returned handle
        and translate them into ``RefStale`` as well — this method only covers
        the "not in cache" failure mode.
        """
        handle = self._handles.get(ref_id)
        if handle is None:
            raise RefStale(
                f"ref {ref_id!r} is not in the snapshot cache "
                "(may have been invalidated by a navigation; re-snapshot)"
            )
        return handle
