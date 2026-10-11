"""PTY integration checks: python core/tests/terminal_protocol.py

First build: cargo build -p opscope-core --example terminal_probe
Emulates terminal replies; real Kitty/multiplexer visual checks remain separate.
"""
import base64
import ctypes
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import termios
import time
import zlib

LIBC = ctypes.CDLL(None, use_errno=True)
LIBC.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_uint]
LIBC.shm_open.restype = ctypes.c_int
LIBC.shm_unlink.argtypes = [ctypes.c_char_p]
LIBC.shm_unlink.restype = ctypes.c_int

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target/debug/examples/terminal_probe"
COMMAND = re.compile(rb"\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\")


class Terminal:
    def __init__(self, mode="auto", reply="kitty"):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 80, 0, 0))
        env = dict(os.environ, TERM="xterm-kitty", OPSCOPE_GRAPHICS=mode)
        self.proc = subprocess.Popen([str(BINARY)], stdin=slave, stdout=slave,
                                     stderr=slave, env=env, start_new_session=True)
        os.close(slave)
        self.output = b""
        self.reply = reply
        self.answered = 0

    def send(self, value):
        os.write(self.master, value)

    def read(self, seconds=0.15):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if not select.select([self.master], [], [], min(0.03, max(0, until - time.monotonic())))[0]:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            self.output += data
            count = self.output.count(b"\x1b[?2026$p")
            if count > self.answered and self.reply != "silent":
                self.answered = count
                response = b""
                if self.reply == "kitty" or self.reply == "keyboard":
                    response += b"\x1b_Gi=2147483646;OK\x1b\\"
                elif self.reply == "reject":
                    response += b"\x1b_Gi=2147483646;ENOTSUP\x1b\\"
                response += b"\x1b[?2026;2$y"
                # Device attributes often arrive before the keyboard flags.
                # Current flags are 0 until we push; the flags reply is still
                # the support signal when it comes second, inside the window.
                response += b"\x1b[?62;c"
                if self.reply == "keyboard":
                    response += b"\x1b[?0u"
                # Deliberately split strings/CSI across reads.
                for part in [response[:4], response[4:17], response[17:]]:
                    self.send(part)
                    time.sleep(0.01)
        return self.output

    def commands(self):
        return [(dict(field.split(b"=", 1) for field in keys.split(b",") if b"=" in field), payload or b"")
                for keys, payload in COMMAND.findall(self.output)]

    def images(self):
        return [(keys, payload) for keys, payload in self.commands() if keys.get(b"a") == b"T"]

    def quit(self):
        names = shm_names(self.commands())
        self.send(b"q")
        self.read(0.2)
        assert self.proc.wait(timeout=3) == 0
        assert b"KEYS:\r\n" in self.output, "terminal replies became user keys"
        left = [name for name in names if shm_exists(name)]
        assert not left, f"exit left shared memory {left!r}"

    def close(self):
        for name in shm_names(self.commands()):
            LIBC.shm_unlink(name)
        if self.proc.poll() is None:
            os.kill(self.proc.pid, signal.SIGCONT)
            self.proc.kill()
            self.proc.wait(timeout=3)
        os.close(self.master)


def shm_names(commands):
    names = []
    for keys, payload in commands:
        if keys.get(b"t") == b"s" and payload:
            try:
                names.append(base64.b64decode(payload))
            except Exception:
                continue
    return names


def shm_exists(name):
    fd = LIBC.shm_open(name, os.O_RDONLY, 0)
    if fd < 0:
        return False
    os.close(fd)
    return True


def shm_read(name):
    fd = LIBC.shm_open(name, os.O_RDONLY, 0)
    if fd < 0:
        return None
    try:
        return os.read(fd, os.fstat(fd).st_size)
    finally:
        os.close(fd)


def run(mode, reply, check):
    terminal = Terminal(mode, reply)
    try:
        terminal.read(0.3)
        check(terminal)
        terminal.quit()
    finally:
        terminal.close()


def artifact(name, data):
    # Decoded images for visual inspection, written only when asked for, to a
    # directory the caller chose rather than a fixed name under /tmp.
    directory = os.environ.get("OPSCOPE_PROBE_ARTIFACTS")
    if directory:
        Path(directory).mkdir(parents=True, exist_ok=True)
        (Path(directory) / name).write_bytes(data)


