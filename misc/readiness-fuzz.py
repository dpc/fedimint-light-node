#!/usr/bin/env python3
"""Sequential, measured-child-CPU sanitizer campaigns on pinned offline inputs."""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import resource
import subprocess
import sys
import threading
import time

from readiness_monitor import SafetyMonitor
from readiness_watchdog import owned_command, run_owned


def digest(path):
    """Hash a bounded pinned input without retaining another binary-sized buffer."""
    if not path.is_file() or path.stat().st_size > 128 * 1024**2:
        raise ValueError(f"not a bounded regular input: {path}")
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def cpu_usage():
    """Reaped direct-child CPU, including startup, corpus loading and system work."""
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime, usage.ru_stime


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", required=True, type=pathlib.Path)
    parser.add_argument("--cpu-seconds", type=int, default=14400)
    parser.add_argument("--wall-seconds", type=int, default=21600)
    parser.add_argument("--chunk-seconds", type=int, default=300)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 1 <= args.cpu_seconds <= 14400:
        parser.error("CPU target must be 1..14400 seconds per target")
    if not 10 <= args.wall_seconds <= 21600 or not 1 <= args.chunk_seconds <= 300:
        parser.error("wall limit 10..21600 and chunk size 1..300 seconds required")
    if not args.worker:
        raise SystemExit(run_owned(
            [sys.executable, __file__, *sys.argv[1:], "--worker"],
            2 * args.wall_seconds + 60,
        ))
    directory = args.directory.resolve()
    # Preparation copies immutable binaries and corpora into a new private
    # directory. Refuse reuse of a completed or interrupted campaign.
    log = (directory / "campaign.jsonl").open("x")

    def record(value):
        log.write(json.dumps(value) + "\n")
        log.flush()
        os.fsync(log.fileno())

    targets = [("block", 604870434, 4_000_000), ("transaction", 3402604921, 400_000)]
    cpu = min(os.sched_getaffinity(0))
    record({
        "start_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "cpu_target_seconds": args.cpu_seconds,
        "per_target_wall_seconds": args.wall_seconds,
        "chunk_wall_seconds": args.chunk_seconds,
        "cpu_affinity": [cpu], "nice": 10,
        "cpu_scope": "reaped child user+system including wrapper/startup/corpus; not pure mutation CPU",
        "limits": "512MiB libFuzzer RSS; sampled 1GiB RSS/256FD/2GiB recursive disk; no hard memory quota",
        "binary_sha256": {name: digest(directory / name) for name, _, _ in targets},
        "scripts_sha256": {
            name: digest(pathlib.Path(__file__).with_name(name))
            for name in ["readiness-fuzz.py", "readiness_monitor.py", "readiness_watchdog.py"]
        },
    })
    outcomes = []
    for name, seed, max_len in targets:
        corpus = directory / "corpus" / name
        if not corpus.is_dir() or not any(corpus.iterdir()):
            raise ValueError(f"missing nonempty prepared corpus: {corpus}")
        artifacts = directory / "artifacts" / name
        artifacts.mkdir(parents=True)
        started = time.monotonic()
        user_cpu = system_cpu = 0.0
        runs = 0
        chunk = 0
        process = None
        monitor = SafetyMonitor(
            lambda: process, directory, lambda: 0, 1, threading.Event(), recursive=True
        )
        outcome, reason = "FAIL", "incomplete"
        try:
            monitor.start()
            while user_cpu + system_cpu < args.cpu_seconds:
                remaining = args.wall_seconds - (time.monotonic() - started)
                if remaining < args.chunk_seconds + 15:
                    raise RuntimeError("per-target wall budget exhausted before CPU target")
                chunk += 1
                command = [
                    "taskset", "-c", str(cpu), "nice", "-n", "10", str(directory / name),
                    str(corpus), f"-artifact_prefix={artifacts}/",
                    f"-seed={seed}", f"-max_total_time={args.chunk_seconds}",
                    "-rss_limit_mb=512", "-timeout=5", f"-max_len={max_len}",
                ]
                before = cpu_usage()
                path = directory / f"{name}-{chunk:03}.log"
                with path.open("xb") as output:
                    process = subprocess.Popen(
                        owned_command(command), stdout=output, stderr=subprocess.STDOUT,
                        env=dict(os.environ, ASAN_OPTIONS="abort_on_error=1:detect_leaks=1"),
                    )
                    code = process.wait(timeout=min(remaining, args.chunk_seconds + 15))
                after = cpu_usage()
                user_cpu += after[0] - before[0]
                system_cpu += after[1] - before[1]
                if path.stat().st_size > 16 * 1024**2:
                    raise RuntimeError("chunk diagnostic log exceeds bounded parser limit")
                text = path.read_text(errors="replace")
                summary = re.search(r"Done (\d+) runs in (\d+) second", text)
                record({
                    "target": name, "chunk": chunk, "exit": code,
                    "user_cpu_seconds": user_cpu, "system_cpu_seconds": system_cpu,
                    "wall_seconds": time.monotonic() - started,
                    "runs": int(summary[1]) if summary else None,
                    "sampled_peaks": monitor.peaks.copy(), "command": command,
                })
                if monitor.error or code != 0 or not summary:
                    raise RuntimeError(monitor.error or "fuzzer failed or lacks completion summary")
                runs += int(summary[1])
            outcome, reason = "PASS", "measured child CPU target reached in successful chunks"
        except Exception as error:
            reason = str(error)
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
            monitor.close()
            if monitor.error:
                outcome, reason = "FAIL", monitor.error
            result = {
                "target": name, "outcome": outcome, "reason": reason,
                "successful_runs": runs, "chunks": chunk,
                "user_cpu_seconds": user_cpu, "system_cpu_seconds": system_cpu,
                "wall_seconds": time.monotonic() - started,
                "sampled_peaks": monitor.peaks.copy(),
            }
            record(result)
            outcomes.append(result)
        if outcome != "PASS":
            break  # No further pressure after a failed resource/sanitizer gate.
    success = len(outcomes) == len(targets) and all(r["outcome"] == "PASS" for r in outcomes)
    record({"outcome": "PASS" if success else "FAIL", "completed_targets": len(outcomes)})
    log.close()
    raise SystemExit(0 if success else 1)


if __name__ == "__main__":
    main()
