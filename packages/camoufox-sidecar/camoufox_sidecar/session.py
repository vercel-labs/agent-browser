"""Session holds the AsyncCamoufox browser for the sidecar's lifetime.

Unit 2 owns the lifecycle (launch / close / cleanup); later units add the
command handlers (navigate, snapshot, click, ...). The launch-kwarg allowlist
lives here because it's the public contract with the Rust side.
"""

from __future__ import annotations

from typing import Any, Optional

from .protocol import log

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
        if cm is None:
            return {"closed": False}
        try:
            await cm.__aexit__(None, None, None)
        except Exception as exc:  # noqa: BLE001
            log(f"error during close: {exc}")
            # Don't re-raise; the sidecar is shutting down either way and
            # leaving a half-closed state just masks the root cause.
        return {"closed": True}

    async def goto(self, args: Optional[dict] = None) -> dict:
        """Navigate the single session page to ``args['url']``.

        Unit 3 covers only single-tab open+close+goto as the smoke flow for
        `agent-browser --engine camoufox open <url>`. Multi-tab routing and
        ref-aware snapshot/click ride on top of this in Units 4 and 5.
        """
        if not self._launched or self._browser is None:
            raise LaunchError(
                "not-launched",
                "Camoufox browser is not launched; send `launch` first",
            )
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

        if self._page is None:
            self._page = await self._browser.new_page()

        try:
            response = await self._page.goto(url, wait_until=wait_until)
        except Exception as exc:  # noqa: BLE001
            raise LaunchError("navigation-failed", str(exc)) from exc

        try:
            title = await self._page.title()
        except Exception:  # noqa: BLE001
            title = ""
        final_url = self._page.url
        status = response.status if response is not None else None
        return {"url": final_url, "title": title, "status": status}


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
