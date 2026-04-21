"""Session holds the AsyncCamoufox browser + per-tab pages for the sidecar.

Unit 2 owned lifecycle (launch / close); Unit 4 grew the per-page command
surface (snapshot, click, fill, get_text, navigate); Unit 5 replaces the
Unit-4 single ``self._page`` stopgap with a per-tab map keyed by the
agent-browser ``t<N>`` scheme. The Rust daemon owns the counter (reusing
``BrowserManager::format_tab_id`` / ``resolve_tab_ref``) and passes stable
string tab ids in to every command; the sidecar is a pure map from those
ids to Playwright ``Page`` instances, with a per-tab :class:`RefCache` so
a ``click @e1`` on ``t2`` cannot resolve against a stale ``t1`` snapshot.

Why Rust owns the counter (deferred decision from the plan): the Rust
``BrowserManager`` already tracks ``next_tab_id``, formats ids with
``t<N>``, and resolves labels via ``TabRef::parse``. Duplicating any of
that in the sidecar would split a single invariant ("tab ids never
reused") across a process boundary. Instead the sidecar stores pages
under whatever string the Rust side hands it; on ``tab.new`` the Rust
side assigns the next id and tells the sidecar to register the Playwright
``Page`` under that id.
"""

from __future__ import annotations

from typing import Any, Optional

from .protocol import Protocol, log
from .refs import RefCache, RefStale, parse_ref
from .snapshot import SnapshotError, take_snapshot

# Allowlist derived from https://camoufox.com/python/usage/ — keep in sync with
# the plan's Unit 2 Approach. New kwargs must be added deliberately so the
# Rust side knows to expose them; silently passing unknown kwargs through is a
# footgun when Camoufox bumps and adds options we haven't reviewed.
ALLOWED_LAUNCH_KWARGS: frozenset[str] = frozenset(
    {
        "headless",
        "humanize",
        "os",
        "locale",
        "geoip",
        "screen",
        "window",
        "webgl_config",
        "fonts",
        "block_images",
        "block_webrtc",
        "block_webgl",
        "disable_coop",
        "executable_path",
        "proxy",
        "addons",
        "exclude_default_addons",
        "main_world_eval",
        "enable_cache",
        "config",
    }
)

# Explicitly rejected in v1 (see plan). Surfacing a distinct code makes the
# "not-yet-supported" state obvious rather than conflating it with typos.
REJECTED_LAUNCH_KWARGS: frozenset[str] = frozenset(
    {
        "persistent_context",
        "user_data_dir",
    }
)

# Default timeout for per-element actions (ms). Matches agent-browser's
# default_timeout_ms on the Rust side (see BrowserManager::launch).
DEFAULT_ACTION_TIMEOUT_MS: int = 25_000


