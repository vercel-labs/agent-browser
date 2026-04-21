"""Sidecar entry point.

Lifecycle:

    1. Attach to stdin/stdout, emit `{"event": "ready"}`.
    2. Read command frames from stdin; dispatch to Session handlers.
    3. Exit cleanly on stdin EOF, SIGTERM, SIGINT, or `{"cmd": "close"}`.

Unit 2 only ships lifecycle commands (`launch`, `close`). Anything else is
responded to with `not-yet-supported` so agents get a clear signal rather than
silent drops; later units replace those stubs.
"""

from __future__ import annotations

import asyncio
import signal
import sys
from typing import Any, Awaitable, Callable, Optional

from .protocol import Protocol, log
from .session import LaunchError, Session


class Sidecar:
    def __init__(self) -> None:
        self.protocol = Protocol()
        # Hand the protocol to the session so per-page events (console, crash)
        # can fan out to the Rust daemon without the session holding a
        # stdout handle of its own.
        self.session = Session(protocol=self.protocol)
        self._shutdown = asyncio.Event()

    async def run(self) -> int:
        await self.protocol.start()
        _install_signal_handlers(self._shutdown)

        await self.protocol.write_event("ready", {"pid": _own_pid()})

        reader_task = asyncio.create_task(self._read_loop(), name="sidecar-reader")
        shutdown_task = asyncio.create_task(
            self._shutdown.wait(), name="sidecar-shutdown"
        )
        try:
            done, _ = await asyncio.wait(
                {reader_task, shutdown_task},
                return_when=asyncio.FIRST_COMPLETED,
            )
            for task in done:
                exc = task.exception()
                if exc is not None:
                    log(f"sidecar task raised: {exc!r}")
        finally:
            reader_task.cancel()
            shutdown_task.cancel()
            for task in (reader_task, shutdown_task):
                try:
                    await task
                except (asyncio.CancelledError, Exception):  # noqa: BLE001
                    pass
            await self.session.close()
        return 0

    async def _read_loop(self) -> None:
        try:
            async for frame in self.protocol.messages():
                await self._dispatch(frame)
        finally:
            # stdin closed → daemon gone → we shut down
            self._shutdown.set()

    async def _dispatch(self, frame: dict) -> None:
        cmd = frame.get("cmd")
        req_id = frame.get("id")
        args = frame.get("args") or {}

        if cmd == "close":
            await self.protocol.write_response(req_id, ok=True, result={"closed": True})
            self._shutdown.set()
            return

        handler = _HANDLERS.get(cmd)  # type: ignore[arg-type]
        if handler is None:
            await self.protocol.write_response(
                req_id,
                ok=False,
                error={
                    "code": "not-yet-supported" if isinstance(cmd, str) else "invalid-frame",
                    "message": (
                        f"command {cmd!r} is not implemented in this sidecar version"
                        if isinstance(cmd, str)
                        else "frame is missing a 'cmd' field"
                    ),
                },
            )
            return

        try:
            result = await handler(self, args)
        except LaunchError as exc:
            await self.protocol.write_response(
                req_id,
                ok=False,
                error={"code": exc.code, "message": exc.message},
            )
            return
        except Exception as exc:  # noqa: BLE001
            log(f"handler {cmd} raised: {exc!r}")
            await self.protocol.write_response(
                req_id,
                ok=False,
                error={"code": "internal-error", "message": str(exc)},
            )
            return

        await self.protocol.write_response(req_id, ok=True, result=result)


Handler = Callable[["Sidecar", dict], Awaitable[Any]]


async def _cmd_launch(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.launch(args)


async def _cmd_page_goto(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.goto(args)


async def _cmd_page_snapshot(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.snapshot(args)


async def _cmd_page_click(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.click(args)


async def _cmd_page_fill(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.fill(args)


async def _cmd_page_get_text(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.get_text(args)


async def _cmd_page_screenshot(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.screenshot(args)


async def _cmd_tab_new(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.tab_new(args)


async def _cmd_tab_switch(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.tab_switch(args)


async def _cmd_tab_close(sidecar: "Sidecar", args: dict) -> dict:
    return await sidecar.session.tab_close(args)


async def _cmd_tab_list(sidecar: "Sidecar", args: dict) -> dict:
    return sidecar.session.tab_list(args)


_HANDLERS: dict[str, Handler] = {
    "launch": _cmd_launch,
    "page.goto": _cmd_page_goto,
    "page.navigate": _cmd_page_goto,  # alias for CDP-side naming parity
    "page.snapshot": _cmd_page_snapshot,
    "page.click": _cmd_page_click,
    "page.fill": _cmd_page_fill,
    "page.getText": _cmd_page_get_text,
    "page.screenshot": _cmd_page_screenshot,
    "tab.new": _cmd_tab_new,
    "tab.switch": _cmd_tab_switch,
    "tab.close": _cmd_tab_close,
    "tab.list": _cmd_tab_list,
}


def _own_pid() -> int:
    import os

    return os.getpid()


def _install_signal_handlers(shutdown: asyncio.Event) -> None:
    loop = asyncio.get_event_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, shutdown.set)
        except (NotImplementedError, RuntimeError):
            # Windows / non-main thread: fall back to default disposition.
            pass


def main(argv: Optional[list[str]] = None) -> int:
    _ = argv  # reserved for future flags; the sidecar takes config via stdio
    try:
        return asyncio.run(Sidecar().run())
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
