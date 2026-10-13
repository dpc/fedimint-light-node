"""Independent Linux safety sampling for one explicitly owned evidence child."""

import pathlib
import threading
import os


class SafetyMonitor:
    """Stop the node without waiting for foreground RPC or reference requests."""

    def __init__(self, process, directory, traffic, byte_stop, overflow, recursive=False):
        self.process = process
        self.directory = directory
        self.traffic = traffic
        self.byte_stop = byte_stop
        self.overflow = overflow
        self.recursive = recursive
        self.error = None
        self.latest = {}
        self.peaks = {"rss_kib": 0, "hwm_kib": 0, "fd": 0, "sockets": 0, "disk_bytes": 0}
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run)

    def run(self):
        while not self.done.wait(0.25):
            child = self.process()
            if child is None or child.poll() is not None:
                continue
            try:
                status = pathlib.Path(f"/proc/{child.pid}/status").read_text()
                memory = {}
                for line in status.splitlines():
                    if line.startswith(("VmRSS:", "VmHWM:")):
                        memory[line.split(":")[0]] = int(line.split()[1])
                descriptors = list(pathlib.Path(f"/proc/{child.pid}/fd").iterdir())
                sockets = 0
                for descriptor in descriptors:
                    try:
                        sockets += os.readlink(descriptor).startswith("socket:")
                    except FileNotFoundError:
                        pass  # A concurrently closed descriptor is not a monitor failure.
                ticks = pathlib.Path(f"/proc/{child.pid}/stat").read_text().rsplit(")", 1)[1].split()
                snapshot = {
                    "pid": child.pid,
                    "rss_kib": memory.get("VmRSS", 0),
                    "hwm_kib": memory.get("VmHWM", 0),
                    "fd": len(descriptors),
                    "sockets": sockets,
                    "user_seconds": int(ticks[11]) / os.sysconf("SC_CLK_TCK"),
                    "system_seconds": int(ticks[12]) / os.sysconf("SC_CLK_TCK"),
                    "disk_bytes": self.disk_bytes(),
                    "namespace_network_bytes": self.traffic(),
                }
                self.latest = snapshot
                for key in self.peaks:
                    self.peaks[key] = max(self.peaks[key], snapshot[key])
                if not 0 <= snapshot["namespace_network_bytes"] < self.byte_stop:
                    raise RuntimeError("namespace traffic counter/budget safety stop")
                if snapshot["rss_kib"] > 1024**2 or snapshot["fd"] > 256 or snapshot["disk_bytes"] > 2 * 1024**3:
                    raise RuntimeError("RSS/FD/disk safety stop")
                if self.overflow.is_set():
                    raise RuntimeError("log queue safety stop")
            except Exception as error:
                # A process can disappear during a planned restart; do not mistake
                # that for a sampling failure. All other sampling errors fail closed.
                if child.poll() is not None:
                    continue
                self.error = str(error)
                child.kill()
                child.wait(timeout=10)
                return

    def disk_bytes(self):
        """Count disposable files, including nested test stores when requested."""
        paths = self.directory.rglob("*") if self.recursive else self.directory.iterdir()
        total = 0
        for path in paths:
            try:
                if path.is_file():
                    total += path.stat().st_size
            except FileNotFoundError:
                pass  # A disposable temporary file can disappear during sampling.
        return total

    def start(self):
        self.thread.start()

    def close(self):
        self.done.set()
        self.thread.join(timeout=15)
        if self.thread.is_alive():
            raise RuntimeError("safety monitor did not join")