class LaunchError(Exception):
    """Structured error surfaced as a {"ok": false, "error": {...}} response."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class Tab:
    """A single Playwright :class:`Page` with its own ref cache.

    Keeping the cache per-tab (not per-session, as Unit 4 had it) prevents
    a ``click @e1`` on ``t2`` from resolving against a handle cached by a
    snapshot taken on ``t1``. That failure mode is silent on CDP because
    refs live on the Rust side; it would be silent on Camoufox too without
    this split.
    """

    def __init__(self, tab_id: str, page: Any) -> None:
        self.tab_id = tab_id
        self.page = page
        self.refs = RefCache()


class Session:
    """Holds the AsyncCamoufox browser + a ``{tab_id: Tab}`` map.

    The browser is launched lazily on the first `launch` command so that
    bringing up the sidecar process itself does not require Camoufox to be
    installed — useful for the startup-and-close lifecycle test.
    """

    def __init__(self, protocol: Optional[Protocol] = None) -> None:
        self._camoufox_cm: Optional[Any] = None  # AsyncCamoufox context manager
        self._browser: Optional[Any] = None
        self._tabs: dict[str, Tab] = {}
        self._active_tab_id: Optional[str] = None
        self._launched: bool = False
        # Retained to broadcast page.console / page.crashed back to the Rust
        # daemon. Unit 4 wired framenavigated internally only; Unit 5 finishes
        # the observability story.
        self._protocol: Optional[Protocol] = protocol

    @property
    def is_launched(self) -> bool:
        return self._launched

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def launch(self, args: Optional[dict] = None) -> dict:
        """Launch the Camoufox browser with validated kwargs.

        Returns a result dict for the response frame. Raises LaunchError for
        validation or environment failures that should surface as structured
        errors to the Rust side.
        """
        if self._launched:
            raise LaunchError(
                "already-launched",
                "sidecar already has an active Camoufox browser; close it first",
            )

        kwargs = _validate_launch_args(args or {})

        try:
            from camoufox.async_api import AsyncCamoufox  # type: ignore
        except ImportError as exc:
            raise LaunchError(
                "camoufox-not-installed",
                (
                    "camoufox Python package is not importable: "
                    f"{exc}. Install with `pip install -U 'camoufox[geoip]'`."
                ),
            ) from exc

        cm = AsyncCamoufox(**kwargs)
        try:
            browser = await cm.__aenter__()
        except FileNotFoundError as exc:
            # Camoufox raises FileNotFoundError when the browser binary has
            # not been fetched. Surface the actionable message.
            raise LaunchError(
                "camoufox-not-installed",
                (
                    f"Camoufox browser binary not found: {exc}. "
                    "Run `python -m camoufox fetch`."
                ),
            ) from exc
        except Exception as exc:  # noqa: BLE001
            message = str(exc)
            if _looks_like_missing_binary(message):
                raise LaunchError(
                    "camoufox-not-installed",
                    (
                        f"Camoufox browser binary not available: {message}. "
                        "Run `python -m camoufox fetch`."
                    ),
                ) from exc
            raise LaunchError("launch-failed", message) from exc

        self._camoufox_cm = cm
        self._browser = browser
        self._launched = True
        log("camoufox launched")
        return {"launched": True}

    async def close(self) -> dict:
        """Close the browser if launched. Safe to call when never launched."""
        cm = self._camoufox_cm
        self._camoufox_cm = None
        self._browser = None
        self._tabs.clear()
        self._active_tab_id = None
        self._launched = False
        if cm is None:
            return {"closed": False}
        try:
            await cm.__aexit__(None, None, None)
        except Exception as exc:  # noqa: BLE001
            log(f"error during close: {exc}")
            # Don't re-raise; the sidecar is shutting down either way and
            # leaving a half-closed state just masks the root cause.
        return {"closed": True}

    # ------------------------------------------------------------------
    # Tab management (Unit 5)
    # ------------------------------------------------------------------

    async def tab_new(self, args: Optional[dict] = None) -> dict:
        """Create a new tab and register it under ``args['tabId']``.

        Rust has already assigned the stable ``t<N>`` id before calling — see
        ``BrowserManager::camoufox_tab_new`` in ``browser.rs``. The sidecar
        rejects reused ids so double-registration shows up as a structured
        error rather than a silent handle swap.
        """
        args = args or {}
        browser = self._require_browser()
        tab_id = _require_str(args, "tabId")
        url = args.get("url")
        if tab_id in self._tabs:
            raise LaunchError(
                "tab-id-in-use",
                f"tab id {tab_id!r} is already registered in the sidecar",
            )
        page = await browser.new_page()
        tab = Tab(tab_id, page)
        self._wire_page_events(tab)
        self._tabs[tab_id] = tab
        self._active_tab_id = tab_id

        if isinstance(url, str) and url and url != "about:blank":
            try:
                await page.goto(url, wait_until=_wait_until(args.get("waitUntil", "load")))
            except Exception as exc:  # noqa: BLE001
                # Roll the registration back so Rust doesn't end up thinking
                # the sidecar has a tab it can target.
                await self._close_tab_silently(tab_id)
                raise LaunchError("navigation-failed", str(exc)) from exc

        current_url = _safe_page_url(page)
        title = await _safe_page_title(page)
        return {
            "tabId": tab_id,
            "url": current_url,
            "title": title,
        }

    async def tab_switch(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        tab_id = _require_str(args, "tabId")
        tab = self._require_tab(tab_id)
        try:
            await tab.page.bring_to_front()
        except Exception as exc:  # noqa: BLE001
            # bring_to_front failing usually means the page has been
            # externally closed (e.g. window.close()). Report a structured
            # error; the caller can re-issue tab.list.
            raise LaunchError("tab-gone", str(exc)) from exc
        self._active_tab_id = tab_id
        return {
            "tabId": tab_id,
            "url": _safe_page_url(tab.page),
            "title": await _safe_page_title(tab.page),
        }

    async def tab_close(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        tab_id = args.get("tabId")
        if not isinstance(tab_id, str) or not tab_id:
            # Default to the active tab, matching the Chrome path's
            # ``mgr.tab_close_by_id(None)`` semantics.
            tab_id = self._active_tab_id
        if tab_id is None:
            raise LaunchError("no-active-tab", "no tabs are open in the sidecar")
        tab = self._require_tab(tab_id)
        try:
            await tab.page.close()
        except Exception as exc:  # noqa: BLE001
            log(f"page.close({tab_id!r}) raised: {exc}")
        # ``page.close()`` also fires the ``close`` event wired in
        # ``_wire_page_events``, which pops the tab out of ``self._tabs``
        # before we get here. Use ``pop(..., None)`` so the idempotent path
        # doesn't race a KeyError on whichever handler won.
        self._tabs.pop(tab_id, None)
        if self._active_tab_id == tab_id:
            # Promote the first remaining tab to active (arbitrary but
            # deterministic order — Python dicts preserve insertion order).
            self._active_tab_id = next(iter(self._tabs), None)
        return {"tabId": tab_id, "closed": True, "remaining": len(self._tabs)}

    def tab_list(self, args: Optional[dict] = None) -> dict:
        _ = args
        tabs = [
            {
                "tabId": tab.tab_id,
                "url": _safe_page_url(tab.page),
                "active": tab.tab_id == self._active_tab_id,
            }
            for tab in self._tabs.values()
        ]
        return {"tabs": tabs, "active": self._active_tab_id}

    async def screenshot(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        page = await self._page_for(args)
        full_page = bool(args.get("fullPage", False))
        fmt = (args.get("format") or "png").lower()
        if fmt not in ("png", "jpeg"):
            raise LaunchError(
                "invalid-args",
                f"screenshot format must be `png` or `jpeg`, got {fmt!r}",
            )
        path = args.get("path")
        # Auto-allocate a temp path when the caller didn't pass one. Keeping
        # screenshots on disk (not base64 in the response frame) matches the
        # Chrome CDP path and avoids blowing past the asyncio stdio reader's
        # default line-length limit on full-page captures.
        if not isinstance(path, str) or not path:
            import tempfile
            import time

            suffix = ".jpg" if fmt == "jpeg" else ".png"
            fd, path = tempfile.mkstemp(prefix=f"ab-camoufox-{int(time.time() * 1000)}-", suffix=suffix)
            import os

            os.close(fd)
        kwargs: dict[str, Any] = {
            "full_page": full_page,
            "type": fmt,
            "path": path,
        }
        quality = args.get("quality")
        if fmt == "jpeg" and isinstance(quality, int):
            kwargs["quality"] = quality
        try:
            await page.screenshot(**kwargs)
        except Exception as exc:  # noqa: BLE001
            raise LaunchError("screenshot-failed", str(exc)) from exc

        return {
            "path": path,
            "format": fmt,
            "fullPage": full_page,
        }

    async def _page_for(self, args: dict) -> Any:
        """Resolve the target page for a command.

        Commands may pass an explicit ``tabId``; otherwise we fall back to
        the active tab, creating it on demand for the first `page.goto`
        that happens before `tab.new` (preserves the Unit-4 flow where
        `agent-browser --engine camoufox open <url>` doesn't issue an
        explicit tab.new first).
        """
        tab_id = args.get("tabId")
        if isinstance(tab_id, str) and tab_id:
            return self._require_tab(tab_id).page
        # Lazy default tab: only created when someone actually needs a page.
        if self._active_tab_id is None:
            await self._ensure_default_tab()
        return self._require_tab(self._active_tab_id).page  # type: ignore[arg-type]

    async def _ensure_default_tab(self) -> None:
        """Create an implicit ``t1`` tab on first use.

        The Unit-4 smoke path (`agent-browser --engine camoufox open <url>`)
        issues a raw `page.goto` without first calling `tab.new`. We keep
        that working by auto-creating a page under the canonical first id;
        once Rust issues an explicit `tab.new` the implicit tab stays
        registered under its id and the counter continues from there.
        """
        browser = self._require_browser()
        page = await browser.new_page()
        tab = Tab("t1", page)
        self._wire_page_events(tab)
        self._tabs["t1"] = tab
        self._active_tab_id = "t1"

    def _require_browser(self) -> Any:
        if not self._launched or self._browser is None:
            raise LaunchError(
                "not-launched",
                "Camoufox browser is not launched; send `launch` first",
            )
        return self._browser

    def _require_tab(self, tab_id: str) -> Tab:
        tab = self._tabs.get(tab_id)
        if tab is None:
            raise LaunchError(
                "tab-not-found",
                f"no tab registered with id {tab_id!r}",
            )
        return tab

    async def _close_tab_silently(self, tab_id: str) -> None:
        tab = self._tabs.pop(tab_id, None)
        if tab is None:
            return
        if self._active_tab_id == tab_id:
            self._active_tab_id = next(iter(self._tabs), None)
        try:
            await tab.page.close()
        except Exception as exc:  # noqa: BLE001
            log(f"silent tab close ({tab_id!r}) raised: {exc}")

    def _wire_page_events(self, tab: Tab) -> None:
        """Invalidate the tab's ref cache on nav + forward console/crash events.

        Playwright's ``framenavigated`` fires for every frame; we invalidate
        only on main-frame navigations. ``console`` and ``crash`` events
        fan out to the Rust daemon via the shared :class:`Protocol`.
        """
        page = tab.page

        def _on_framenavigated(frame: Any) -> None:
            try:
                if frame == page.main_frame:
                    tab.refs.invalidate()
            except Exception as exc:  # noqa: BLE001
                log(f"framenavigated handler: {exc}")

        def _on_console(msg: Any) -> None:
            if self._protocol is None:
                return
            try:
                data = {
                    "tabId": tab.tab_id,
                    "level": getattr(msg, "type", lambda: "log")()
                    if callable(getattr(msg, "type", None))
                    else getattr(msg, "type", "log"),
                    "text": getattr(msg, "text", lambda: "")()
                    if callable(getattr(msg, "text", None))
                    else getattr(msg, "text", ""),
                }
            except Exception as exc:  # noqa: BLE001
                log(f"console handler payload build failed: {exc}")
                return
            _schedule_event(self._protocol, "page.console", data)

        def _on_crash(_page: Any) -> None:
            if self._protocol is None:
                return
            _schedule_event(self._protocol, "page.crashed", {"tabId": tab.tab_id})

        def _on_close(_page: Any) -> None:
            # A tab closed out from under us (window.close(), target_blank
            # cascade, etc.): drop our reference so later commands see a
            # clean `tab-not-found` rather than a dangling Playwright handle.
            self._tabs.pop(tab.tab_id, None)
            if self._active_tab_id == tab.tab_id:
                self._active_tab_id = next(iter(self._tabs), None)

        for event_name, handler in (
            ("framenavigated", _on_framenavigated),
            ("console", _on_console),
            ("crash", _on_crash),
            ("close", _on_close),
        ):
            try:
                page.on(event_name, handler)
            except Exception as exc:  # noqa: BLE001
                log(f"could not attach {event_name} handler: {exc}")

    # ------------------------------------------------------------------
    # Commands that operate on a tab's page
    # ------------------------------------------------------------------

    async def goto(self, args: Optional[dict] = None) -> dict:
        """Navigate the target page to ``args['url']``.

        ``args['tabId']`` selects the tab; defaulting to the active tab
        (which is auto-created on first use) keeps the single-tab open flow
        from Unit 3 working unchanged.
        """
        args = args or {}
        url = args.get("url")
        if not isinstance(url, str) or not url:
            raise LaunchError(
                "invalid-args",
                "`page.goto` requires a non-empty `url` string",
            )
        wait_until = _wait_until(args.get("waitUntil", "load"))

        tab = await self._tab_for(args)
        # Any navigation request invalidates prior refs, even before
        # ``framenavigated`` fires; clearing here closes the window in which
        # an agent could click on a stale ref after issuing ``navigate``.
        tab.refs.invalidate()

        try:
            response = await tab.page.goto(url, wait_until=wait_until)
        except Exception as exc:  # noqa: BLE001
            raise LaunchError("navigation-failed", str(exc)) from exc

        title = await _safe_page_title(tab.page)
        final_url = _safe_page_url(tab.page)
        status = response.status if response is not None else None
        return {"url": final_url, "title": title, "status": status, "tabId": tab.tab_id}

    async def snapshot(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        tab = await self._tab_for(args)
        try:
            return await take_snapshot(
                tab.page,
                tab.refs,
                interactive_only=bool(args.get("interactive", False)),
                selector=args.get("selector"),
            )
        except SnapshotError as exc:
            raise LaunchError(exc.code, exc.message) from exc

    async def click(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        selector_or_ref = _require_str(args, "selector")
        button = args.get("button", "left")
        click_count = int(args.get("clickCount", 1) or 1)
        timeout = int(args.get("timeoutMs") or DEFAULT_ACTION_TIMEOUT_MS)

        tab = await self._tab_for(args)
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = _require_ref(tab, ref_id)
            await _try_click_handle(handle, button, click_count, timeout)
        else:
            await _try_click_locator(
                tab.page.locator(selector_or_ref),
                selector_or_ref,
                button,
                click_count,
                timeout,
            )
        return {"clicked": selector_or_ref, "tabId": tab.tab_id}

    async def fill(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        selector_or_ref = _require_str(args, "selector")
        value = args.get("value")
        if not isinstance(value, str):
            raise LaunchError("invalid-args", "`fill` requires a string `value` argument")
        timeout = int(args.get("timeoutMs") or DEFAULT_ACTION_TIMEOUT_MS)

        tab = await self._tab_for(args)
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = _require_ref(tab, ref_id)
            await _try_fill_handle(handle, value, timeout)
        else:
            await _try_fill_locator(
                tab.page.locator(selector_or_ref), selector_or_ref, value, timeout
            )
        return {"filled": selector_or_ref, "tabId": tab.tab_id}

    async def get_text(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        selector_or_ref = _require_str(args, "selector")
        timeout = int(args.get("timeoutMs") or DEFAULT_ACTION_TIMEOUT_MS)

        tab = await self._tab_for(args)
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = _require_ref(tab, ref_id)
            text = await _handle_text(handle, timeout)
        else:
            text = await _locator_text(
                tab.page.locator(selector_or_ref), selector_or_ref, timeout
            )
        return {"text": text, "origin": _safe_page_url(tab.page), "tabId": tab.tab_id}

    async def _tab_for(self, args: dict) -> Tab:
        tab_id = args.get("tabId")
        if isinstance(tab_id, str) and tab_id:
            return self._require_tab(tab_id)
        if self._active_tab_id is None:
            await self._ensure_default_tab()
        return self._require_tab(self._active_tab_id)  # type: ignore[arg-type]


# ---------------------------------------------------------------------------
# Internal helpers — kept module-level so Session stays focused on lifecycle
# and command dispatch, not Playwright error translation.
# ---------------------------------------------------------------------------


def _require_str(args: dict, key: str) -> str:
    value = args.get(key)
    if not isinstance(value, str) or not value:
        raise LaunchError("invalid-args", f"missing required `{key}` string argument")
    return value


def _require_ref(tab: Tab, ref_id: str) -> Any:
    try:
        return tab.refs.require(ref_id)
    except RefStale as exc:
        raise LaunchError("ref-stale", exc.message) from exc


def _classify_playwright_error(exc: Exception, selector_or_ref: str) -> LaunchError:
    """Translate Playwright errors into agent-browser error codes.

    Keeping this logic in one place means new error codes (e.g. ``timeout``)
    pick up the same behaviour across click/fill/get_text without each handler
    reimplementing the pattern match.
    """
    msg = str(exc)
    lowered = msg.lower()
    if "strict mode violation" in lowered or "resolved to" in lowered and "elements" in lowered:
        # Try to parse the match count from the message ("resolved to N elements").
        import re

        match = re.search(r"resolved to\s+(\d+)\s+elements", msg)
        count = int(match.group(1)) if match else 0
        return LaunchError(
            "ambiguous-selector",
            f"Selector {selector_or_ref!r} matched {count} elements; refine it or use a ref",
        )
    if "element is not attached" in lowered or "node is detached" in lowered or "detached" in lowered:
        return LaunchError("element-detached", msg)
    if "timeout" in lowered and "exceeded" in lowered:
        return LaunchError("timeout", msg)
    if "no element matches" in lowered or "no elements match" in lowered:
        return LaunchError("selector-not-found", msg)
    return LaunchError("action-failed", msg)


async def _try_click_handle(handle: Any, button: str, click_count: int, timeout: int) -> None:
    try:
        await handle.click(button=button, click_count=click_count, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, "<ref>") from exc


async def _try_click_locator(
    locator: Any, selector: str, button: str, click_count: int, timeout: int
) -> None:
    try:
        count = await locator.count()
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc
    if count == 0:
        raise LaunchError(
            "selector-not-found",
            f"Selector {selector!r} did not match any element",
        )
    if count > 1:
        raise LaunchError(
            "ambiguous-selector",
            f"Selector {selector!r} matched {count} elements; refine it or use a ref",
        )
    try:
        await locator.click(button=button, click_count=click_count, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc


async def _try_fill_handle(handle: Any, value: str, timeout: int) -> None:
    try:
        await handle.fill(value, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, "<ref>") from exc


async def _try_fill_locator(locator: Any, selector: str, value: str, timeout: int) -> None:
    try:
        count = await locator.count()
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc
    if count == 0:
        raise LaunchError(
            "selector-not-found",
            f"Selector {selector!r} did not match any element",
        )
    if count > 1:
        raise LaunchError(
            "ambiguous-selector",
            f"Selector {selector!r} matched {count} elements; refine it or use a ref",
        )
    try:
        await locator.fill(value, timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc


async def _handle_text(handle: Any, timeout: int) -> str:
    # ElementHandle.text_content does not accept a ``timeout`` kwarg (only the
    # Locator variant does) — if the handle is still live in the cache we
    # already know the element is attached, so no additional timeout gymnastics
    # are needed.
    _ = timeout
    try:
        raw = await handle.text_content()
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, "<ref>") from exc
    return (raw or "").strip()


async def _locator_text(locator: Any, selector: str, timeout: int) -> str:
    try:
        count = await locator.count()
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc
    if count == 0:
        raise LaunchError(
            "selector-not-found",
            f"Selector {selector!r} did not match any element",
        )
    if count > 1:
        raise LaunchError(
            "ambiguous-selector",
            f"Selector {selector!r} matched {count} elements; refine it or use a ref",
        )
    try:
        raw = await locator.text_content(timeout=timeout)
    except Exception as exc:  # noqa: BLE001
        raise _classify_playwright_error(exc, selector) from exc
    return (raw or "").strip()


def _validate_launch_args(args: dict) -> dict:
    if not isinstance(args, dict):
        raise LaunchError(
            "invalid-args",
            f"launch args must be an object, got {type(args).__name__}",
        )
    rejected = sorted(set(args) & REJECTED_LAUNCH_KWARGS)
    if rejected:
        raise LaunchError(
            "unsupported-launch-option",
            (
                f"launch options not supported in v1: {rejected}. "
                "persistent_context / user_data_dir are tracked as a v2 item."
            ),
        )
    unknown = sorted(set(args) - ALLOWED_LAUNCH_KWARGS - REJECTED_LAUNCH_KWARGS)
    if unknown:
        raise LaunchError(
            "unknown-launch-option",
            f"unknown launch option(s): {unknown}",
        )
    return dict(args)


def _looks_like_missing_binary(message: str) -> bool:
    """Heuristic for Camoufox's 'please run camoufox fetch' family of errors."""
    lowered = message.lower()
    return any(
        needle in lowered
        for needle in (
            "camoufox fetch",
            "no camoufox",
            "camoufox is not installed",
            "executable doesn't exist",
        )
    )


def _wait_until(raw: Any) -> str:
    if not isinstance(raw, str):
        return "load"
    return "commit" if raw == "none" else raw


def _safe_page_url(page: Any) -> str:
    try:
        return page.url or ""
    except Exception:  # noqa: BLE001 - Playwright raises when the page has closed
        return ""


async def _safe_page_title(page: Any) -> str:
    try:
        return await page.title()
    except Exception:  # noqa: BLE001
        return ""


def _schedule_event(protocol: Protocol, name: str, data: dict) -> None:
    """Fire-and-forget an event frame from a sync Playwright callback.

    Playwright's ``page.on(...)`` handlers are invoked synchronously from
    Playwright's dispatcher task, so we drop onto the running event loop
    via ``asyncio.ensure_future``. Errors are swallowed because a console
    event that doesn't reach the daemon must not take down a live session.
    """
    import asyncio

    try:
        loop = asyncio.get_event_loop()
    except RuntimeError:
        return
    try:
        loop.create_task(protocol.write_event(name, data))
    except Exception as exc:  # noqa: BLE001
        log(f"could not schedule {name} event: {exc}")


