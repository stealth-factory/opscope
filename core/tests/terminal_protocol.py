"""PTY integration checks: python core/tests/terminal_protocol.py

First build: cargo build -p opscope-core --example terminal_probe
Emulates terminal replies; real Kitty/multiplexer visual checks remain separate.
"""
import base64
import ctypes
import fcntl
import mmap
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
    def __init__(self, mode="auto", reply="kitty", image_reply="ok", extra=None):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 80, 0, 0))
        env = dict(os.environ, TERM="xterm-kitty", OPSCOPE_GRAPHICS=mode)
        # The parent shell may be inside tmux. Existing cases are the direct path.
        env.pop("TMUX", None)
        env.pop("HERDR_ENV", None)
        if extra:
            env.update(extra)
        self.proc = subprocess.Popen([str(BINARY)], stdin=slave, stdout=slave,
                                     stderr=slave, env=env, start_new_session=True)
        os.close(slave)
        self.output = b""
        self.reply = reply
        self.answered = 0
        self.image_reply = image_reply
        self.ack_commands = 0
        self.chunk_reply = None

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
            self.acknowledge_images()
            count = self.output.count(b"\x1b[?2026$p")
            if count > self.answered and self.reply != "silent":
                self.answered = count
                response = b""
                if self.reply == "kitty" or self.reply == "keyboard":
                    response += self._graphics_reply(b"OK")
                elif self.reply == "basic":
                    # A plain graphics OK, even when the placeholder probe was sent.
                    response += b"\x1b_Gi=2147483646;OK\x1b\\"
                elif self.reply == "reject":
                    response += self._graphics_reply(b"ENOTSUP")
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

    def acknowledge_images(self):
        # q=0 owes one response after the complete upload. Placements and
        # deletions keep their quiet modes and do not advance the FIFO.
        commands = self.commands()
        for keys, _ in commands[self.ack_commands:]:
            if keys.get(b"a") == b"T":
                self.chunk_reply = (keys[b"i"] + b",p=" + keys[b"p"]) if keys.get(b"q", b"0") == b"0" else None
            if keys.get(b"m") == b"0" and self.chunk_reply is not None:
                if self.image_reply == "ok":
                    self.send(b"\x1b_Gi=" + self.chunk_reply + b";OK\x1b\\")
                self.chunk_reply = None
        self.ack_commands = len(commands)

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

    def _graphics_reply(self, status):
        # The placeholder probe uses its own id. Answering the plain query
        # must not be treated as support for virtual placement.
        ident = b"2147483645" if b"i=2147483645," in self.output else b"2147483646"
        return b"\x1b_Gi=" + ident + b";" + status + b"\x1b\\"


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
    # Darwin shm descriptors reject read(); the bytes are mapped, which is
    # also how the terminal reads the object. fstat there is the page-rounded
    # length, so callers slice to the command's S= payload size.
    fd = LIBC.shm_open(name, os.O_RDONLY, 0)
    if fd < 0:
        return None
    try:
        size = os.fstat(fd).st_size
        if size <= 0:
            return b""
        with mmap.mmap(fd, size, access=mmap.ACCESS_READ) as mapped:
            return bytes(mapped)
    finally:
        os.close(fd)


def run(mode, reply, check, extra=None):
    terminal = Terminal(
        mode,
        reply,
        image_reply="manual" if check == rejected_image else "ok",
        extra=extra,
    )
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


def moving_meter_keeps_unchanged_pixels():
    for width in (57, 120):
        for shared in (False, True):
            t = Terminal()
            try:
                t.read(0.3)
                t.send(b"\x1b_Gi=2147483645;" + (b"OK" if shared else b"EBADF") + b"\x1b\\")
                fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 20, width, 0, 0))
                os.kill(t.proc.pid, signal.SIGWINCH)
                t.read(0.2)
                t.send(b"m")
                t.read(0.3)
                assert t.images()[-1][0][b"r"] == b"1"
                assert t.images()[-1][0][b"t"] == (b"s" if shared else b"d")
                count = len(t.images())
                start = len(t.commands())
                t.read(1.0)
                assert len(t.images()) == count, "subpixel marker changes triggered an upload/reply loop"
                assert not any(k.get(b"a") == b"d" for k, _ in t.commands()[start:]), "unchanged pixels were deleted"
                t.quit()
                print(f"PASS moving-meter/{width}/{'shared' if shared else 'direct'}")
            finally:
                t.close()


