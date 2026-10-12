#!/usr/bin/env python3
"""Build once, then measure isolated release test processes on Linux."""

import json
import shutil
import subprocess
import sys


def main():
    """Fail closed on build, workload, or measurement errors."""
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
