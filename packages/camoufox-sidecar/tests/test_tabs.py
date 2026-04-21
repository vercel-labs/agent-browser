"""Tab + screenshot tests for the Camoufox sidecar (Unit 5 of the plan).

The sidecar is driven as a real subprocess; tests skip automatically if the
`camoufox` Python package isn't importable. Tab ids are assigned by the
Rust daemon in production, but these tests mimic Rust's role by passing
`tabId="t1"`, `"t2"`, ... explicitly so the counter-owned-by-Rust contract
is honoured.
"""

from __future__ import annotations

import asyncio
import pathlib
import tempfile
from typing import Any

import pytest

from conftest import Sidecar, spawn_sidecar  # noqa: E402


pytestmark = pytest.mark.asyncio


BLANK_URL = "data:text/html,<html><body>blank</body></html>"
FIXTURE_URL = (
    "file://"
    + str(
        pathlib.Path(__file__).resolve().parents[3] / "cli/tests/fixtures/form.html"
    )
)


async def _launch(sc: Sidecar) -> None:
    await sc.expect_event("ready")
    await sc.send({"id": 1, "cmd": "launch", "args": {"headless": True}})
    resp = await sc.read_frame(timeout=60.0)
    assert resp["ok"] is True, resp


async def _call(sc: Sidecar, cid: int, cmd: str, args: dict[str, Any]) -> dict:
    await sc.send({"id": cid, "cmd": cmd, "args": args})
    return await sc.read_frame(timeout=30.0)


@pytest.fixture
async def camoufox_sidecar(requires_camoufox: None):
    sc = await spawn_sidecar()
    try:
        yield sc
    finally:
        await sc.kill()


async def test_tab_new_registers_under_rust_assigned_id(
    camoufox_sidecar: Sidecar,
) -> None:
    """Rust hands `t1` down; the sidecar stores the page under that key."""
    sc = camoufox_sidecar
    await _launch(sc)

    resp = await _call(sc, 2, "tab.new", {"tabId": "t1", "url": BLANK_URL})
    assert resp["ok"] is True, resp
    assert resp["result"]["tabId"] == "t1"
    # Second tab with a Rust-assigned id.
    resp = await _call(sc, 3, "tab.new", {"tabId": "t2", "url": BLANK_URL})
    assert resp["ok"] is True, resp
    assert resp["result"]["tabId"] == "t2"

    listed = await _call(sc, 4, "tab.list", {})
    tab_ids = [t["tabId"] for t in listed["result"]["tabs"]]
    assert tab_ids == ["t1", "t2"], listed


async def test_tab_switch_routes_subsequent_commands(
    camoufox_sidecar: Sidecar,
) -> None:
    """`switch t1` + `click @e1` must drive the right page's DOM."""
    sc = camoufox_sidecar
    await _launch(sc)

    # t1: fixture form; t2: blank. Snapshot on t1 while t2 is active must
    # still find the submit button because the tabId selects the target.
    assert (await _call(sc, 2, "tab.new", {"tabId": "t1", "url": FIXTURE_URL}))["ok"]
    assert (await _call(sc, 3, "tab.new", {"tabId": "t2", "url": BLANK_URL}))["ok"]
    assert (await _call(sc, 4, "tab.switch", {"tabId": "t2"}))["ok"]

    snap = await _call(sc, 5, "page.snapshot", {"tabId": "t1"})
    assert snap["ok"] is True, snap
    submit_ref = next(
        r
        for r, meta in snap["result"]["refs"].items()
        if meta["role"] == "button" and meta["name"].strip() == "Submit"
    )

    click = await _call(
        sc, 6, "page.click", {"tabId": "t1", "selector": f"@{submit_ref}"}
    )
    assert click["ok"] is True, click

    text = await _call(
        sc, 7, "page.getText", {"tabId": "t1", "selector": "#status"}
    )
    assert text["ok"] is True, text
    assert text["result"]["text"] == "Submitted"


async def test_tab_close_closes_target_and_keeps_others(
    camoufox_sidecar: Sidecar,
) -> None:
    sc = camoufox_sidecar
    await _launch(sc)
    for cid, tab_id in ((2, "t1"), (3, "t2"), (4, "t3")):
        assert (
            await _call(sc, cid, "tab.new", {"tabId": tab_id, "url": BLANK_URL})
        )["ok"], tab_id

    close = await _call(sc, 5, "tab.close", {"tabId": "t2"})
    assert close["ok"] is True, close
    assert close["result"]["remaining"] == 2
    listed = await _call(sc, 6, "tab.list", {})
    assert [t["tabId"] for t in listed["result"]["tabs"]] == ["t1", "t3"]


async def test_ref_cache_is_per_tab(camoufox_sidecar: Sidecar) -> None:
    """A ref from `t1` cannot resolve on `t2`.

    Unit 4 used a session-level cache; with tabs that would cross the
    streams. A click by `@e1` on t2 must surface `ref-stale` rather than
    silently click a t1-scoped handle.
    """
    sc = camoufox_sidecar
    await _launch(sc)
    assert (await _call(sc, 2, "tab.new", {"tabId": "t1", "url": FIXTURE_URL}))["ok"]
    assert (await _call(sc, 3, "tab.new", {"tabId": "t2", "url": BLANK_URL}))["ok"]

    # Snapshot on t1 populates t1's cache only.
    snap = await _call(sc, 4, "page.snapshot", {"tabId": "t1"})
    submit_ref = next(
        r
        for r, meta in snap["result"]["refs"].items()
        if meta["role"] == "button" and meta["name"].strip() == "Submit"
    )

    # Click by the same ref on t2 — t2 has no cache entries.
    resp = await _call(
        sc, 5, "page.click", {"tabId": "t2", "selector": f"@{submit_ref}"}
    )
    assert resp["ok"] is False, resp
    assert resp["error"]["code"] == "ref-stale", resp


