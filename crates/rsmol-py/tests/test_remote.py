"""Protocol tests for the app client (rsmol._remote) against a fake app, no GPU or network."""

import json
import os
import socket
import tempfile
import threading
import unittest

from rsmol._remote import RemoteSession


class FakeApp:
    """Answers each JSON request with a canned response and records what it was asked."""

    def __init__(self, responses):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "app.sock")
        self.requests = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(1)
        threading.Thread(target=self._serve, args=(list(responses),), daemon=True).start()

    def _serve(self, responses):
        conn, _ = self.server.accept()
        with conn, conn.makefile("rwb") as f:
            for response in responses:
                self.requests.append(json.loads(f.readline()))
                f.write(json.dumps(response).encode() + b"\n")
                f.flush()


class RemoteTest(unittest.TestCase):
    def test_calls_and_results(self):
        app = FakeApp([{"ok": True, "result": 236}, {"ok": True, "result": "1HSG"}])
        s = RemoteSession(app.path)
        self.assertEqual(s.count_atoms("pocket"), 236)
        self.assertEqual(s.load("x.cif"), "1HSG")
        self.assertEqual(app.requests[0], {"method": "count_atoms", "params": {"selection": "pocket"}})
        # Paths are made absolute: the app's working directory differs from the script's.
        self.assertTrue(os.path.isabs(app.requests[1]["params"]["path"]))

    def test_errors_map_to_python_exceptions(self):
        app = FakeApp([
            {"ok": False, "kind": "value", "error": "bad selection"},
            {"ok": False, "kind": "io", "error": "no such file"},
            {"ok": False, "kind": "not_implemented", "error": "png"},
        ])
        s = RemoteSession(app.path)
        with self.assertRaisesRegex(ValueError, "bad selection"):
            s.do("select x, nope")
        with self.assertRaises(OSError):
            s.load("missing.cif")
        with self.assertRaises(NotImplementedError):
            s.png("x.png")


if __name__ == "__main__":
    unittest.main()
