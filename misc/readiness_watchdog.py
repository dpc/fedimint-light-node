"""Linux process-group deadline for disposable evidence jobs and their children."""

import os
import signal
import subprocess


def run_owned(command, seconds):
    """Kill and reap the owned process group on deadline or interruption."""
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt("evidence supervisor interrupted")

    previous = signal.signal(signal.SIGTERM, interrupted)
    process = None
    try:
        process = subprocess.Popen(command, start_new_session=True)
        return process.wait(timeout=seconds)
    finally:
        # The group belongs only to this invocation, never the invoking shell.
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        signal.signal(signal.SIGTERM, previous)
