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
import shutil
import signal
import struct
import subprocess
import tempfile
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
    def __init__(self, mode="auto", reply="kitty", image_reply="ok", extra=None, extra_env=None):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 80, 0, 0))
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_") and k not in ("TMUX", "TMUX_PANE")}
        env.update(TERM="xterm-kitty", OPSCOPE_GRAPHICS=mode)
        env.update(extra or {})
        env.update(extra_env or {})
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
        # 2147483645 is the shared-memory probe. The placeholder path does
        # not send it, and a direct session may send only that probe.
        for keys, _ in self.commands():
            if keys.get(b"i") == b"2147483645":
                assert keys.get(b"a") == b"q" and keys.get(b"t") == b"s", keys
        return b"\x1b_Gi=2147483646;" + status + b"\x1b\\"

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


def wait_for(t, ready, seconds):
    # Short reads, not one fixed pause: a busy runner can print the marker
    # late, and the caller decides when the condition is actually true.
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if ready():
            return True
        slice_s = min(0.05, deadline - time.monotonic())
        if slice_s <= 0:
            break
        t.read(slice_s)
    return bool(ready())


def run(mode, reply, check, extra=None):
    terminal = Terminal(
        mode,
        reply,
        image_reply="manual" if check == rejected_image else "ok",
        extra=extra,
    )
    try:
        terminal.read(0.3)
        if mode == "text":
            assert b"\x1b[16t" not in terminal.output, "text mode queried pixel dimensions"
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


def scroll_clears_previous_image_region():
    # Multiplexers may keep the old placement while replacing an upload.
    # A moved/cropped image must be explicitly cleared before text redraw.
    for cell_w, cell_h in ((8, 16), (20, 40)):
        for shared in (False, True):
            t = Terminal()
            try:
                t.read(0.3)
                t.send(f"\x1b[6;{cell_h};{cell_w}t".encode())
                t.send(b"\x1b_Gi=2147483645;" + (b"OK" if shared else b"EBADF") + b"\x1b\\")
                t.read(0.2)
                for key in (b"\x1b[B", b"\x1b[A") * 3:
                    old = t.images()[-1][0]
                    start = len(t.commands())
                    t.send(key)
                    t.read(0.2)
                    added = t.commands()[start:]
                    upload = next(i for i, (k, _) in enumerate(added) if k.get(b"a") == b"T")
                    new = added[upload][0]
                    assert old[b"r"] != new[b"r"], "fixture did not change the visible crop"
                    assert new[b"p"] != old[b"p"], "reply generation was reused"
                    assert new[b"t"] == (b"s" if shared else b"d")
                    assert any(k.get(b"a") == b"d" and k.get(b"d") == b"I"
                               and k.get(b"i") == old[b"i"] for k, _ in added[:upload]), \
                        "scroll replaced a cropped chart without clearing its previous placement"
                    count = len(t.images())
                    t.read(0.15)
                    assert len(t.images()) == count, "unchanged chart entered a redraw loop"
                t.quit()
                print(f"PASS scroll-cleanup/{cell_w}x{cell_h}/{'shared' if shared else 'direct'}")
            finally:
                t.close()


def supported(t):
    assert t.images(), "acknowledged Kitty did not receive images"
    assert b"\x1b[?2026h" in t.output
    assert b"now" in t.output, "line-chart label was blanked"
    assert any(
        keys.get(b"a") == b"p" and keys.get(b"z") == b"-1" for keys, _ in t.commands()
    ), "line chart was not placed under the text"
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
    placed_under_text(t.commands())
    os.kill(t.proc.pid, signal.SIGTSTP)
    t.read()
    assert t.commands()[-1][0].get(b"a") == b"d", "suspend left an image"
    os.kill(t.proc.pid, signal.SIGCONT)
    t.read(0.3)
    assert t.answered == 2, "resume did not renegotiate"
    placed_under_text(t.commands())


