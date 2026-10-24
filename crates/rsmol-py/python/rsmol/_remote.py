"""Client for a running rsmol app.

When the app runs a script it sets RSMOL_APP_SOCKET; `rsmol.cmd` then talks to the live
session over that socket instead of a headless one, so the script drives the open window.
Each call is one JSON line: {"method": ..., "params": {...}} answered by {"ok": ..., ...}.
"""

from __future__ import annotations

import json
import os
import socket
from typing import Any

_ERRORS = {"value": ValueError, "io": OSError, "runtime": RuntimeError, "not_implemented": NotImplementedError}


class RemoteSession:
    """Same methods as rsmol._native.Session, executed by the app."""

    def __init__(self, path: str):
        self._socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._socket.connect(path)
        self._file = self._socket.makefile("rwb")

    def _call(self, method: str, **params: Any) -> Any:
        self._file.write(json.dumps({"method": method, "params": params}).encode() + b"\n")
        self._file.flush()
        line = self._file.readline()
        if not line:
            raise RuntimeError("the rsmol app closed the connection")
        response = json.loads(line)
        if not response["ok"]:
            raise _ERRORS.get(response.get("kind"), RuntimeError)(response["error"])
        return response["result"]

    def load(self, path, object=None) -> str:
        return self._call("load", path=os.path.abspath(path), object=object)

    def fetch(self, code: str, object=None) -> str:
        return self._call("fetch", code=code, object=object)

    def do(self, command: str) -> str:
        return self._call("do", command=command)

    def run(self, command: str) -> dict[str, Any]:
        return self._call("run", command=command)

    def count_atoms(self, selection: str = "all") -> int:
        return self._call("count_atoms", selection=selection)

    def get_coords(self, selection: str = "all") -> list[list[float]]:
        return self._call("get_coords", selection=selection)

    def get_atoms(self, selection: str = "all") -> list[dict[str, Any]]:
        return self._call("get_atoms", selection=selection)

    def get_names(self) -> list[str]:
        return self._call("get_names")

    def get_measurements(self) -> list[dict[str, Any]]:
        return self._call("get_measurements")

    def alter(self, changes: list[tuple[int, str, str]]) -> int:
        return self._call("alter", changes=changes)

    def export(self, path, selection: str = "all") -> None:
        self._call("export", path=os.path.abspath(path), selection=selection)

    def save(self, path) -> None:
        self._call("save", path=os.path.abspath(path))

    def turn(self, axis: str, angle: float) -> None:
        self._call("turn", axis=axis, angle=angle)

    def reset(self) -> None:
        self._call("reset")

    def quit(self) -> bool:
        # The app's window belongs to the user; a script ending does not close it.
        return True

    def png(self, path, width: int = 1600, height: int = 1000) -> None:
        self._call("png", path=os.path.abspath(path), width=width, height=height)
