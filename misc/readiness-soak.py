#!/usr/bin/env python3
"""Supervise a finite read-only loopback RPC/mainnet exercise on disposable state."""

import argparse
import base64
import datetime
import hashlib
import http.client
import json
import os
import pathlib
import queue
import resource
import secrets
import signal
import subprocess
import sys
import threading
import time

from readiness_watchdog import owned_command, run_owned
from readiness_monitor import SafetyMonitor
from readiness_logging import node_event


def interface_bytes():
    """Conservative namespace-wide non-loopback counter, not per-process traffic."""
    total = 0
    for line in pathlib.Path("/proc/net/dev").read_text().splitlines()[2:]:
        name, fields = line.split(":")
        if name.strip() != "lo":
            values = fields.split()
            total += int(values[0]) + int(values[8])
    return total


def stop(process, abrupt=False):
    if process.poll() is None:
        process.send_signal(signal.SIGKILL if abrupt else signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--directory", required=True, type=pathlib.Path)
    parser.add_argument("--seconds", type=int, default=600)
    parser.add_argument("--byte-stop", type=int, default=2 * 1024**3)
    parser.add_argument("--restart-seconds", type=int, default=21600)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 60 <= args.seconds <= 72 * 3600 or not 1 <= args.byte_stop <= 6 * 1024**3:
        parser.error("duration 60..259200 and byte-stop 1..6442450944 required")
    if args.restart_seconds < 60:
        parser.error("restart interval must be at least60 seconds")
    if not args.worker:
        raise SystemExit(run_owned(
            [sys.executable, __file__, *sys.argv[1:], "--worker"], args.seconds + 60
        ))
    directory = args.directory.resolve()
    if not (directory / "chain.redb").is_file():
        parser.error("bootstrap a NEW disposable default-capacity store first")
    credential = f"evidence:{secrets.token_hex(32)}"
    auth = directory / "soak.auth"
    with open(auth, "x", opener=lambda path, flags: os.open(path, flags, 0o600)) as output:
        output.write(credential + "\n")
    binary = str(args.binary.resolve())
    binary_digest = hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest()
    command = [
        binary, "--db", str(directory / "chain.redb"), "serve", "--standalone",
        "--fake-bitcoind", "--rpc-bind", "127.0.0.1:0", "--rpc-credentials", str(auth),
    ]
    initial = subprocess.run(
        owned_command([binary, "--db", str(directory / "chain.redb"), "status"]),
        check=True, capture_output=True, text=True, timeout=30,
    ).stdout
    baseline = interface_bytes()
    started = time.monotonic()
    events = queue.Queue(maxsize=256)
    overflow = threading.Event()
    process = None
    reader = None
    log_bytes = 0
    failures = 0
    successes = 0
    references = 0
    reference_attempts = 0
    hash_references = 0
    lag_samples = 0
    bodies = 0
    max_connections = 0
    outcome = "FAIL"
    reason = "incomplete"
    next_rpc = started + 30
    next_body = started + 60
    next_restart = started + args.restart_seconds
    restarts = 0
    address = None
    observed_sync_lines = 0
    connections_histogram = {}
    launched_at = started
    monitor = SafetyMonitor(
        lambda: process, directory, lambda: interface_bytes() - baseline, args.byte_stop, overflow
    )

    def read_lines(child):
        while line := child.stdout.readline(8193):
            if len(line) > 8192:
                overflow.set()
                return
            try:
                events.put_nowait(line)
            except queue.Full:
                overflow.set()
                return

    def launch():
        if hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest() != binary_digest:
            raise RuntimeError("binary changed during campaign; refusing mixed-version restart")
        # The sampling contract requires default info summaries, independent of
        # the invoking shell's logging overrides. No live run is implied here.
        env = dict(os.environ, RUST_LOG="info")
        child = subprocess.Popen(owned_command(command), env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        thread = threading.Thread(target=read_lines, args=(child,), daemon=True)
        thread.start()
        return child, thread

    def rpc(method, params):
        host, port = address.rsplit(":", 1)
        connection = http.client.HTTPConnection(host, int(port), timeout=15)
        try:
            connection.request(
                "POST", "/", json.dumps({"id": 1, "method": method, "params": params}),
                {"Authorization": "Basic " + base64.b64encode(credential.encode()).decode(),
                 "Content-Type": "application/json"},
            )
            response = connection.getresponse()
            body = response.read(8_001_025)
            if response.status != 200 or len(body) > 8_001_024:
                raise ValueError("HTTP status or reply bound")
            result = json.loads(body)
            if result.get("error"):
                raise ValueError(str(result["error"]))
            return result["result"]
        finally:
            connection.close()

    with (directory / "soak.jsonl").open("x") as report, (directory / "soak.log").open("xb") as log:
        def record(value):
            report.write(json.dumps({"elapsed": round(time.monotonic() - started, 3), **value}) + "\n")
            report.flush()

        record({"start_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                "binary_sha256": binary_digest,
                "scripts_sha256": {
                    name: hashlib.sha256(pathlib.Path(__file__).with_name(name).read_bytes()).hexdigest()
                    for name in ("readiness-soak.py", "readiness_monitor.py", "readiness_watchdog.py")
                },
                "seconds": args.seconds, "byte_stop": args.byte_stop,
                "network_counter": "namespace-wide non-loopback RX+TX, sampled stop threshold",
                "initial_status": initial, "command": command})
        try:
            process, reader = launch()
            monitor.start()
            while time.monotonic() - started < args.seconds:
                now = time.monotonic()
                traffic = interface_bytes() - baseline
                if traffic < 0 or traffic >= args.byte_stop or overflow.is_set():
                    raise RuntimeError("traffic counter/budget or log queue limit")
                if monitor.error:
                    raise RuntimeError(monitor.error)
                if process.poll() is not None:
                    raise RuntimeError(f"unexpected node exit {process.returncode}")
                while not events.empty():
                    line = events.get_nowait()
                    text = line.decode(errors="replace")
                    event = node_event(text)
                    if event and event["event"] == "rpc_listening":
                        address = event["address"]
                    if event and event["event"] == "sync_progress":
                        connections = event["connections"]
                        max_connections = max(max_connections, connections)
                        connections_histogram[connections] = connections_histogram.get(connections, 0) + 1
                        observed_sync_lines += 1
                    log_bytes += len(line)
                    if log_bytes > 16 * 1024**2:
                        raise RuntimeError("16MiB log budget")
                    log.write(line)
                    log.flush()
                if address and now >= next_rpc:
                    next_rpc = now + 60
                    sample = dict(monitor.latest)
                    try:
                        height = rpc("getblockcount", [])
                        block_hash = rpc("getblockhash", [height])
                        sample.update(height=height, block_hash=block_hash,
                                      since_restart_seconds=round(now - launched_at, 3))
                        # An external explorer observation, NOT independent Core validation.
                        reference = http.client.HTTPSConnection("mempool.space", timeout=10)
                        try:
                            reference_attempts += 1
                            compare_hash = reference_attempts % 2 == 1
                            reference.request("GET", f"/api/block-height/{height}" if compare_hash else "/api/blocks/tip/height")
                            response = reference.getresponse()
                            expected = response.read(129)
                            if response.status != 200 or len(expected) > 128:
                                raise ValueError("external reference unavailable")
                            if compare_hash:
                                if len(expected) != 64:
                                    raise ValueError("external hash length")
                                if expected.decode() != block_hash:
                                    raise RuntimeError("external reference disagreement; never reanchor")
                                hash_references += 1
                                sample["reference"] = "mempool.space matching-height hash"
                            else:
                                external_tip = int(expected)
                                if not 0 <= external_tip <= 100_000_000:
                                    raise ValueError("external tip range")
                                lag_samples += 1
                                sample.update(external_tip=external_tip, observed_height_lag=external_tip - height,
                                              reference="mempool.space tip observation, not validation")
                            references += 1
                        finally:
                            reference.close()
                        if now >= next_body:
                            next_body = now + 600
                            raw = bytes.fromhex(rpc("getblock", [block_hash, 0]))
                            if len(raw) < 80 or hashlib.sha256(hashlib.sha256(raw[:80]).digest()).digest()[::-1].hex() != block_hash:
                                raise RuntimeError("RPC body header mismatch")
                            bodies += 1
                            sample.update(body_bytes=len(raw), body_sha256=hashlib.sha256(raw).hexdigest())
                        successes += 1
                    except (OSError, ValueError, http.client.HTTPException) as error:
                        failures += 1
                        sample["temporary_failure"] = str(error)
                    record(sample)
                if now >= next_restart:
                    stop(process, abrupt=restarts % 2 == 1)
                    reader.join(timeout=2)
                    restarts += 1
                    record({"restart": restarts, "previous_exit": process.returncode})
                    address = None
                    process, reader = launch()
                    launched_at = time.monotonic()
                    next_restart = time.monotonic() + args.restart_seconds
                time.sleep(1)
            if successes == 0 or hash_references == 0 or lag_samples == 0 or bodies == 0 or max_connections < 4:
                raise RuntimeError("insufficient successful RPC/body/reference/four-peer evidence")
            outcome, reason = "PASS", "completed finite observation; not production certification"
        except Exception as error:
            reason = str(error)
        finally:
            if process is not None:
                stop(process)
            if monitor.thread.ident is not None:
                monitor.close()
            if monitor.error:
                outcome, reason = "FAIL", monitor.error
            if reader is not None:
                reader.join(timeout=2)
            final = subprocess.run(
                owned_command([binary, "--db", str(directory / "chain.redb"), "status"]),
                capture_output=True, text=True, timeout=30,
            )
            for prefix in ("network=", "checkpoint_height=", "checkpoint_hash=", "protected_height=",
                           "protected_hash=", "height_one_hash=", "guardian_id=", "capacity="):
                old = [s for s in initial.splitlines() if s.startswith(prefix)]
                new = [s for s in final.stdout.splitlines() if s.startswith(prefix)]
                if not old or old != new or final.returncode:
                    outcome, reason = "FAIL", "offline identity/protection/reopen mismatch"
            record({"outcome": outcome, "reason": reason, "successes": successes,
                    "temporary_failures": failures, "references": references, "bodies": bodies,
                    "hash_references": hash_references, "lag_samples": lag_samples,
                    "peaks": monitor.peaks, "max_connections": max_connections,
                    "observed_sync_lines": observed_sync_lines,
                    "sync_sampling": "tracing sync_progress selection changes and 60s summaries",
                    "connections_histogram": connections_histogram,
                    "restarts": restarts, "final_status": final.stdout,
                    "child_user_seconds": resource.getrusage(resource.RUSAGE_CHILDREN).ru_utime,
                    "child_system_seconds": resource.getrusage(resource.RUSAGE_CHILDREN).ru_stime,
                    "namespace_network_bytes": interface_bytes() - baseline})
            auth.unlink()
    print(f"{outcome}: {reason}; report={directory / 'soak.jsonl'}")
    raise SystemExit(0 if outcome == "PASS" else 1)


if __name__ == "__main__":
    main()