def placed_under_text(commands):
    # image() still transmits at the default z. compose() then re-places
    # that same id under the text, so the last command is the placement.
    assert commands[-2][0].get(b"a") == b"T", "chart was not transmitted"
    assert commands[-1][0].get(b"a") == b"p" and commands[-1][0].get(b"z") == b"-1"


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
        assert b"z" not in keys, "a solid fill must not sit under text"
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


def image_bytes(commands, index):
    """One transmitted image, including every continuation chunk after index."""
    keys = commands[index][0]
    if keys.get(b"t") == b"s":
        data = shm_read(base64.b64decode(commands[index][1]))
        assert data is not None, "shared image object cannot be mapped"
        return keys, data[:int(keys[b"S"])]
    blob = b""
    for chunk_keys, payload in commands[index:]:
        blob += payload
        if chunk_keys.get(b"m", b"0") == b"0":
            break
    return keys, zlib.decompress(base64.b64decode(blob))


def hidpi_shared_crops_keep_cell_layout(t):
    initial = t.images()[0][0]
    t.send(b"\x1b_Gi=2147483645;OK\x1b\\\x1b[6;40;20t")
    t.read(0.4)
    commands = t.commands()
    index = max(i for i, (k, _) in enumerate(commands) if k.get(b"a") == b"T")
    full, pixels = image_bytes(commands, index)
    assert full[b"t"] == b"s" and full[b"f"] == b"32"
    assert full[b"c"] == initial[b"c"] and full[b"r"] == initial[b"r"]
    assert int(full[b"s"]) == int(full[b"c"]) * 20
    assert int(full[b"v"]) == int(full[b"r"]) * 40
    assert len(pixels) == int(full[b"s"]) * int(full[b"v"]) * 4
    t.send(b"\x1b[B")
    t.read(0.2)
    commands = t.commands()
    index = max(i for i, (k, _) in enumerate(commands) if k.get(b"a") == b"T")
    cropped, visible = image_bytes(commands, index)
    assert cropped[b"t"] == b"s" and cropped[b"c"] == full[b"c"]
    assert int(cropped[b"v"]) == int(cropped[b"r"]) * 40
    assert int(cropped[b"r"]) < int(full[b"r"])
    offset = (int(full[b"v"]) - int(cropped[b"v"])) * int(full[b"s"]) * 4
    assert visible == pixels[offset:], "HiDPI scroll changed the cropped pixels"
    assert cropped[b"p"] != full[b"p"]
    cell_queries_stay_outside_images(t.output)


def hidpi_shared_rgb_gallery(t):
    t.send(b"\x1b_Gi=2147483645;OK\x1b\\\x1b[6;40;20t")
    t.read(0.3)
    before = len(t.commands())
    t.send(b"g")
    t.read(0.3)
    commands = t.commands()
    rgb = []
    for i, (keys, _) in enumerate(commands):
        if i >= before and keys.get(b"a") == b"T" and keys.get(b"f") == b"24":
            keys, data = image_bytes(commands, i)
            assert keys[b"t"] == b"s"
            assert int(keys[b"s"]) == int(keys[b"c"]) * 20
            assert int(keys[b"v"]) == int(keys[b"r"]) * 40
            assert len(data) == int(keys[b"s"]) * int(keys[b"v"]) * 3
            rgb.append(keys)
    assert len(rgb) == 2, "HiDPI opaque stacked bar/tile did not use RGB shared uploads"


def cell_raster_follows_report(t):
    assert b"\x1b[16t" in t.output
    assert t.images(), "no image before the cell-size reply"
    keys, _ = t.images()[0]
    cols, rows = int(keys[b"c"]), int(keys[b"r"])
    assert cols > 0 and rows > 0
    assert int(keys[b"s"]) == cols * 8
    assert int(keys[b"v"]) == rows * 16
    t.send(b"\x1b[6;40;20t")
    t.read(0.8)
    commands = t.commands()
    last = max(i for i, (k, _) in enumerate(commands) if k.get(b"a") == b"T")
    keys, decoded = image_bytes(commands, last)
    assert int(keys[b"s"]) == cols * 20, keys
    assert int(keys[b"v"]) == rows * 40, keys
    assert len(decoded) == int(keys[b"s"]) * int(keys[b"v"]) * 4