def supported(t):
    assert t.images(), "acknowledged Kitty did not receive images"
    assert b"\x1b[?2026h" in t.output
    count = len(t.images())
    t.read(0.2)
    assert len(t.images()) == count, "unchanged chart retransmitted"
    keys, payload = t.images()[0]
    assert keys[b"t"] == b"d" and keys[b"o"] == b"z" and keys[b"C"] == b"1"
    decoded = zlib.decompress(base64.b64decode(payload))
    assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4
    artifact("opscope-probe.rgba", decoded)
    artifact("opscope-probe-size.txt", f"{int(keys[b's'])} {int(keys[b'v'])}".encode())
    t.send(b"\x1b[B")
    t.read()
    assert int(t.images()[-1][0][b"r"]) < int(keys[b"r"]), "scroll did not crop chart"
    fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 42, 0, 0))
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read()
    assert int(t.images()[-1][0][b"c"]) == 36, "resize used stale geometry"
    t.send(b"c")
    t.read()
    assert t.commands()[-1][0].get(b"a") == b"d", "screen handoff left an image"
    t.send(b"c")
    t.read()
    assert t.commands()[-1][0].get(b"a") == b"T"
    os.kill(t.proc.pid, signal.SIGTSTP)
    t.read()
    assert t.commands()[-1][0].get(b"a") == b"d", "suspend left an image"
    os.kill(t.proc.pid, signal.SIGCONT)
    t.read(0.3)
    assert t.answered == 2, "resume did not renegotiate"
    assert t.commands()[-1][0].get(b"a") == b"T"


def fallback(t):
    assert not t.images()
    assert b"\x1b[>1u" not in t.output, "keyboard protocol pushed without an answer"
    assert any(0x2800 < ord(c) <= 0x28ff for c in t.output.decode(errors="replace")), "no text chart"


def left_in_legacy(output, where):
    last_push = output.rfind(b"\x1b[>1u")
    last_pop = output.rfind(b"\x1b[<u")
    assert last_push != -1, f"{where}: keyboard protocol was not pushed"
    assert last_pop > last_push, f"{where}: terminal was not left in legacy mode"


def timeout(t):
    t.read(0.7)
    t.send(b"\x1b_Gi=2147483646;OK\x1b\\\x1b[?2026;2$y")
    t.read()
    fallback(t)
    assert b"\x1b[?2026h" not in t.output
    assert b"Kitty unavailable" in t.output


def rejected_image(t):
    keys, _ = t.images()[0]
    t.send(b"\x1b_Gi=" + keys[b"i"] + b",p=1;ENOMEM:fixture\x1b\\")
    t.read()
    assert b"Kitty image rejected" in t.output
    assert t.commands()[-1][0].get(b"a") == b"d"
    count = len(t.images())
    t.read()
    assert len(t.images()) == count


def filled_graphics(t):
    count = len(t.images())
    t.send(b"g")
    t.read()
    added = t.images()[count:]
    assert len(added) == 5, "bars, calendar, meter and state distribution did not reach the compositor"
    for n, (keys, payload) in enumerate(added):
        assert keys[b"m"] == b"0", "small gallery fixture unexpectedly chunked"
        decoded = zlib.decompress(base64.b64decode(payload))
        assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4
        assert any(decoded[3::4]), "empty graphic"
        if n == 3:
            # The pace mark at 65% has two dark pixels on either side.
            row = decoded[8 * 160 * 4:9 * 160 * 4]
            assert row[102 * 4:104 * 4] == bytes([10, 12, 18, 255]) * 2
            assert row[104 * 4:106 * 4] == bytes([238, 244, 252, 255]) * 2
            assert row[106 * 4:108 * 4] == bytes([10, 12, 18, 255]) * 2
        artifact(f"opscope-gallery-{n}.rgba", decoded)
        artifact(f"opscope-gallery-{n}.size", f"{int(keys[b's'])} {int(keys[b'v'])}".encode())
    count = len(t.images())
    t.read()
    assert len(t.images()) == count, "unchanged figures retransmitted"
    t.send(b"c")
    t.read()
    assert t.commands()[-1][0].get(b"a") == b"d"


def filled_text(t):
    t.send(b"g")
    t.read()
    assert not t.images()
    text = t.output.decode(errors="replace")
    assert all(glyph in text for glyph in "█┃")
    assert "\x1b[48;2;10;12;18m\x1b[38;2;238;244;252m┃\x1b[49m" in text



