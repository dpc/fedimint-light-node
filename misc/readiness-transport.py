#!/usr/bin/env python3
"""Run one offline transport-pressure test with owned deadlines and safety sampling."""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import resource
import subprocess
import sys
import threading

from readiness_monitor import SafetyMonitor
from readiness_watchdog import owned_command, run_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--headers", type=pathlib.Path, required=True)
    parser.add_argument("--block", type=pathlib.Path, required=True)
    parser.add_argument("--directory", type=pathlib.Path, required=True)
    parser.add_argument("--seconds", type=int, default=120)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 30 <= args.seconds <= 600:
        parser.error("finite duration 30..600 seconds required")
    if not args.worker:
        raise SystemExit(run_owned(
            [sys.executable, __file__, *sys.argv[1:], "--worker"], args.seconds + 30
        ))
    directory = args.directory.resolve()
    directory.mkdir(mode=0o700)  # NEW output only; never overwrite a failed run.
    binary = args.binary.resolve()
    headers = args.headers.resolve()
    block = args.block.resolve()
    test = "readiness_transport_tests::default_store_real_pool_and_rpc_pressure"
    listing = subprocess.run(
        owned_command([str(binary), "--ignored", "--exact", test, "--list", "--format", "terse"]),
        check=True, capture_output=True, text=True, timeout=10,
    )
    if listing.stdout.strip() != f"{test}: test":
        raise SystemExit("missing or unexpected selected test")
    # Child-only affinity, not a hostwide setting or a memory/cgroup quota.
    cpus = sorted(os.sched_getaffinity(0))[:2]
    command = [
        "taskset", "-c", ",".join(map(str, cpus)), str(binary), "--ignored",
        "--exact", test, "--nocapture", "--test-threads=1",
    ]
    manifest = {
        "start_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "command": command,
        "seconds": args.seconds,
        "sha256": {},
        "cpu_affinity": cpus,
        "limits": "sampled 1GiB RSS, 256 FD, 2GiB directory; no hard memory quota",
    }
    for path, limit in [(binary, 128 * 1024**2), (headers, 12_775_680), (block, 4_000_000)]:
        if path.stat().st_size > limit:
            raise SystemExit(f"input exceeds bound: {path}")
        manifest["sha256"][str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    (directory / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    env = dict(os.environ, READINESS_HEADERS=str(headers), READINESS_BLOCK=str(block),
               TMPDIR=str(directory))
    with (directory / "test.log").open("xb") as log:
        process = subprocess.Popen(owned_command(command), env=env, stdout=log, stderr=subprocess.STDOUT)
        monitor = SafetyMonitor(lambda: process, directory, lambda: 0, 1, threading.Event(), recursive=True)
        code, reason = None, "incomplete"
        try:
            monitor.start()
            code = process.wait(timeout=args.seconds)
            reason = "test completed" if code == 0 else "test failed"
        except subprocess.TimeoutExpired:
            reason = "test duration exceeded"
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            monitor.close()
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    result = {
        "outcome": "PASS" if code == 0 and monitor.error is None else "FAIL",
        "reason": monitor.error or reason,
        "exit": code,
        "sampled_peaks": monitor.peaks,
        "child_user_seconds": usage.ru_utime,
        "child_system_seconds": usage.ru_stime,
        "child_maxrss_kib": usage.ru_maxrss,
    }
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    raise SystemExit(0 if result["outcome"] == "PASS" else 1)


if __name__ == "__main__":
    main()
