"""JSON-line stdio protocol used by the Rust daemon <-> Python sidecar.

Frames are single-line JSON documents. Requests and responses carry a
monotonic `id`; events are unsolicited and carry no `id`.

    request:   {"id": 42, "cmd": "<name>", "args": {...}}
    response:  {"id": 42, "ok": true,  "result": {...}}
               {"id": 42, "ok": false, "error": {"code": "...", "message": "..."}}
    event:     {"event": "<name>", "data": {...}}

stdout is reserved for these frames. stderr is free-form diagnostic logging
that the Rust side captures when --verbose is on.
"""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any, AsyncIterator, Optional


async def _stdin_reader() -> asyncio.StreamReader:
    """Attach an asyncio StreamReader to sys.stdin."""
    loop = asyncio.get_event_loop()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    await loop.connect_read_pipe(lambda: protocol, sys.stdin)
    return reader


class Protocol:
    """Async JSON-line protocol bound to stdin/stdout.

    Writes are synchronous and flushed — correctness beats throughput here,
    since the Rust side relies on line-boundary framing and the volume is low.
    """

    def __init__(self) -> None:
        self._reader: Optional[asyncio.StreamReader] = None
        self._write_lock = asyncio.Lock()

    async def start(self) -> None:
        if self._reader is None:
            self._reader = await _stdin_reader()

    async def messages(self) -> AsyncIterator[dict]:
        """Yield incoming frames until stdin EOF.

        Malformed lines are reported back as a response with
        {"code": "invalid-frame"} when they carry an id, and logged to stderr
        when they do not. The iterator itself does not raise on parse errors.
        """
        assert self._reader is not None, "Protocol.start() must be called first"
        while True:
            raw = await self._reader.readline()
            if not raw:
                return
            line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
            if not line.strip():
                continue
            try:
                frame = json.loads(line)
            except json.JSONDecodeError as exc:
                log(f"invalid JSON on stdin: {exc}: {line!r}")
                await self.write_response(
                    req_id=None,
                    ok=False,
                    error={
                        "code": "invalid-frame",
                        "message": f"could not parse JSON: {exc}",
                    },
                )
                continue
            if not isinstance(frame, dict):
                log(f"non-object frame on stdin: {line!r}")
                continue
            yield frame

    async def write_event(self, name: str, data: Optional[dict] = None) -> None:
        await self._write({"event": name, "data": data or {}})

    async def write_response(
        self,
        req_id: Optional[int],
        ok: bool,
        result: Optional[Any] = None,
        error: Optional[dict] = None,
    ) -> None:
        frame: dict[str, Any] = {"id": req_id, "ok": ok}
        if ok:
            frame["result"] = result if result is not None else {}
        else:
            frame["error"] = error or {"code": "unknown", "message": ""}
        await self._write(frame)

    async def _write(self, frame: dict) -> None:
        encoded = json.dumps(frame, separators=(",", ":"), ensure_ascii=False)
        async with self._write_lock:
            sys.stdout.write(encoded + "\n")
            sys.stdout.flush()


def log(message: str) -> None:
    """Diagnostic logging. Goes to stderr; never touches the protocol pipe."""
    sys.stderr.write(f"[camoufox-sidecar] {message}\n")
    sys.stderr.flush()
