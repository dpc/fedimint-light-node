"""Linux deadlines and parent-death cleanup for disposable evidence processes.

Call from a long-lived main thread; Linux tracks the spawning thread's death.
Every process-tree edge must use owned_command. Targets must be trusted ordinary
executables: setuid/setgid, file capabilities, and credential-changing programs
can clear Linux parent-death protection. Launch the top supervisor with shell
``exec`` so killing the managed shell cannot leave that supervisor behind.
"""

import os
import signal
import subprocess
import sys


def owned_command(command):
    """Arm Linux parent-death killing before exec, without a threaded preexec hook."""
    return [sys.executable, os.path.abspath(__file__), str(os.getpid()), *command]


def exec_owned():
    """Kill this direct child if its recorded parent dies, including during setup."""
    import ctypes

    parent = int(sys.argv[1])
    libc = ctypes.CDLL(None, use_errno=True)
    # PR_SET_PDEATHSIG survives exec of our ordinary, non-setuid executables.
    if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_PDEATHSIG")
    if os.getppid() != parent:
        raise SystemExit("owned parent exited before parent-death protection was armed")
    os.execvp(sys.argv[2], sys.argv[2:])


def run_owned(command, seconds):
    """Kill and reap the owned process group on deadline or interruption."""
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt("evidence supervisor interrupted")

    previous = signal.signal(signal.SIGTERM, interrupted)
    process = None
    try:
        process = subprocess.Popen(owned_command(command), start_new_session=True)
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


if __name__ == "__main__":
    exec_owned()
