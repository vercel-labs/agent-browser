#!/usr/bin/env python3
"""Observe Claude lifecycle events without changing tool decisions or context."""
import json
import os
from pathlib import Path
import sys
import time


def append_event(path, event):
    data = (json.dumps(event) + "\n").encode()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        os.write(descriptor, data)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    event = json.load(sys.stdin)
    event["observed_at_ns"] = time.time_ns()
    append_event(Path(sys.argv[1]), event)
    print("{}")
