"""Login sessions in progress (waiting for a 2FA code), kept in memory only."""

from __future__ import annotations

import threading
from typing import Any

_sessions: dict[int, Any] = {}
_lock = threading.Lock()


def get(connection_id: int) -> Any:
    with _lock:
        return _sessions.get(connection_id)


def put(connection_id: int, value: Any) -> None:
    with _lock:
        _sessions[connection_id] = value


def drop(connection_id: int) -> None:
    with _lock:
        _sessions.pop(connection_id, None)


def clear() -> None:
    with _lock:
        _sessions.clear()