async def test_screenshot_writes_png(camoufox_sidecar: Sidecar) -> None:
    sc = camoufox_sidecar
    await _launch(sc)
    assert (await _call(sc, 2, "tab.new", {"tabId": "t1", "url": FIXTURE_URL}))["ok"]

    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as tmp:
        path = tmp.name
    try:
        resp = await _call(sc, 3, "page.screenshot", {"tabId": "t1", "path": path})
        assert resp["ok"] is True, resp
        assert resp["result"]["path"] == path
        data = pathlib.Path(path).read_bytes()
        # PNG magic.
        assert data[:8] == b"\x89PNG\r\n\x1a\n"
        assert len(data) > 128
    finally:
        pathlib.Path(path).unlink(missing_ok=True)


async def test_screenshot_fullpage_is_taller(camoufox_sidecar: Sidecar) -> None:
    """`fullPage: true` produces a larger image than the viewport-only variant."""
    sc = camoufox_sidecar
    await _launch(sc)
    assert (
        await _call(
            sc,
            2,
            "tab.new",
            {
                "tabId": "t1",
                # Tall page so full-page differs from viewport. Inline style sets
                # body height to 3000px so the two captures diverge clearly.
                "url": "data:text/html,"
                + "<html><body style='margin:0;height:3000px;background:"
                + "linear-gradient(red,blue)'>long</body></html>",
            },
        )
    )["ok"]

    viewport_path = tempfile.NamedTemporaryFile(suffix=".png", delete=False).name
    full_path = tempfile.NamedTemporaryFile(suffix=".png", delete=False).name
    try:
        viewport = await _call(
            sc, 3, "page.screenshot", {"tabId": "t1", "path": viewport_path}
        )
        assert viewport["ok"] is True, viewport
        full = await _call(
            sc,
            4,
            "page.screenshot",
            {"tabId": "t1", "fullPage": True, "path": full_path},
        )
        assert full["ok"] is True, full
        vp_bytes = pathlib.Path(viewport_path).read_bytes()
        fp_bytes = pathlib.Path(full_path).read_bytes()
        assert fp_bytes[:8] == b"\x89PNG\r\n\x1a\n"
        assert vp_bytes[:8] == b"\x89PNG\r\n\x1a\n"
        assert len(fp_bytes) > len(vp_bytes), (len(vp_bytes), len(fp_bytes))
    finally:
        pathlib.Path(viewport_path).unlink(missing_ok=True)
        pathlib.Path(full_path).unlink(missing_ok=True)


async def test_tab_not_found_returns_structured_error(
    camoufox_sidecar: Sidecar,
) -> None:
    sc = camoufox_sidecar
    await _launch(sc)
    assert (await _call(sc, 2, "tab.new", {"tabId": "t1", "url": BLANK_URL}))["ok"]

    resp = await _call(sc, 3, "tab.close", {"tabId": "t99"})
    assert resp["ok"] is False, resp
    assert resp["error"]["code"] == "tab-not-found", resp


async def test_console_event_forwarded(camoufox_sidecar: Sidecar) -> None:
    """`page.console` events fan out to the Rust daemon via the protocol.

    Unit 4 only wired framenavigated internally; Unit 5 finishes the
    console/crash broadcast wiring called out in the next-unit context.
    The test uses a ``navigate`` + post-load ``console.log`` injected via
    an inline <script> after the page has loaded so Playwright's console
    listener is already attached when the log fires (Firefox's listener
    doesn't replay events that happened before ``page.on`` was called).
    """
    sc = camoufox_sidecar
    await _launch(sc)
    # Create the tab empty first so the console listener is wired *before*
    # the page script that emits the event runs.
    assert (await _call(sc, 2, "tab.new", {"tabId": "t1", "url": "about:blank"}))["ok"]

    # Navigate to a page whose inline <script> emits a console log; the
    # listener is attached at this point. Read frames manually with
    # ``include_events=True`` so the Unit-5 default (skip events) doesn't
    # swallow the event we're explicitly looking for.
    await sc.send(
        {
            "id": 3,
            "cmd": "page.goto",
            "args": {
                "tabId": "t1",
                "url": (
                    "data:text/html,<html><body>hi"
                    "<script>console.log('hello from t1')</script>"
                    "</body></html>"
                ),
            },
        }
    )

    saw_console = False
    saw_response = False
    deadline = asyncio.get_event_loop().time() + 10.0
    while asyncio.get_event_loop().time() < deadline and not (saw_console and saw_response):
        try:
            frame = await sc.read_frame(timeout=1.0, include_events=True)
        except (asyncio.TimeoutError, RuntimeError):
            continue
        if frame.get("event") == "page.console":
            if (
                frame["data"].get("tabId") == "t1"
                and "hello from t1" in frame["data"].get("text", "")
            ):
                saw_console = True
        elif frame.get("id") == 3:
            saw_response = True
    assert saw_console, "expected a page.console event within 10s"
    assert saw_response, "expected the page.goto response within 10s"