def underlines(t):
    # Inside the negotiation window the fixture keeps today's colour-only text.
    assert b"\x1bP+q536d756c78;536574756c63\x1b\\" in t.output
    assert b" stale" in t.output and b" partial" in t.output and b" over" in t.output
    assert b"\x1b[4:3m" not in t.output
    assert b"\x1b[4:5m" not in t.output
    assert b"\x1b[58;2;" not in t.output
    t.send(b"\x1bP1+r536d756c78=1b5b343a25703125646d;536574756c63=1b5b3538\x1b\\")
    t.read(0.45)
    assert b"\x1b[4:3m" in t.output
    assert b"\x1b[4:5m" in t.output
    assert b"\x1b[58;2;255;214;140m" in t.output
    assert b" stale" in t.output and b" partial" in t.output and b" over" in t.output


def plain_underlines(t):
    assert b"\x1bP+q536d756c78;536574756c63\x1b\\" in t.output
    t.send(b"\x1bP0+r536d756c78;536574756c63\x1b\\")
    t.read(0.45)
    assert b"\x1b[4m" in t.output
    assert b"\x1b[4:3m" not in t.output
    assert b"\x1b[4:5m" not in t.output
    assert b"\x1b[58;2;" not in t.output
    assert b" stale" in t.output and b" partial" in t.output and b" over" in t.output


def shm_rejected(t):
    probes = [c for c in t.commands() if c[0].get(b"a") == b"t" and c[0].get(b"t") == b"s"]
    assert probes, "no shared-memory probe"
    name = base64.b64decode(probes[0][1])
    t.send(b"\x1b_Gi=2147483645;EBADF:fixture\x1b\\")
    t.read(0.3)
    assert not shm_exists(name), "rejected probe was not unlinked"
    before = len(t.images())
    fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 60, 0, 0))
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.4)
    assert len(t.images()) > before, "resize did not resend the chart"
    assert all(keys.get(b"t") == b"d" for keys, _ in t.images())
    assert b"Kitty image rejected" not in t.output


def shm_accepted(t):
    t.send(b"\x1b_Gi=2147483645;OK\x1b\\")
    t.read(0.25)
    before = len(t.images())
    fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 60, 0, 0))
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.4)
    added = t.images()[before:]
    assert added, "accepted probe did not resend an image"
    keys, payload = added[-1]
    assert keys[b"t"] == b"s" and keys[b"m"] == b"0" and keys[b"o"] == b"z"
    data = shm_read(base64.b64decode(payload))
    assert data, "shared image was not readable"
    decoded = zlib.decompress(data)
    assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4
    assert any(decoded[3::4]), "empty graphic"


def responsive_selection(t):
    # Each press must produce its own frame inside the 300 ms idle interval.
    # Also verifies unchanged Kitty images are not retransmitted on navigation.
    for selected in [1, 2]:
        t.send(b"\x1b[B")
        t.read(0.12)
        assert f"SELECTED:{selected}".encode() in t.output, "input waited for the idle redraw timer"
    # First Down scrolls/crops the fixture; second only changes selection.
    after_scroll = len(t.images())
    t.send(b"\x1b[B")
    t.read(0.12)
    assert b"SELECTED:3" in t.output
    assert len(t.images()) == after_scroll, "selection retransmitted unchanged images"

