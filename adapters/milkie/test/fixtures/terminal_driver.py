"""Give the integration-test child a controlling terminal, forwarding bytes only.

Node's piped stdio uses sockets on macOS, which /usr/bin/script cannot use as
its input terminal. This fixture creates a real PTY without changing the CLI.
"""
import errno
import os
import pty
import select
import sys

pid, master = pty.fork()
if pid == 0:
    os.execvp(sys.argv[1], sys.argv[1:])

try:
    inputs = [master, sys.stdin.fileno()]
    while True:
        ready, _, _ = select.select(inputs, [], [])
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                break
            if not data:
                break
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
        if sys.stdin.fileno() in ready:
            data = os.read(sys.stdin.fileno(), 4096)
            if data:
                os.write(master, data)
            else:
                inputs.remove(sys.stdin.fileno())
    _, status = os.waitpid(pid, 0)
    sys.exit(os.waitstatus_to_exitcode(status))
finally:
    os.close(master)
