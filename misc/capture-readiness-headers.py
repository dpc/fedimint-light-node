#!/usr/bin/env python3
"""Capture bounded public header evidence, never production checkpoint authority."""

import argparse
import datetime
import hashlib
import json
import pathlib
import socket
import ssl
import sys
import time

from readiness_watchdog import run_owned


def write_manifest(path, manifest):
    """Publish a distinct exclusive sidecar, never truncate capture or old metadata."""
    with pathlib.Path(str(path) + ".json").open("x") as output:
        output.write(json.dumps(manifest, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument("--host", default="electrum.blockstream.info")
    parser.add_argument("--start", type=int, default=810432)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    # One context period plus three default retained windows: about 12.8 MB raw.
    count = 2016 + 3 * 52560
    if args.start < 0 or args.start % 2016:
        parser.error("start must be a nonnegative retarget boundary")
    if not args.worker:
        # A socket inactivity timeout cannot bound a trickling peer or libc DNS.
        # The parent kills the entire disposable worker even inside those calls.
        raise SystemExit(run_owned([sys.executable, __file__, *sys.argv[1:], "--worker"], 300))
    deadline = time.monotonic() + 300
    received = 0
    digest = hashlib.sha256()
    previous = None
    first_hash = None
    with args.output.open("xb") as output:
        with socket.create_connection((args.host, 50002), timeout=10) as raw:
            with ssl.create_default_context().wrap_socket(
                raw, server_hostname=args.host
            ) as stream:
                reader = stream.makefile("rb")
                for index, height in enumerate(range(args.start, args.start + count, 2016)):
                    if time.monotonic() >= deadline:
                        raise TimeoutError("300 second campaign deadline")
                    n = min(2016, args.start + count - height)
                    request = {
                        "id": index,
                        "method": "blockchain.block.headers",
                        "params": [height, n],
                    }
                    stream.sendall(json.dumps(request).encode() + b"\n")
                    line = reader.readline(400001)
                    received += len(line)
                    if len(line) > 400000 or not line.endswith(b"\n") or received > 32_000_000:
                        raise ValueError("reply/aggregate byte budget")
                    reply = json.loads(line)
                    if reply.get("id") != index or reply.get("error"):
                        raise ValueError("unexpected RPC response")
                    result = reply["result"]
                    if result["count"] != n:
                        raise ValueError("short header range")
                    payload = bytes.fromhex(result["hex"])
                    if len(payload) != n * 80:
                        raise ValueError("header byte length")
                    for offset in range(0, len(payload), 80):
                        header = payload[offset : offset + 80]
                        if previous is not None and header[4:36] != previous:
                            raise ValueError("broken header linkage")
                        previous = hashlib.sha256(hashlib.sha256(header).digest()).digest()
                        if first_hash is None:
                            first_hash = previous[::-1].hex()
                    output.write(payload)
                    digest.update(payload)
                    print(f"height={height + n - 1} received={received}", flush=True)
                    time.sleep(1)
    manifest = {
        "captured_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source": f"TLS {args.host}:50002 blockchain.block.headers",
        "start": args.start,
        "count": count,
        "first_hash": first_hash,
        "last_hash": previous[::-1].hex(),
        "sha256": digest.hexdigest(),
        "received_application_bytes": received,
        "limits": "300s, 32MB replies, 400000 bytes/reply, one request/second",
        "trust": "Public header capture, not independent full-consensus validation",
    }
    write_manifest(args.output, manifest)
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
