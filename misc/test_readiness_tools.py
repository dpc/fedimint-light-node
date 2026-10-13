"""Offline regression tests for evidence-tool safety, not production node tests."""

import importlib.util
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

from readiness_watchdog import run_owned
from readiness_monitor import SafetyMonitor

spec = importlib.util.spec_from_file_location(
    "capture", pathlib.Path(__file__).with_name("capture-readiness-headers.py")
)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


class EvidenceSafety(unittest.TestCase):
    def test_monitor_stops_child_while_foreground_is_blocked(self):
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
            monitor = SafetyMonitor(
                lambda: process, pathlib.Path(directory), lambda: 2, 1, threading.Event()
            )
            try:
                monitor.start()
                # No foreground loop or network cooperation is needed.
                process.wait(timeout=3)
                self.assertEqual(process.returncode, -9)
                self.assertIn("traffic", monitor.error)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                monitor.close()

    def test_trickling_response_cannot_extend_process_deadline(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            listener.settimeout(3)
            stopped = threading.Event()

            def trickle():
                connection, _ = listener.accept()
                with connection:
                    while not stopped.wait(0.03):
                        try:
                            connection.sendall(b"x")
                        except OSError:
                            return

            thread = threading.Thread(target=trickle)
            thread.start()
            code = (
                "import socket;"
                f"s=socket.create_connection({listener.getsockname()!r},timeout=2);"
                "s.makefile('rb').readline(400001)"
            )
            started = time.monotonic()
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    run_owned([sys.executable, "-c", code], 0.3)
                self.assertLess(time.monotonic() - started, 2)
            finally:
                stopped.set()
                thread.join(timeout=3)
            self.assertFalse(thread.is_alive())

    def test_json_capture_and_existing_manifest_are_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "headers.json"
            path.write_bytes(b"capture")
            capture.write_manifest(path, {"sha256": "first"})
            sidecar = pathlib.Path(str(path) + ".json")
            original = sidecar.read_bytes()
            with self.assertRaises(FileExistsError):
                capture.write_manifest(path, {"sha256": "replacement"})
            self.assertEqual(path.read_bytes(), b"capture")
            self.assertEqual(sidecar.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
