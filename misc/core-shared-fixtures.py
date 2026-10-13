#!/usr/bin/env python3
"""Capture narrow body-check vectors from isolated, network-disabled Core regtest."""

import argparse
import base64
import contextlib
import hashlib
import http.client
import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import time

from readiness_watchdog import run_owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bitcoind", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.worker:
        raise SystemExit(run_owned([sys.executable, __file__, *sys.argv[1:], "--worker"], 120))
    args.output.mkdir(mode=0o700)
    binary = str(args.bitcoind.resolve())
    processes = []

    def start(root):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        log = (root / "core.log").open("wb")
        process = subprocess.Popen([
            binary, f"-datadir={root}", "-regtest", "-server", "-networkactive=0",
            "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-discover=0",
            "-rpcbind=127.0.0.1", f"-rpcport={port}", "-debuglogfile=0", "-printtoconsole=1",
        ], stdout=log, stderr=subprocess.STDOUT)
        processes.append((process, log))
        cookie = root / "regtest/.cookie"
        for _ in range(100):
            if process.poll() is not None:
                raise RuntimeError("Core startup failed; inspect disposable core.log")
            if cookie.is_file():
                break
            time.sleep(0.05)
        auth = base64.b64encode(cookie.read_bytes().strip()).decode()

        def rpc(method, params):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            try:
                connection.request("POST", "/", json.dumps({"id": 1, "method": method, "params": params}),
                                   {"Authorization": "Basic " + auth})
                response = connection.getresponse()
                raw = response.read(100001)
                if len(raw) > 100000:
                    raise ValueError("Core reply bound")
                result = json.loads(raw)
                if result.get("error"):
                    raise ValueError(str(result["error"]))
                return result["result"]
            finally:
                connection.close()
        # Cookie creation precedes full RPC readiness.
        for _ in range(100):
            try:
                rpc("getblockcount", [])
                return rpc
            except (OSError, ValueError):
                time.sleep(0.05)
        raise RuntimeError("Core RPC startup deadline")

    def stop_all():
        for process, log in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            log.close()

    try:
        with contextlib.ExitStack() as stack:
            first = stack.enter_context(tempfile.TemporaryDirectory(dir=args.output, prefix="core-"))
            second = stack.enter_context(tempfile.TemporaryDirectory(dir=args.output, prefix="core-"))
            stack.callback(stop_all)
            source = start(pathlib.Path(first))
            verifier = start(pathlib.Path(second))
            block_hash = source("generatetodescriptor", [1, "raw(51)"])[0]
            payload = bytes.fromhex(source("getblock", [block_hash, 0]))
            vectors = {"valid": payload}
            merkle = bytearray(payload)
            merkle[-1] ^= 1  # Coinbase locktime: changes txid without changing header root.
            vectors["bad-merkle"] = bytes(merkle)
            witness = bytearray(payload)
            witness[-5] ^= 1  # Single coinbase's reserved witness, before final locktime.
            vectors["bad-witness"] = bytes(witness)
            expected = {}
            for name in ("bad-merkle", "bad-witness", "valid"):
                expected[name] = verifier("submitblock", [vectors[name].hex()])
            if expected != {"bad-merkle": "bad-txnmrklroot", "bad-witness": "bad-witness-merkle-match", "valid": None}:
                raise ValueError(f"unexpected independent Core decisions: {expected}")
            manifest = {
                "core_version": subprocess.check_output([binary, "--version"], text=True).splitlines()[0],
                "binary_sha256": hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
                "network": "regtest, networkactive=0, listen=0, no real transactions",
                "hash": block_hash,
                "scope": "shared body Merkle/witness checks only, not full-consensus equivalence",
                "vectors": {},
            }
            for name, data in vectors.items():
                (args.output / f"{name}.raw").write_bytes(data)
                manifest["vectors"][name] = {
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "bytes": len(data), "core_submitblock": expected[name],
                }
            (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
            print(json.dumps(manifest, indent=2))
    finally:
        stop_all()


if __name__ == "__main__":
    main()