if __name__ == "__main__":
    for mode, reply, check in [("auto", "kitty", supported), ("auto", "reject", fallback),
                               ("text", "sync", fallback), ("kitty", "silent", timeout),
                               ("auto", "kitty", rejected_image),
                               ("auto", "kitty", shm_rejected), ("auto", "kitty", shm_accepted),
                               ("auto", "kitty", filled_graphics), ("text", "sync", filled_text),
                               ("auto", "kitty", responsive_selection), ("text", "sync", responsive_selection),
                               ("text", "sync", underlines), ("text", "sync", plain_underlines)]:
        run(mode, reply, check)
        print(f"PASS {mode}/{reply}: {check.__name__}")
    for ending in ["signal", "panic"]:
        t = Terminal()
        try:
            t.read(0.3)
            assert t.images()
            if ending == "signal":
                os.kill(t.proc.pid, signal.SIGTERM)
            else:
                t.send(b"p")
            t.read(0.2)
            assert t.proc.wait(timeout=3) != 0
            assert t.commands()[-1][0].get(b"a") == b"d", f"{ending} left an image"
            left = [name for name in shm_names(t.commands()) if shm_exists(name)]
            assert not left, f"{ending} left shared memory {left!r}"
            assert t.output.rfind(b"\x1b[?2026l") > t.output.rfind(b"\x1b[?2026h")
            print(f"PASS cleanup/{ending}")
        finally:
            t.close()

    t = Terminal(reply="keyboard")
    try:
        t.read(0.3)
        assert b"\x1b[?u" in t.output, "keyboard support was not queried"
        assert b"\x1b[>1u" in t.output, "an answer did not push disambiguation"
        t.quit()
        left_in_legacy(t.output, "exit")
        print("PASS keyboard/exit")
    finally:
        t.close()

    t = Terminal(reply="keyboard")
    try:
        t.read(0.3)
        assert b"\x1b[>1u" in t.output
        os.kill(t.proc.pid, signal.SIGTSTP)
        t.read()
        left_in_legacy(t.output, "suspend")
        os.kill(t.proc.pid, signal.SIGCONT)
        t.read(0.6)
        assert t.output.rfind(b"\x1b[>1u") > t.output.rfind(b"\x1b[<u"), "resume did not push again"
        t.quit()
        left_in_legacy(t.output, "exit after resume")
        print("PASS keyboard/suspend")
    finally:
        t.close()

    t = Terminal(reply="keyboard")
    try:
        t.read(0.3)
        assert b"\x1b[>1u" in t.output
        t.send(b"p")
        t.read(0.3)
        assert t.proc.wait(timeout=3) != 0
        left_in_legacy(t.output, "panic")
        print("PASS keyboard/panic")
    finally:
        t.close()

    # os.kill covers the handler. These are the bytes flag 1 sends for the
    # chords the terminal driver used to turn into signals.
    for sequence, sig, label in [
        (b"\x1b[99;5u", signal.SIGINT, "typed-interrupt"),
        (b"\x1b[92;5u", signal.SIGQUIT, "typed-quit"),
    ]:
        t = Terminal(reply="keyboard")
        try:
            t.read(0.3)
            assert b"\x1b[>1u" in t.output
            t.send(sequence)
            t.read(0.4)
            status = t.proc.wait(timeout=3)
            t.read(0.2)
            assert status == 128 + sig, f"{label}: exit {status}"
            left_in_legacy(t.output, label)
            print(f"PASS keyboard/{label}")
        finally:
            t.close()

    t = Terminal(reply="keyboard")
    try:
        t.read(0.3)
        assert b"\x1b[>1u" in t.output
        t.send(b"\x1b[122;5u")
        stopped = False
        until = time.monotonic() + 1
        while time.monotonic() < until:
            state = subprocess.check_output(
                ["ps", "-o", "state=", "-p", str(t.proc.pid)], text=True
            ).strip()
            if state.startswith("T"):
                stopped = True
                break
            time.sleep(0.02)
        assert stopped, "typed Ctrl-Z did not stop the process"
        t.read(0.2)
        left_in_legacy(t.output, "typed suspend")
        os.kill(t.proc.pid, signal.SIGCONT)
        t.read(1.0)
        assert t.output.rfind(b"\x1b[>1u") > t.output.rfind(b"\x1b[<u"), "resume did not push again"
        t.quit()
        left_in_legacy(t.output, "exit after typed suspend")
        print("PASS keyboard/typed-suspend")
    finally:
        t.close()

    # Ctrl-S holds frames inside the probe. The redraw goes through poll and
    # draw_plots; a stopped tty would block that draw and never read Ctrl-Q.
    t = Terminal(reply="keyboard")
    try:
        t.read(0.3)
        assert b"\x1b[>1u" in t.output
        assert b"SELECTED:0" in t.output
        t.send(b"\x1b[115;5u")
        t.read(0.3)
        t.send(b"\x1b[B")
        t.read(0.5)
        assert t.proc.poll() is None, "Ctrl-S wedged the probe"
        assert b"SELECTED:1" not in t.output, "stopped output reached the terminal"
        t.send(b"\x1b[113;5u")
        t.read(0.8)
        assert b"SELECTED:1" in t.output, "Ctrl-Q did not resume output"
        t.send(b"\x1b[99;5u")
        t.read(0.4)
        status = t.proc.wait(timeout=3)
        assert status == 128 + signal.SIGINT, f"flow-control: exit {status}"
        left_in_legacy(t.output, "flow-control")
        print("PASS keyboard/flow-control")
    finally:
        t.close()
