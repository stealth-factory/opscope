"""PTY integration checks: python core/tests/terminal_protocol.py

First build: cargo build -p opscope-core --example terminal_probe
Emulates terminal replies; real Kitty/multiplexer visual checks remain separate.
"""
import base64
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
                if self.reply == "kitty":
                    response += b"\x1b_Gi=2147483646;OK\x1b\\"
                elif self.reply == "reject":
                    response += b"\x1b_Gi=2147483646;ENOTSUP\x1b\\"
                response += b"\x1b[?2026;2$y\x1b[?62;c"
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
        self.send(b"q")
        self.read(0.2)
        assert self.proc.wait(timeout=3) == 0
        assert b"KEYS:\r\n" in self.output, "terminal replies became user keys"

    def close(self):
        if self.proc.poll() is None:
            os.kill(self.proc.pid, signal.SIGCONT)
            self.proc.kill()
            self.proc.wait(timeout=3)
        os.close(self.master)


def run(mode, reply, check):
    terminal = Terminal(mode, reply)
    try:
        terminal.read(0.3)
        check(terminal)
        terminal.quit()
    finally:
        terminal.close()


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
    # Save only a test artifact for optional visual inspection.
    Path("/tmp/opscope-probe.rgba").write_bytes(decoded)
    Path("/tmp/opscope-probe-size.txt").write_text(f"{int(keys[b's'])} {int(keys[b'v'])}")
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
    assert any(0x2800 < ord(c) <= 0x28ff for c in t.output.decode(errors="replace")), "no text chart"


def timeout(t):
    t.read(0.7)
    t.send(b"\x1b_Gi=2147483646;OK\x1b\\\x1b[?2026;2$y")
    t.read()
    fallback(t)
    assert b"\x1b[?2026h" not in t.output
    assert b"Kitty unavailable; using text" in t.output


def rejected_image(t):
    keys, _ = t.images()[0]
    t.send(b"\x1b_Gi=" + keys[b"i"] + b",p=1;ENOMEM:fixture\x1b\\")
    t.read()
    assert b"Kitty image rejected; using text" in t.output
    assert t.commands()[-1][0].get(b"a") == b"d"
    count = len(t.images())
    t.read()
    assert len(t.images()) == count


def filled_graphics(t):
    count = len(t.images())
    t.send(b"g")
    t.read()
    added = t.images()[count:]
    assert len(added) == 4, "bars, calendar and meter did not reach the compositor"
    for n, (keys, payload) in enumerate(added):
        assert keys[b"m"] == b"0", "small gallery fixture unexpectedly chunked"
        decoded = zlib.decompress(base64.b64decode(payload))
        assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4
        assert any(decoded[3::4]), "empty graphic"
        Path(f"/tmp/opscope-gallery-{n}.rgba").write_bytes(decoded)
        Path(f"/tmp/opscope-gallery-{n}.size").write_text(f"{int(keys[b's'])} {int(keys[b'v'])}")
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
                               ("auto", "kitty", filled_graphics), ("text", "sync", filled_text),
                               ("auto", "kitty", responsive_selection), ("text", "sync", responsive_selection)]:
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
            assert t.output.rfind(b"\x1b[?2026l") > t.output.rfind(b"\x1b[?2026h")
            print(f"PASS cleanup/{ending}")
        finally:
            t.close()