def supported(t):
    assert t.images(), "acknowledged Kitty did not receive images"
    assert b"\x1b[?2026h" in t.output
    count = len(t.images())
    t.read(0.2)
    assert len(t.images()) == count, "unchanged chart retransmitted"
    keys, payload = t.images()[0]
    assert keys[b"t"] == b"d" and keys[b"o"] == b"z" and keys[b"C"] == b"1"
    assert keys[b"f"] == b"32", "line charts keep alpha"
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
    t.send(b"\x1b_Gi=" + keys[b"i"] + b",p=" + keys[b"p"] + b";ENOMEM:fixture\x1b\\")
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
    assert len(added) == 6, "bars, calendar, meter, state distribution and solid tile did not reach the compositor"
    formats = []
    for n, (keys, payload) in enumerate(added):
        assert keys[b"m"] == b"0", "small gallery fixture unexpectedly chunked"
        assert keys[b"f"] in (b"24", b"32")
        bpp = 3 if keys[b"f"] == b"24" else 4
        decoded = zlib.decompress(base64.b64decode(payload))
        width = int(keys[b"s"])
        height = int(keys[b"v"])
        assert len(decoded) == width * height * bpp
        if bpp == 4:
            assert any(decoded[3::4]), "empty graphic"
        else:
            assert any(decoded), "empty graphic"
        formats.append(keys[b"f"])
        if n == 3:
            # Pace mark at 65% of the meter: two dark pixels, the mark, two
            # dark pixels. The buffer is the visible crop, so the row is the
            # middle of what was sent and the stride is that crop's width.
            y = height // 2
            row = decoded[y * width * bpp:(y + 1) * width * bpp]
            dark = bytes([10, 12, 18, 255][:bpp])
            light = bytes([238, 244, 252, 255][:bpp])
            assert row[102 * bpp:104 * bpp] == dark * 2
            assert row[104 * bpp:106 * bpp] == light * 2
            assert row[106 * bpp:108 * bpp] == dark * 2
        # Keep the transmitted bytes. RGB is not padded out to RGBA; the
        # name and the size line say which layout a viewer should use.
        ext = "rgb" if bpp == 3 else "rgba"
        artifact(f"opscope-gallery-{n}.{ext}", decoded)
        artifact(
            f"opscope-gallery-{n}.size",
            f"{width} {height} {ext}".encode(),
        )
    assert formats[:4] == [b"32", b"32", b"32", b"32"], "gaps, partial bars and the meter inset keep alpha"
    assert formats[-2:] == [b"24", b"24"], "the stacked bar and fully opaque tile are sent as RGB"
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
    probes = [c for c in t.commands() if c[0].get(b"a") == b"q" and c[0].get(b"t") == b"s"]
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
    assert keys[b"t"] == b"s" and keys[b"m"] == b"0" and b"o" not in keys
    data = shm_read(base64.b64decode(payload))
    assert data, "shared image was not readable"
    if b"S" in keys:
        data = data[: int(keys[b"S"])]
    decoded = data
    assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4
    assert any(decoded[3::4]), "empty graphic"