def cell_raster_rejects_absurd_and_zero(t):
    keys, _ = t.images()[0]
    cols, rows = int(keys[b"c"]), int(keys[b"r"])
    t.send(b"\x1b[6;0;0t\x1b[6;2;500t\x1b[6;8;8t")
    t.read(0.5)
    keys, _ = t.images()[-1]
    assert int(keys[b"s"]) == cols * 8
    assert int(keys[b"v"]) == rows * 16


def cell_queries_stay_outside_images(output):
    """`CSI 16 t` is sent between frames, never inside a Kitty APC chunk."""
    rest = output
    while True:
        start = rest.find(b"\x1b_G")
        if start < 0:
            break
        end = rest.find(b"\x1b\\", start)
        if end < 0:
            assert b"\x1b[16t" not in rest[start:]
            break
        assert b"\x1b[16t" not in rest[start:end]
        rest = rest[end + 2:]


def cell_raster_follows_winsize_pixels(t):
    keys, _ = t.images()[0]
    cols, rows = int(keys[b"c"]), int(keys[b"r"])
    assert int(keys[b"s"]) == cols * 8
    # 20 rows, 80 cols, 1600×800 px → a 20×40 cell. Column count stays put.
    before = t.output.count(b"\x1b[16t")
    fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 80, 1600, 800))
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.8)
    keys, _ = t.images()[-1]
    assert int(keys[b"c"]) == cols, "pixel size changed the cell layout"
    assert int(keys[b"r"]) == rows
    assert int(keys[b"s"]) == cols * 20, keys
    assert int(keys[b"v"]) == rows * 40, keys
    assert t.output.count(b"\x1b[16t") > before, "resize did not ask for a cell size"
    cell_queries_stay_outside_images(t.output)


def cell_raster_drops_a_barren_resize(t):
    keys, _ = t.images()[0]
    cols, rows = int(keys[b"c"]), int(keys[b"r"])
    t.send(b"\x1b[6;40;20t")
    t.read(0.8)
    keys, _ = t.images()[-1]
    assert int(keys[b"s"]) == cols * 20
    assert int(keys[b"v"]) == rows * 40
    # Same cell grid, no pixel fields, and no new reply. The previous 20×40
    # must not stick; the raster returns to 8×16.
    before = t.output.count(b"\x1b[16t")
    fcntl.ioctl(t.master, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 80, 0, 0))
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.8)
    keys, _ = t.images()[-1]
    assert int(keys[b"c"]) == cols
    assert int(keys[b"r"]) == rows
    assert int(keys[b"s"]) == cols * 8, keys
    assert int(keys[b"v"]) == rows * 16, keys
    assert t.output.count(b"\x1b[16t") > before
    cell_queries_stay_outside_images(t.output)


def lost_startup_cell_reply_does_not_block_resize(t):
    # No reply to startup's CSI 16 t. Once its timeout passes, the next
    # resize query must stand alone and accept the new cell dimensions.
    t.read(1.8)
    before = t.output.count(b"\x1b[16t")
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.4)
    assert t.output.count(b"\x1b[16t") > before
    for _ in range(3):
        t.send(b"\x1b[6;40;20t")
        t.read(0.2)
    keys = t.images()[-1][0]
    assert int(keys[b"s"]) == int(keys[b"c"]) * 20, "lost startup reply blocked the resize report"
    assert int(keys[b"v"]) == int(keys[b"r"]) * 40
    cell_queries_stay_outside_images(t.output)


def ambiguous_cell_reply_retries(t):
    # Startup reply is lost; the first resize reply is therefore ambiguous.
    before = t.output.count(b"\x1b[16t")
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.2)
    t.send(b"\x1b[6;40;20t")
    t.read(0.2)
    for _ in range(3):
        t.send(b"\x1b[6;40;20t")
        t.read(0.2)
    assert t.output.count(b"\x1b[16t") > before + 1, "ambiguous reply did not retry"
    keys = t.images()[-1][0]
    assert int(keys[b"s"]) == int(keys[b"c"]) * 20, "ambiguous reply left fallback raster"


