#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

binary="./target/release/terminal-cli"
transcript="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/extensible-terminal-smoke.typescript"
full_screen_transcript="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/extensible-terminal-less-smoke.typescript"
sentinel="EXTENSIBLE_TERMINAL_SMOKE_OK"
full_screen_sentinel="EXTENSIBLE_TERMINAL_LESS_SMOKE_OK"
fixture="$(mktemp "${TMPDIR:-/tmp}/extensible-terminal-less.XXXXXX")"
trap 'rm -f "$fixture"' EXIT

rm -f "$transcript" "$full_screen_transcript"

# `script` allocates the controlling PTY required by crossterm raw mode. Pin a deterministic
# window size because CI-created pseudo-terminals may otherwise report 0x0. The child shell emits
# a marker through the real local-pty -> terminal-emulation -> renderer path.
script --quiet --return --command "stty cols 80 rows 24; exec $binary -- /bin/sh -c 'printf $sentinel'" "$transcript" >/dev/null

if ! grep -aFq "$sentinel" "$transcript"; then
  echo "terminal smoke marker was not rendered" >&2
  exit 1
fi

# Exercise a real interactive full-screen Unix program through the same path. The PTY harness waits
# until less has rendered its first page, sends G through terminal-cli's host-input path, waits for
# less's visible end-of-file status, sends q, then requires a marker emitted by the endpoint shell
# after less returns. The shell deliberately waits for one final Enter after printing that marker so
# the recovery assertion is not coupled to process-exit timing; the basic smoke above separately
# proves final output from a short-lived endpoint is rendered. This combines real pager output,
# input forwarding, alternate-screen lifecycle, and post-program shell recovery without network or
# user configuration.
if ! command -v less >/dev/null 2>&1; then
  echo "less is required for the Unix full-screen compatibility smoke" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for the Unix interactive PTY smoke harness" >&2
  exit 1
fi
for line in $(seq 1 160); do
  printf 'release-smoke-line-%03d\n' "$line" >>"$fixture"
done

python3 - "$binary" "$fixture" "$full_screen_transcript" "$full_screen_sentinel" <<'PY'
import fcntl
import os
import pty
import re
import select
import shlex
import signal
import struct
import sys
import termios
import time

binary, fixture, transcript_path, recovery_text = sys.argv[1:]
first_page = b"release-smoke-line-001"
end_indicator = b"(END)"
recovery = recovery_text.encode()
csi = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
sync_read, sync_write = os.pipe()
pid, master = pty.fork()

if pid == 0:
    os.close(sync_write)
    os.read(sync_read, 1)
    os.close(sync_read)
    env = os.environ.copy()
    env.update(
        {
            "TERM": "xterm-256color",
            "LESS": "",
            "LESSKEY": "/dev/null",
            "LESSHISTFILE": "-",
            "LESSOPEN": "",
        }
    )
    command = (
        f"less -- {shlex.quote(fixture)}; "
        "status=$?; "
        f"printf '%s' {shlex.quote(recovery_text)}; "
        "IFS= read -r _; "
        "exit \"$status\""
    )
    os.execvpe(binary, [binary, "--", "/bin/sh", "-c", command], env)

os.close(sync_read)
fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
os.write(sync_write, b"1")
os.close(sync_write)

deadline = time.monotonic() + 15.0
buffer = bytearray()
seen_first = False
seen_end = False
sent_end = False
sent_quit = False
seen_recovery = False
sent_recovery_ack = False
status = None

with open(transcript_path, "wb") as transcript:
    while time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], 0.1)
        if readable:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            if chunk:
                transcript.write(chunk)
                buffer.extend(chunk)
                if len(buffer) > 262144:
                    del buffer[:-262144]
                # terminal-cli's damage renderer positions individual changed cells with CSI cursor
                # commands. Strip only those host-renderer control sequences when recognizing the
                # visible ASCII fixture/status text; the raw transcript remains preserved.
                rendered = csi.sub(b"", bytes(buffer))
                if first_page in rendered and not sent_end:
                    seen_first = True
                    os.write(master, b"G")
                    sent_end = True
                if sent_end and end_indicator in rendered and not sent_quit:
                    seen_end = True
                    os.write(master, b"q")
                    sent_quit = True
                if recovery in rendered and not sent_recovery_ack:
                    seen_recovery = True
                    os.write(master, b"\r")
                    sent_recovery_ack = True

        waited_pid, waited_status = os.waitpid(pid, os.WNOHANG)
        if waited_pid == pid:
            status = waited_status
            break

def fail(message):
    print(message, file=sys.stderr)
    print(
        f"smoke_state first={seen_first} end={seen_end} sent_end={sent_end} "
        f"sent_quit={sent_quit} recovery={seen_recovery} recovery_ack={sent_recovery_ack}",
        file=sys.stderr,
    )
    print(f"smoke_tail={bytes(buffer[-4096:])!r}", file=sys.stderr)
    sys.exit(1)

if status is None:
    os.kill(pid, signal.SIGKILL)
    _, status = os.waitpid(pid, 0)
    fail("interactive less smoke timed out")

exit_code = os.waitstatus_to_exitcode(status)
if not seen_first:
    fail("less did not render the first fixture page")
if not seen_end:
    fail("less did not reach visible end-of-file state after G")
if not sent_quit:
    fail("less never reached the point where q could be sent")
if not seen_recovery:
    fail("terminal did not recover after the less full-screen session")
if not sent_recovery_ack:
    fail("terminal recovery marker was not acknowledged through the input path")
if exit_code != 0:
    fail(f"terminal/less smoke exited with status {exit_code}")
PY

echo "terminal interactive and full-screen Unix smokes passed"
