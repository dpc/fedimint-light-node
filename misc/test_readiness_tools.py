"""Offline regression tests for evidence-tool safety, not production node tests."""

import importlib.util
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

from readiness_watchdog import run_owned
from readiness_monitor import SafetyMonitor
from readiness_logging import node_event
import readiness_watchdog

spec = importlib.util.spec_from_file_location(
    "capture", pathlib.Path(__file__).with_name("capture-readiness-headers.py")
)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


class EvidenceSafety(unittest.TestCase):
    def test_tracing_event_sampling(self):
        self.assertEqual(node_event(
            '2026-10-19T00:00:00Z INFO fedimint_light_node::operations: '
            'RPC listener ready event="rpc_listening" rpc_listen=127.0.0.1:1234 '
            'rpc_no_auth=true fake_bitcoind=false submission_only=false\n'),
            {"event": "rpc_listening", "address": "127.0.0.1:1234"})
        self.assertEqual(node_event(
            'INFO fedimint_light_node::sync_progress: selected chain summary '
            'event="sync_progress" selected_height=42 connections=3 candidates=10\n'),
            {"event": "sync_progress", "connections": 3})
        self.assertIsNone(node_event('WARN event="sync_degraded" connections=0'))
        self.assertIsNone(node_event('sync: connections=4 visit=Ok(Empty)'))

    def fuzz_failure(self, wall_seconds):
        """Use tiny local executables to check fail-closed campaign accounting."""
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("block", "transaction"):
                binary = root / name
                binary.write_text(f"#!{sys.executable}\nraise SystemExit(7)\n")
                binary.chmod(0o700)
                corpus = root / "corpus" / name
                corpus.mkdir(parents=True)
                (corpus / "seed").write_bytes(b"x")
            result = subprocess.run(
                [sys.executable, str(pathlib.Path(__file__).with_name("readiness-fuzz.py")),
                 "--directory", directory, "--cpu-seconds", "1",
                 "--wall-seconds", str(wall_seconds), "--chunk-seconds", "1"],
                capture_output=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            records = [json.loads(line) for line in (root / "campaign.jsonl").read_text().splitlines()]
            self.assertEqual(records[-1]["outcome"], "FAIL")
            self.assertEqual(records[-1]["completed_targets"], 1)
            self.assertFalse(any(r.get("target") == "transaction" for r in records))
            return records

    def test_fuzz_failure_does_not_start_second_target(self):
        records = self.fuzz_failure(30)
        self.assertEqual(records[1]["exit"], 7)
        self.assertEqual(records[-2]["successful_runs"], 0)

    def test_fuzz_wall_budget_is_not_a_cpu_pass(self):
        records = self.fuzz_failure(10)
        self.assertIn("wall budget", records[-2]["reason"])
        self.assertEqual(records[-2]["chunks"], 0)

    def test_parent_death_before_arming_refuses_exec(self):
        command = readiness_watchdog.owned_command(
            [sys.executable, "-c", "raise SystemExit(42)"]
        )
        command[2] = "0"  # Never the real parent; model the pre-arm parent-death race.
        result = subprocess.run(command, capture_output=True, timeout=3)
        self.assertNotEqual(result.returncode, 42)
        self.assertIn(b"parent exited", result.stderr)

    def test_killed_supervisor_kills_worker_and_its_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            node = root / "node"
            worker = root / "worker"
            child_code = (
                "import os,pathlib,time;"
                f"pathlib.Path({str(node)!r}).write_text(str(os.getpid()));"
                "time.sleep(30)"
            )
            worker_code = (
                "import os,pathlib,subprocess,sys,time;"
                "from readiness_watchdog import owned_command;"
                f"pathlib.Path({str(worker)!r}).write_text(str(os.getpid()));"
                f"p=subprocess.Popen(owned_command([sys.executable,'-c',{child_code!r}]));"
                "p.wait()"
            )
            supervisor = subprocess.Popen(
                [sys.executable, "-c",
                 "import sys;from readiness_watchdog import run_owned;"
                 f"run_owned([sys.executable,'-c',{worker_code!r}],30)"],
                env=dict(os.environ, PYTHONPATH=str(pathlib.Path(__file__).parent.resolve())),
            )
            pids = []
            try:
                deadline = time.monotonic() + 3
                while not node.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(node.exists(), "nested child did not start")
                pids = [int(worker.read_text()), int(node.read_text())]
                supervisor.kill()  # No finally block or cooperative signal handler.
                supervisor.wait(timeout=3)
                def alive(pid):
                    try:
                        return pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
                    except (FileNotFoundError, ProcessLookupError):
                        return False
                deadline = time.monotonic() + 3
                while any(alive(pid) for pid in pids) and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertFalse(any(alive(pid) for pid in pids))
            finally:
                if supervisor.poll() is None:
                    supervisor.kill()
                    supervisor.wait()
                for pid in pids:
                    try:
                        os.kill(pid, 9)
                    except ProcessLookupError:
                        pass

    def test_recursive_monitor_includes_nested_store(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "nested").mkdir()
            with (root / "nested" / "store").open("wb") as output:
                output.truncate(2 * 1024**3 + 1)  # Sparse: no multi-GiB allocation.
            process = subprocess.Popen([sys.executable, "-c", "import time;time.sleep(30)"])
            monitor = SafetyMonitor(
                lambda: process, root, lambda: 0, 1, threading.Event(), recursive=True
            )
            try:
                monitor.start()
                process.wait(timeout=3)
                self.assertEqual(process.returncode, -9)
                self.assertIn("disk", monitor.error)
                self.assertGreater(monitor.peaks["disk_bytes"], 2 * 1024**3)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                monitor.close()

    def test_monitor_confirms_zombie_exit_despite_busy_waiter(self):
        """Exercise real zombie procfs permissions and Popen's poll lock race."""
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.Popen([sys.executable, "-c", "pass"])
            os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOWAIT)
            denied = threading.Event()
            original = pathlib.Path.iterdir

            def observe(path):
                try:
                    yield from original(path)
                except PermissionError:
                    if str(path) == f"/proc/{process.pid}/fd":
                        denied.set()
                    raise

            monitor = SafetyMonitor(
                lambda: process, pathlib.Path(directory), lambda: 0, 1, threading.Event()
            )
            # Model a concurrent Popen.wait() holding its internal lock before
            # publishing returncode. poll() deliberately cannot decide yet.
            process._waitpid_lock.acquire()
            lock_held = True
            try:
                self.assertIsNone(process.poll())
                with mock.patch.object(pathlib.Path, "iterdir", observe):
                    monitor.start()
                    self.assertTrue(denied.wait(timeout=3), "zombie fd access was not denied")
                    process._waitpid_lock.release()
                    lock_held = False
                    self.assertEqual(process.wait(timeout=3), 0)
                    monitor.close()
                    self.assertIsNone(monitor.error)
            finally:
                if lock_held:
                    process._waitpid_lock.release()
                process.wait(timeout=3)
                monitor.close()

    def test_monitor_kills_live_unobservable_child(self):
        """A real non-dumpable child must not get a permission-error exemption."""
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            ready = root / "ready"
            process = subprocess.Popen([
                sys.executable, "-c",
                "import ctypes,pathlib,time;"
                "assert ctypes.CDLL(None).prctl(4,0,0,0,0)==0;"
                f"pathlib.Path({str(ready)!r}).touch();"
                "time.sleep(30)",
            ])
            monitor = SafetyMonitor(
                lambda: process, root, lambda: 0, 1, threading.Event()
            )
            try:
                deadline = time.monotonic() + 3
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "non-dumpable child did not start")
                monitor.start()
                self.assertEqual(process.wait(timeout=3), -9)
                monitor.close()
                self.assertIn("Permission denied", monitor.error)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                if monitor.thread.ident is not None:
                    monitor.close()

    def test_monitor_disk_error_remains_fatal_after_child_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.Popen([sys.executable, "-c", "import time;time.sleep(30)"])
            monitor = SafetyMonitor(
                lambda: process, pathlib.Path(directory), lambda: 0, 1, threading.Event()
            )

            def inaccessible_disk():
                process.kill()
                process.wait(timeout=3)
                raise PermissionError("inaccessible evidence directory")

            try:
                with mock.patch.object(monitor, "disk_bytes", inaccessible_disk):
                    monitor.start()
                    process.wait(timeout=3)
                    monitor.close()
                self.assertEqual(monitor.error, "inaccessible evidence directory")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                monitor.close()

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