def expired_cell_reply_is_quarantined(t):
    t.read(1.6)
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.2)
    # Expired startup response precedes the actual current response.
    t.send(b"\x1b[6;40;20t\x1b[6;24;12t")
    t.read(0.2)
    for _ in range(3):
        t.send(b"\x1b[6;24;12t")
        t.read(0.2)
    keys = t.images()[-1][0]
    assert int(keys[b"s"]) == int(keys[b"c"]) * 12, "expired reply was relabeled current"


def resumed_cell_reply_is_quarantined(t):
    # Leave a resize query outstanding before suspending with a buffered reply.
    t.send(b"\x1b[6;16;8t")
    t.read(0.1)
    os.kill(t.proc.pid, signal.SIGWINCH)
    t.read(0.1)
    os.kill(t.proc.pid, signal.SIGTSTP)
    t.read(0.1)
    t.send(b"\x1b[6;20;10t")
    os.kill(t.proc.pid, signal.SIGWINCH)
    os.kill(t.proc.pid, signal.SIGCONT)
    t.read(0.2)
    for _ in range(3):
        t.send(b"\x1b[6;40;20t")
        t.read(0.2)
    keys = t.images()[-1][0]
    assert int(keys[b"s"]) == int(keys[b"c"]) * 20, "resume accepted a buffered old report"


def stop_child(pid):
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        # The child can already have exited after an earlier SIGTERM.
        pass


