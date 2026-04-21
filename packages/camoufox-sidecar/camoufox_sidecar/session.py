"""Session holds the AsyncCamoufox browser for the sidecar's lifetime.

Unit 2 owned lifecycle (launch / close); Unit 4 grows the per-page command
surface: snapshot, click, fill, get_text, navigate. The single
``self._page`` held here is a stopgap — Unit 5 (tabs) will replace it with a
per-tab map.
"""

from __future__ import annotations

from typing import Any, Optional

from .protocol import log
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


class Session:
    """Holds the single AsyncCamoufox browser + its playwright context.

    The browser is launched lazily on the first `launch` command so that
    bringing up the sidecar process itself does not require Camoufox to be
    installed — useful for the startup-and-close lifecycle test.
    """

    def __init__(self) -> None:
        self._camoufox_cm: Optional[Any] = None  # AsyncCamoufox context manager
        self._browser: Optional[Any] = None
        self._page: Optional[Any] = None  # lazily created on first goto
        self._launched: bool = False
        self._ref_cache: RefCache = RefCache()

    @property
    def is_launched(self) -> bool:
        return self._launched

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
        self._page = None
        self._launched = False
        self._ref_cache.invalidate()
        if cm is None:
            return {"closed": False}
        try:
            await cm.__aexit__(None, None, None)
        except Exception as exc:  # noqa: BLE001
            log(f"error during close: {exc}")
            # Don't re-raise; the sidecar is shutting down either way and
            # leaving a half-closed state just masks the root cause.
        return {"closed": True}

    async def _ensure_page(self) -> Any:
        if not self._launched or self._browser is None:
            raise LaunchError(
                "not-launched",
                "Camoufox browser is not launched; send `launch` first",
            )
        if self._page is None:
            self._page = await self._browser.new_page()
            self._wire_page_events(self._page)
        return self._page

    def _wire_page_events(self, page: Any) -> None:
        """Invalidate the ref cache on navigation and forward lifecycle events.

        Playwright's ``framenavigated`` fires for every frame, including
        subframes, so we scope invalidation to main-frame navigations only.
        Unit 4/5 adds ``page.console``/``page.crashed`` forwarding.
        """

        def _on_framenavigated(frame: Any) -> None:
            try:
                if frame == page.main_frame:
                    self._ref_cache.invalidate()
            except Exception as exc:  # noqa: BLE001
                log(f"framenavigated handler: {exc}")

        try:
            page.on("framenavigated", _on_framenavigated)
        except Exception as exc:  # noqa: BLE001
            log(f"could not attach framenavigated handler: {exc}")

    async def goto(self, args: Optional[dict] = None) -> dict:
        """Navigate the single session page to ``args['url']``.

        Unit 3 covers only single-tab open+close+goto as the smoke flow for
        `agent-browser --engine camoufox open <url>`. Multi-tab routing and
        ref-aware snapshot/click ride on top of this in Units 4 and 5.
        """
        args = args or {}
        url = args.get("url")
        if not isinstance(url, str) or not url:
            raise LaunchError(
                "invalid-args",
                "`page.goto` requires a non-empty `url` string",
            )
        wait_until = args.get("waitUntil", "load")
        if wait_until == "none":
            wait_until = "commit"

        page = await self._ensure_page()
        # Any navigation request invalidates prior refs, even before
        # ``framenavigated`` fires; clearing here closes the window in which
        # an agent could click on a stale ref after issuing ``navigate``.
        self._ref_cache.invalidate()

        try:
            response = await page.goto(url, wait_until=wait_until)
        except Exception as exc:  # noqa: BLE001
            raise LaunchError("navigation-failed", str(exc)) from exc

        try:
            title = await page.title()
        except Exception:  # noqa: BLE001
            title = ""
        final_url = page.url
        status = response.status if response is not None else None
        return {"url": final_url, "title": title, "status": status}

    # ------------------------------------------------------------------
    # Unit 4: core command surface
    # ------------------------------------------------------------------

    async def snapshot(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        page = await self._ensure_page()
        try:
            return await take_snapshot(
                page,
                self._ref_cache,
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

        page = await self._ensure_page()
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = self._require_ref(ref_id)
            await _try_click_handle(handle, button, click_count, timeout)
        else:
            await _try_click_locator(page.locator(selector_or_ref), selector_or_ref, button, click_count, timeout)
        return {"clicked": selector_or_ref}

    async def fill(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        selector_or_ref = _require_str(args, "selector")
        value = args.get("value")
        if not isinstance(value, str):
            raise LaunchError("invalid-args", "`fill` requires a string `value` argument")
        timeout = int(args.get("timeoutMs") or DEFAULT_ACTION_TIMEOUT_MS)

        page = await self._ensure_page()
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = self._require_ref(ref_id)
            await _try_fill_handle(handle, value, timeout)
        else:
            await _try_fill_locator(page.locator(selector_or_ref), selector_or_ref, value, timeout)
        return {"filled": selector_or_ref}

    async def get_text(self, args: Optional[dict] = None) -> dict:
        args = args or {}
        selector_or_ref = _require_str(args, "selector")
        timeout = int(args.get("timeoutMs") or DEFAULT_ACTION_TIMEOUT_MS)

        page = await self._ensure_page()
        ref_id = parse_ref(selector_or_ref)
        if ref_id is not None:
            handle = self._require_ref(ref_id)
            text = await _handle_text(handle, timeout)
        else:
            text = await _locator_text(page.locator(selector_or_ref), selector_or_ref, timeout)
        return {"text": text, "origin": page.url}

    def _require_ref(self, ref_id: str) -> Any:
        try:
            return self._ref_cache.require(ref_id)
        except RefStale as exc:
            raise LaunchError("ref-stale", exc.message) from exc


# ---------------------------------------------------------------------------
# Internal helpers — kept module-level so Session stays focused on lifecycle
# and command dispatch, not Playwright error translation.
# ---------------------------------------------------------------------------


def _require_str(args: dict, key: str) -> str:
    value = args.get(key)
    if not isinstance(value, str) or not value:
        raise LaunchError("invalid-args", f"missing required `{key}` string argument")
    return value


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
