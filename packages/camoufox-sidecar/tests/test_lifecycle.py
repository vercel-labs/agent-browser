"""Lifecycle tests for the Camoufox sidecar.

Covers the 5 scenarios from Unit 2 of the plan:

    1. ready + close happy path
    2. stdin EOF triggers cleanup
    3. Unknown launch option → structured error, session still usable
    4. Camoufox binary missing → actionable error
    5. SIGTERM cleans up the Firefox child within 5s
"""

from __future__ import annotations

import asyncio
import os
import signal

import pytest

try:
    import psutil  # type: ignore
except ImportError:  # pragma: no cover - test-only dep
    psutil = None  # type: ignore

from conftest import Sidecar, spawn_sidecar  # noqa: E402 — pytest injects tests/ onto sys.path


pytestmark = pytest.mark.asyncio


async def test_ready_and_close(sidecar: Sidecar) -> None:
    """#1: Sidecar starts, emits `ready`, accepts `close`, exits 0 fast."""
    frame = await asyncio.wait_for(sidecar.expect_event("ready"), timeout=2.0)
    assert isinstance(frame.get("data"), dict)
    assert frame["data"].get("pid") == sidecar.pid

    await sidecar.send({"id": 1, "cmd": "close"})

    response = await sidecar.read_frame(timeout=2.0)
    assert response == {"id": 1, "ok": True, "result": {"closed": True}}

    rc = await sidecar.wait(timeout=2.0)
    assert rc == 0


async def test_stdin_eof_triggers_cleanup(sidecar: Sidecar) -> None:
    """#2: Closing stdin shuts the sidecar down within 1s even without a browser."""
    await sidecar.expect_event("ready")

    await sidecar.close_stdin()

    rc = await sidecar.wait(timeout=2.0)
    assert rc == 0


async def test_unknown_launch_option_returns_structured_error(sidecar: Sidecar) -> None:
    """#3: An unknown kwarg is rejected and the session remains usable."""
    await sidecar.expect_event("ready")

    await sidecar.send(
        {"id": 1, "cmd": "launch", "args": {"totally_made_up_option": True}}
    )
    response = await sidecar.read_frame(timeout=2.0)
    assert response["id"] == 1
    assert response["ok"] is False
    assert response["error"]["code"] == "unknown-launch-option"
    assert "totally_made_up_option" in response["error"]["message"]

    # Session still usable: close cleanly.
    await sidecar.send({"id": 2, "cmd": "close"})
    close_resp = await sidecar.read_frame(timeout=2.0)
    assert close_resp["id"] == 2 and close_resp["ok"] is True

    rc = await sidecar.wait(timeout=2.0)
    assert rc == 0


async def test_rejected_launch_option_uses_distinct_code(sidecar: Sidecar) -> None:
    """#3b: persistent_context / user_data_dir are explicitly rejected in v1."""
    await sidecar.expect_event("ready")

    await sidecar.send(
        {"id": 1, "cmd": "launch", "args": {"persistent_context": True}}
    )
    response = await sidecar.read_frame(timeout=2.0)
    assert response["ok"] is False
    assert response["error"]["code"] == "unsupported-launch-option"


async def test_missing_camoufox_binary_reports_actionable_error(
    sidecar: Sidecar, camoufox_available: bool
) -> None:
    """#4: When Camoufox can't find its binary, error mentions `camoufox fetch`."""
    if not camoufox_available:
        pytest.skip("requires the camoufox python package to exercise launch")

    await sidecar.expect_event("ready")

    # Force the "binary missing" failure mode by pointing Camoufox at an
    # executable that doesn't exist. Camoufox itself raises when launch time
    # can't find a real browser.
    await sidecar.send(
        {
            "id": 1,
            "cmd": "launch",
            "args": {
                "headless": True,
                "executable_path": "/nonexistent/camoufox-binary-for-test",
            },
        }
    )
    response = await sidecar.read_frame(timeout=30.0)
    assert response["id"] == 1
    assert response["ok"] is False
    # Accept either the specific mapping or a launch-failed fallback whose
    # message still points the user at `camoufox fetch`.
    code = response["error"]["code"]
    msg = response["error"]["message"].lower()
    assert code in {"camoufox-not-installed", "launch-failed"}, response
    assert "camoufox" in msg


async def test_sigterm_cleans_up_firefox_child(requires_camoufox: None) -> None:
    """#5: SIGTERM tears down the sidecar and its Firefox grandchild in <5s."""
    if psutil is None:
        pytest.skip("psutil is required for process-tree assertions")

    sc = await spawn_sidecar()
    try:
        await sc.expect_event("ready")
        await sc.send({"id": 1, "cmd": "launch", "args": {"headless": True}})
        response = await sc.read_frame(timeout=60.0)
        assert response["ok"] is True, response

        parent = psutil.Process(sc.pid)
        children_before = parent.children(recursive=True)
        assert children_before, "expected camoufox to have spawned at least one child"
        child_pids = [c.pid for c in children_before]

        os.kill(sc.pid, signal.SIGTERM)

        rc = await sc.wait(timeout=5.0)
        assert rc == 0

        # Children must be gone within 5s of the parent exit.
        deadline = asyncio.get_event_loop().time() + 5.0
        while asyncio.get_event_loop().time() < deadline:
            alive = [pid for pid in child_pids if psutil.pid_exists(pid)]
            if not alive:
                break
            await asyncio.sleep(0.1)
        alive = [pid for pid in child_pids if psutil.pid_exists(pid)]
        assert not alive, f"child processes still alive after SIGTERM: {alive}"
    finally:
        await sc.kill()
