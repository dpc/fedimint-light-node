#!/usr/bin/env python3
"""Build once, then measure isolated release test processes on Linux."""

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys


def main():
    """Fail closed on build, workload, or measurement errors."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--headers", type=pathlib.Path,
                        help="also replay a default-scale capture and its appended .json manifest")
    args = parser.parse_args()
    if args.headers:
        if args.headers.stat().st_size != 159696 * 80:
            raise SystemExit("Expected exactly 159696 captured headers")
        manifest = json.loads(pathlib.Path(str(args.headers) + ".json").read_text())
        if hashlib.sha256(args.headers.read_bytes()).hexdigest() != manifest["sha256"]:
            raise SystemExit("Capture manifest digest mismatch")
        os.environ["READINESS_HEADERS"] = str(args.headers.resolve())
        os.environ["READINESS_START"] = str(manifest["start"])
    timer = shutil.which("time")
    if sys.platform != "linux" or timer is None:
        raise SystemExit("Requires Linux /proc, Python 3, Cargo and GNU time in PATH")
    names = ["resource_exercise", "header_sync", "block_download", "block_decode",
             "block_decode_allocations", "validated_header_store", "cli"]
    command = ["cargo", "test", "--release", "--locked", "--no-run",
               "--message-format=json"]
    for name in names:
        command.extend(["--test", name])
    build = subprocess.run(command, check=True, stdout=subprocess.PIPE, text=True)
    executables = {}
    for line in build.stdout.splitlines():
        event = json.loads(line)
        if event.get("reason") == "compiler-artifact" and event.get("executable"):
            executables[event["target"]["name"]] = event["executable"]
    for name in names:
        binary = executables[name]
        workloads = (
            [[test, "--exact", "--ignored"] for test in
             ["raw_store_rollover", "validated_store_rollover",
              "large_body_decode", "many_output_body_decode"]]
            if name == "resource_exercise" else [[]]
        )
        if name == "resource_exercise" and args.headers:
            workloads.append(["default_validated_store_rollover", "--exact", "--ignored"])
        for arguments in workloads:
            listing = subprocess.run(
                [binary, *arguments, "--list", "--format", "terse"],
                check=True, stdout=subprocess.PIPE, text=True,
            )
            tests = [line for line in listing.stdout.splitlines()
                     if line.endswith(": test")]
            if not tests or (arguments and tests != [f"{arguments[0]}: test"]):
                raise SystemExit(f"Missing or unexpected tests: {name} {arguments}")
            print(f"\n=== {name} {' '.join(arguments)} ===", flush=True)
            subprocess.run(
                [timer, "-f", "wall_s=%e user_s=%U system_s=%S peak_rss_kib=%M",
                 binary, *arguments, "--nocapture", "--test-threads=1"],
                check=True,
            )


if __name__ == "__main__":
    main()
