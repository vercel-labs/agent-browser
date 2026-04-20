"""Shared pytest helpers for the sidecar test suite.

The helpers launch the real `camoufox_sidecar` module as a subprocess and
talk to it over stdio. Tests that need an actual Camoufox browser are gated
on the `camoufox_available` fixture so the suite remains informative when
run on a machine that only has the sidecar package installed.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import sys
from dataclasses import dataclass
from typing import Any, AsyncIterator, Optional

import pytest


SIDECAR_MODULE = "camoufox_sidecar"


def _camoufox_importable() -> bool:
    return importlib.util.find_spec("camoufox") is not None


@pytest.fixture
def camoufox_available() -> bool:
    return _camoufox_importable()


@pytest.fixture
def requires_camoufox(camoufox_available: bool) -> None:
    if not camoufox_available:
        pytest.skip("camoufox package not installed")


@dataclass
class Sidecar:
    proc: asyncio.subprocess.Process

    @property
    def pid(self) -> int:
        return self.proc.pid

    async def read_frame(self, timeout: float = 5.0) -> dict:
        assert self.proc.stdout is not None
        line = await asyncio.wait_for(self.proc.stdout.readline(), timeout=timeout)
        if not line:
            raise RuntimeError("sidecar closed stdout before sending a frame")
        return json.loads(line.decode("utf-8"))

    async def expect_event(self, name: str, timeout: float = 5.0) -> dict:
        frame = await self.read_frame(timeout=timeout)
        assert frame.get("event") == name, f"expected event {name!r}, got {frame!r}"
        return frame

    async def send(self, frame: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
        await self.proc.stdin.drain()

    async def close_stdin(self) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.close()
        try:
            await self.proc.stdin.wait_closed()
        except Exception:  # noqa: BLE001
            pass

    async def wait(self, timeout: float = 5.0) -> int:
        return await asyncio.wait_for(self.proc.wait(), timeout=timeout)

    async def kill(self) -> None:
        if self.proc.returncode is None:
            self.proc.kill()
            try:
                await asyncio.wait_for(self.proc.wait(), timeout=5.0)
            except asyncio.TimeoutError:
                pass


async def spawn_sidecar(env: Optional[dict] = None) -> Sidecar:
    env_vars = {**os.environ, **(env or {})}
    proc = await asyncio.create_subprocess_exec(
        sys.executable,
        "-u",
        "-m",
        SIDECAR_MODULE,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env=env_vars,
    )
    return Sidecar(proc=proc)


@pytest.fixture
async def sidecar() -> AsyncIterator[Sidecar]:
    sc = await spawn_sidecar()
    try:
        yield sc
    finally:
        await sc.kill()