def reclaimed_cell_reply_is_quarantined(t):
    # Leave the startup size query unanswered, then buffer an old report while
    # a child owns the tty. Reclaim must not relabel it as the new query's reply.
    # The child prints its pid and waits for SIGTERM, so a slow runner cannot
    # exit it before the stale report is queued.
    before = len(t.images())
    t.send(b"h")
    assert wait_for(t, lambda: re.search(rb"CHILD_READY (\d+)", t.output), 2), \
        "handoff did not reach the child"
    child = int(re.search(rb"CHILD_READY (\d+)", t.output).group(1))
    reclaimed = False
    try:
        queries = t.output.count(b"\x1b[16t")
        t.send(b"\x1b[6;20;10t")
        t.read(0.15)
        assert t.output.count(b"\x1b[16t") == queries, "cell query reached the child"
        stop_child(child)
        assert wait_for(t, lambda: t.answered >= 2 and len(t.images()) > before, 2), \
            "reclaim did not renegotiate"
        reclaimed = True
        assert t.answered == 2, "reclaim did not renegotiate"
        assert len(t.images()) > before, "reclaim did not redraw"
        assert all(int(keys[b"s"]) != int(keys[b"c"]) * 10
                   for keys, _ in t.images()[before:]), "reclaim accepted a buffered old report"
        # Answer only emitted queries, including confirmation. No SIGWINCH is sent.
        answered = queries + 1  # The buffered report consumed the reclaim query.
        for _ in range(4):
            pending = t.output.count(b"\x1b[16t") - answered
            if pending:
                t.send(b"\x1b[6;40;20t" * pending)
                answered += pending
            t.read(0.15)
        keys = t.images()[-1][0]
        assert int(keys[b"s"]) == int(keys[b"c"]) * 20, "reclaim left stale raster without resize"
        assert int(keys[b"v"]) == int(keys[b"r"]) * 40, "reclaim left stale raster height"
    finally:
        if not reclaimed:
            stop_child(child)


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
    data = shm_read(base64.b64decode(payload))
    assert data is not None, "shared image object cannot be mapped"
    data = data[:int(keys[b"S"])]
    assert len(data) == int(keys[b"s"]) * int(keys[b"v"]) * 3
    assert data == bytes([50, 220, 170]) * (len(data) // 3), "RGB shared pixels changed"
    assert b"o" not in keys and keys[b"q"] == b"0"


# ESC P tmux ; then the payload, with each ESC doubled. A payload that
# starts with ESC therefore has two ESC bytes before its command, which
# is the example in tmux's FAQ. unwrap_tmux treats the header as ending
# at `tmux;`, with no extra ESC of its own.
# Eight hex digits: U+10EEEE, not U+010EEE plus a trailing E.
PLACEHOLDER = "\U0010EEEE".encode()


def unwrap_tmux(data):
    """Undo DCS passthrough so the graphics commands can be parsed."""
    out = bytearray()
    i = 0
    header = b"\x1bPtmux;"
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
    assert b"[>q" not in t.output, "XTVERSION was sent through the passthrough"
    assert b"i=2147483646," not in t.output, "tmux sent the plain graphics query"
    assert b"2147483645" not in t.output, "tmux used the shared-memory probe id"
    assert b"\x1bPtmux;\x1b\x1b\x1b" not in t.output, "passthrough header carried an extra ESC"
    images = [(keys, payload) for keys, payload in commands_of(unwrap_tmux(t.output)) if keys.get(b"a") == b"T"]
    assert images, "answered passthrough did not upload a chart"
    keys, _ = images[0]
    assert keys.get(b"U") == b"1" and keys.get(b"q") == b"2", keys
    upload = t.output.find(b"a=T")
    cell = t.output.find(PLACEHOLDER)
    assert 0 <= upload < cell, "virtual placement was not uploaded before the cells"
    assert b"now" in t.output, "under-text reading was replaced by placeholders"
    assert not any(k.get(b"a") == b"p" for k, _ in commands_of(unwrap_tmux(t.output)))
    t.send(b"\x1b[B")
    t.read()
    later = [k for k, _ in commands_of(unwrap_tmux(t.output)) if k.get(b"a") == b"T"]
    assert int(later[-1][b"r"]) < int(keys[b"r"]), "scroll did not crop the placeholder chart"


def tmux_text(t):
    assert b"[>q" not in t.output, "XTVERSION was sent through the passthrough"
    t.read(0.7)
    fallback(t)
    assert PLACEHOLDER not in t.output, "a failed termtype query still drew placeholders"


def tmux_passthrough_off_is_braille(t):
    assert b"[>q" not in t.output, "XTVERSION was sent through the passthrough"
    fallback(t)
    assert PLACEHOLDER not in t.output, "passthrough off still drew placeholders"


def tmux_basic_is_braille(t):
    assert b"[>q" not in t.output, "XTVERSION was sent through the passthrough"
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


def tmux_env(termtype, passthrough="on", pane="%1"):
    """A tmux on PATH that prints client_termtype and this pane's option.

    termtype None makes the termtype command fail. passthrough None makes
    the option unreadable. pane None omits TMUX_PANE, so the query has no -t.
    A window-scoped query (-Awv) is not answered.
    """
    directory = tempfile.mkdtemp(prefix="opscope-tmux-")
    script = Path(directory) / "tmux"
    term = "exit 1" if termtype is None else f"printf '%s\\n' '{termtype}'"
    option = "exit 1" if passthrough is None else f"printf '%s\\n' '{passthrough}'"
    if pane:
        option_if = (
            "if [ \"$1\" = show-options ] && [ \"$2\" = -Apv ] && "
            "[ \"$3\" = -t ] && [ \"$4\" = '" + pane + "' ] && "
            "[ \"$5\" = allow-passthrough ]; then\n"
        )
    else:
        option_if = (
            "if [ \"$1\" = show-options ] && [ \"$2\" = -Apv ] && "
            "[ \"$3\" = allow-passthrough ]; then\n"
        )
    body = (
        "#!/bin/sh\n"
        "if [ \"$1\" = display ] && [ \"$2\" = -p ] && [ \"$3\" = '#{client_termtype}' ]; then\n"
        f"  {term}\n"
        "  exit 0\n"
        "fi\n"
        f"{option_if}"
        f"  {option}\n"
        "  exit 0\n"
        "fi\n"
        "exit 1\n"
    )
    script.write_text(body)
    script.chmod(0o755)
    path = os.environ.get("PATH", "/usr/bin")
    env = {
        "TMUX": "/tmp/tmux-0/default,1,0",
        "PATH": f"{directory}:{path}",
    }
    if pane:
        env["TMUX_PANE"] = pane
    return directory, env


def run_tmux(reply, check, termtype, passthrough="on", pane="%1"):
    directory, env = tmux_env(termtype, passthrough, pane)
    try:
        run("auto", reply, check, env)
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def herdr_pane_sends_images_inline():
    # Herdr's server answers the shared-memory probe OK and then draws
    # nothing sent through t=s, so a Herdr pane never sends the probe.
    t = Terminal(extra_env={"HERDR_ENV": "1"})
    try:
        t.read(0.4)
        probes = [c for c in t.commands() if c[0].get(b"a") == b"q" and c[0].get(b"t") == b"s"]
        assert not probes, "a Herdr pane was sent the shared-memory probe"
        assert t.images(), "no chart under Herdr"
        assert all(keys[b"t"] == b"d" for keys, _ in t.images()), "a Herdr pane used shared memory"
        t.quit()
    finally:
        t.close()


if __name__ == "__main__":
    scroll_clears_previous_image_region()
    moving_meter_keeps_unchanged_pixels()
    successful_upload_before_shared_failure()
    print("PASS shared-memory/success-before-failure")
    missing_and_late_replies_keep_their_upload_keys()
    print("PASS shared-memory/missing-and-late-replies")
    herdr_pane_sends_images_inline()
    print("PASS shared-memory/herdr-inline")
    for mode, reply, check in [("auto", "kitty", supported), ("auto", "reject", fallback),
                               ("text", "sync", fallback), ("kitty", "silent", timeout),
                               ("auto", "kitty", rejected_image),
                               ("auto", "kitty", shm_rejected), ("auto", "kitty", shm_accepted),
                               ("auto", "kitty", shared_rgb_gallery),
                               ("auto", "kitty", filled_graphics), ("text", "sync", filled_text),
                               ("auto", "kitty", cell_raster_follows_report),
                               ("auto", "kitty", hidpi_shared_crops_keep_cell_layout),
                               ("auto", "kitty", hidpi_shared_rgb_gallery),
                               ("auto", "kitty", cell_raster_rejects_absurd_and_zero),
                               ("auto", "kitty", cell_raster_follows_winsize_pixels),
                               ("auto", "kitty", cell_raster_drops_a_barren_resize),
                               ("auto", "kitty", lost_startup_cell_reply_does_not_block_resize),
                               ("auto", "kitty", ambiguous_cell_reply_retries),
                               ("auto", "kitty", expired_cell_reply_is_quarantined),
                               ("auto", "kitty", resumed_cell_reply_is_quarantined),
                               ("auto", "kitty", reclaimed_cell_reply_is_quarantined),
                               ("auto", "kitty", responsive_selection), ("text", "sync", responsive_selection),
                               ("text", "sync", underlines), ("text", "sync", plain_underlines)]:
        run(mode, reply, check)
        print(f"PASS {mode}/{reply}: {check.__name__}")
    run_tmux("kitty", tmux_placeholders, "kitty(0.40.0)", "on", "%1")
    print("PASS placeholder tmux")
    run_tmux("kitty", tmux_placeholders, "kitty(0.40.0)", "on", None)
    print("PASS placeholder tmux without TMUX_PANE")
    run_tmux("kitty", tmux_placeholders, "kitty(0.40.0)", "all")
    print("PASS placeholder tmux allow-passthrough all")
    run_tmux("kitty", tmux_passthrough_off_is_braille, "kitty(0.40.0)", "off")
    print("PASS placeholder passthrough off stays text")
    run_tmux("basic", tmux_basic_is_braille, "kitty(0.27.1)")
    print("PASS placeholder old kitty stays text")
    run("auto", "kitty", herdr_direct, {"HERDR_ENV": "1"})
    print("PASS herdr direct")
    run_tmux("silent", tmux_text, None)
    print("PASS placeholder termtype failed")
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