def shared_rgb_gallery(t):
    t.send(b"\x1b_Gi=2147483645;OK\x1b\\")
    t.read(0.25)
    before = len(t.images())
    t.send(b"g")
    t.read(0.25)
    tiles = [(keys, payload) for keys, payload in t.images()[before:]
             if keys.get(b"f") == b"24" and keys.get(b"t") == b"s"]
    assert tiles, "opaque RGB tile did not use shared memory"
    keys, payload = tiles[-1]
    data = shm_read(base64.b64decode(payload))[:int(keys[b"S"])]
    assert len(data) == int(keys[b"s"]) * int(keys[b"v"]) * 3
    assert data == bytes([50, 220, 170]) * (len(data) // 3), "RGB shared pixels changed"
    assert b"o" not in keys and keys[b"q"] == b"0"


# Header ESC, then the payload's leading ESC doubled. tmux's FAQ uses the
# same shape. unwrap_tmux treats the header as ending at that first ESC.
PROBE = (
    b"\x1bPtmux;\x1b"
    b"\x1b\x1b_Gi=2147483645,s=1,v=1,a=q,U=1,t=d,f=24;AAAA"
    b"\x1b\x1b\\\x1b\\"
)
# Eight hex digits: U+10EEEE, not U+010EEE plus a trailing E.
PLACEHOLDER = "\U0010EEEE".encode()


def unwrap_tmux(data):
    """Undo DCS passthrough so the graphics commands can be parsed."""
    out = bytearray()
    i = 0
    header = b"\x1bPtmux;\x1b"
    while i < len(data):
        if data.startswith(header, i):
            i += len(header)
            while i < len(data):
                if data.startswith(b"\x1b\x1b", i):
                    out.append(0x1B)
                    i += 2
                    continue
                if data.startswith(b"\x1b\\", i):
                    i += 2
                    break
                out.append(data[i])
                i += 1
            continue
        out.append(data[i])
        i += 1
    return bytes(out)


def commands_of(data):
    return [
        (dict(field.split(b"=", 1) for field in keys.split(b",") if b"=" in field), payload or b"")
        for keys, payload in COMMAND.findall(data)
    ]


def tmux_placeholders(t):
    assert PROBE in t.output, "placeholder probe was not passed through"
    assert b"i=2147483646," not in t.output, "tmux sent the plain graphics query"
    images = [(keys, payload) for keys, payload in commands_of(unwrap_tmux(t.output)) if keys.get(b"a") == b"T"]
    assert images, "answered passthrough did not upload a chart"
    keys, _ = images[0]
    assert keys.get(b"U") == b"1" and keys.get(b"q") == b"1", keys
    upload = t.output.find(b"a=T")
    cell = t.output.find(PLACEHOLDER)
    assert 0 <= upload < cell, "virtual placement was not uploaded before the cells"
    assert b"\x1b[0mnow" in t.output, "label inherited the placeholder colour"
    assert not any(k.get(b"a") == b"p" for k, _ in commands_of(unwrap_tmux(t.output)))
    t.send(b"\x1b[B")
    t.read()
    later = [k for k, _ in commands_of(unwrap_tmux(t.output)) if k.get(b"a") == b"T"]
    assert int(later[-1][b"r"]) < int(keys[b"r"]), "scroll did not crop the placeholder chart"


def tmux_text(t):
    assert PROBE in t.output, "placeholder probe was not passed through"
    t.read(0.7)
    fallback(t)
    assert PLACEHOLDER not in t.output, "an unanswered query still drew placeholders"


def tmux_basic_is_braille(t):
    assert PROBE in t.output, "placeholder probe was not passed through"
    t.read(0.2)
    fallback(t)
    assert PLACEHOLDER not in t.output, "a plain graphics OK enabled placeholders"


def herdr_direct(t):
    assert b"\x1bPtmux;" not in t.output, "Herdr was wrapped in a tmux passthrough"
    assert b"U=1" not in t.output
    supported(t)


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

def successful_upload_before_shared_failure():
    t = Terminal(reply="kitty", image_reply="manual")
    try:
        t.read(0.3)
        direct = t.images()[-1][0]
        assert direct[b"t"] == b"d" and direct[b"q"] == b"0", "tracked sends need replies"
        t.send(b"\x1b_Gi=" + direct[b"i"] + b",p=" + direct[b"p"] + b";OK\x1b\\")
        t.send(b"\x1b_Gi=2147483645;OK\x1b\\")
        t.read(0.2)
        # Scrolling preserves the reply queue; resizing would clear it and
        # hide a stale successful send ahead of this shared-memory failure.
        before = len(t.images())
        t.send(b"\x1b[B")
        t.read(0.2)
        shared = t.images()[-1][0]
        assert len(t.images()) > before and shared[b"t"] == b"s"
        assert shared[b"i"] == direct[b"i"] and shared[b"q"] == b"0"
        assert shared[b"p"] != direct[b"p"], "upload placement key was reused"
        t.send(b"\x1b_Gi=" + shared[b"i"] + b",p=" + shared[b"p"] + b";EBADF:fixture\x1b\\")
        t.read(0.3)
        assert b"Kitty image rejected" not in t.output, "shared failure disabled graphics"
        fallback = t.images()[-1][0]
        assert fallback[b"t"] == b"d" and fallback[b"q"] == b"0"
        t.send(b"\x1b_Gi=" + fallback[b"i"] + b",p=" + fallback[b"p"] + b";EINVAL:fixture\x1b\\")
        t.read(0.3)
        assert b"Kitty image rejected" in t.output, "direct failure was mistaken for shared"
        t.quit()
    finally:
        t.close()

def missing_and_late_replies_keep_their_upload_keys():
    t = Terminal(reply="kitty", image_reply="manual")
    try:
        t.read(0.3)
        old = t.images()[-1][0]
        # Leave this direct success unanswered. It must not consume a later
        # shared upload's error, even when replies arrive out of order.
        t.send(b"\x1b_Gi=2147483645;OK\x1b\\")
        t.read(0.2)
        t.send(b"\x1b[B")
        t.read(0.2)
        shared = t.images()[-1][0]
        assert shared[b"t"] == b"s" and shared[b"p"] != old[b"p"]
        t.send(b"\x1b_Gi=" + shared[b"i"] + b",p=" + shared[b"p"] + b";ENOMEM:fixture\x1b\\")
        t.read(0.3)
        assert b"Kitty image rejected" not in t.output
        direct = t.images()[-1][0]
        assert direct[b"t"] == b"d"
        t.send(b"\x1b_Gi=" + old[b"i"] + b",p=" + old[b"p"] + b";OK\x1b\\")
        t.send(b"\x1b_Gi=" + old[b"i"] + b",p=4294967295;EINVAL:unknown\x1b\\")
        t.read(0.2)
        assert b"Kitty image rejected" not in t.output, "late reply rejected a newer upload"
        t.send(b"\x1b_Gi=" + direct[b"i"] + b",p=" + direct[b"p"] + b";EINVAL:current\x1b\\")
        t.read(0.2)
        assert b"Kitty image rejected" in t.output, "late reply consumed the current key"
        t.quit()
    finally:
        t.close()


if __name__ == "__main__":
    moving_meter_keeps_unchanged_pixels()
    successful_upload_before_shared_failure()
    print("PASS shared-memory/success-before-failure")
    missing_and_late_replies_keep_their_upload_keys()
    print("PASS shared-memory/missing-and-late-replies")
    for mode, reply, check in [("auto", "kitty", supported), ("auto", "reject", fallback),
                               ("text", "sync", fallback), ("kitty", "silent", timeout),
                               ("auto", "kitty", rejected_image),
                               ("auto", "kitty", shm_rejected), ("auto", "kitty", shm_accepted),
                               ("auto", "kitty", shared_rgb_gallery),
                               ("auto", "kitty", filled_graphics), ("text", "sync", filled_text),
                               ("auto", "kitty", responsive_selection), ("text", "sync", responsive_selection),
                               ("text", "sync", underlines), ("text", "sync", plain_underlines)]:
        run(mode, reply, check)
        print(f"PASS {mode}/{reply}: {check.__name__}")
    run("auto", "kitty", tmux_placeholders, {"TMUX": "/tmp/tmux-0/default,1,0"})
    print("PASS placeholder tmux")
    run("auto", "basic", tmux_basic_is_braille, {"TMUX": "1"})
    print("PASS placeholder plain-ok stays text")
    run("auto", "kitty", herdr_direct, {"HERDR_ENV": "1"})
    print("PASS herdr direct")
    run("auto", "silent", tmux_text, {"TMUX": "1"})
    print("PASS placeholder unanswered")
    for ending in ["signal", "panic"]:
        t = Terminal()
        try:
            t.read(0.3)
            assert t.images()
            t.send(b"\x1b_Gi=2147483645;OK\x1b\\")
            t.read(0.25)
            before = len(t.images())
            fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 60, 0, 0))
            os.kill(t.proc.pid, signal.SIGWINCH)
            t.read(0.4)
            added = t.images()[before:]
            assert added, f"{ending} had no chart after the probe was accepted"
            keys, payload = added[-1]
            assert keys[b"t"] == b"s", f"{ending} cleaned up before a shared chart existed"
            name = base64.b64decode(payload)
            assert shm_exists(name), f"{ending} chart was not in shared memory"
            if ending == "signal":
                os.kill(t.proc.pid, signal.SIGTERM)
            else:
                t.send(b"p")
            t.read(0.2)
            assert t.proc.wait(timeout=3) != 0
            assert t.commands()[-1][0].get(b"a") == b"d", f"{ending} left an image"
            # Panic drops Keyboard, which unlinks. SIGTERM only disarms: shm_unlink
            # is not async-signal-safe, so the handler must not collect the object.
            if ending == "panic":
                assert not shm_exists(name), "panic left shared memory"
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
