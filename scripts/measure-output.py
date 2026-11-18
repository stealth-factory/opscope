#!/usr/bin/env python3
# opscope - small dependency-free terminal widgets
# Copyright (C) 2026 William Li
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published
# by the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""Measure a widget's terminal output rate in text, Kitty, and Herdr modes.

Runs a widget binary on a local pseudo-terminal (never over SSH by default),
answers opscope-core's startup and runtime terminal queries, splits output
into text bytes and Kitty graphics bytes, counts a=T / a=f / a=p / a=d
commands plus the decoded raw RGBA bytes behind each pixel upload, and checks
each mode against the repo's per-widget data budget.

Run with isolated Python (`python3 -I`):

    python3 -I scripts/measure-output.py ./target/debug/latency --seconds 60
    python3 -I scripts/measure-output.py clocks --seconds 60 --steady

Also provides two opt-in Linux helpers for live session diagnostics:
  * `--proc-io PID` samples `/proc/PID/io` `wchar` (warning if PID is the
    `opscope` launcher or has child widgets sharing its tty).
  * `--watchdog-pid BRIDGE_PID --kill-pid WIDGET_PID` monitors a live Herdr
    client bridge and kills test widget PIDs if output exceeds a threshold.

All KB/s rates use 1 KB = 1024 bytes.
"""

import argparse
import base64
import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

KB = 1024.0

# Capability-probe image IDs used by opscope-core (`terminal.rs`, `shm.rs`,
# `graphics.rs`). None of these are widget chart/icon uploads.
GRAPHICS_QUERY_ID = 2_147_483_646
SHM_PROBE_ID = 2_147_483_645
ANIMATION_PROBE_MASK = 0xF000_0000
ANIMATION_PROBE_PREFIX = 0x2000_0000

# Budget thresholds (1 KB = 1024 B) for a single widget in a 57x40 (10x20 px)
# pane after warm-up. Typical tabs hold 2-6 panes (~12 across tabs).
BUDGET_TOTAL_KB_S = 15.0
BUDGET_STEADY_TOTAL_KB_S = 10.0
BUDGET_KITTY_RAW_RGBA_KB_S = 600.0
BUDGET_KITTY_WHOLE_UPLOADS_S = 2.00
BUDGET_KITTY_TOTAL_UPLOADS_S = 20.00
BUDGET_MATRIX_TEXT_KB_S = 500.0

ALL_DEFAULT_MODES = ("text", "kitty", "herdr")
VALID_MODES = ("text", "kitty", "herdr", "kitty-herdr", "all")

MAX_SEQ_BYTES = 1_048_576


def is_probe_image_id(image_id: Optional[int]) -> bool:
    if image_id is None:
        return False
    if image_id in (GRAPHICS_QUERY_ID, SHM_PROBE_ID):
        return True
    return (image_id & ANIMATION_PROBE_MASK) == ANIMATION_PROBE_PREFIX


def parse_kitty_kv(header: bytes) -> Dict[str, str]:
    kv: Dict[str, str] = {}
    if not header:
        return kv
    for item in header.decode("ascii", "replace").split(","):
        if "=" in item:
            k, v = item.split("=", 1)
            kv[k] = v
    return kv


def decoded_upload_rgba_bytes(kv: Dict[str, str], payload_chunks: List[bytes]) -> int:
    """Return the decoded RGBA byte count (`w * h * 4`) of an upload."""
    s_str = kv.get("s")
    v_str = kv.get("v")
    if s_str is not None and v_str is not None:
        try:
            w = int(s_str)
            h = int(v_str)
            if w > 0 and h > 0:
                return w * h * 4
        except ValueError:
            pass
    raw_b64 = b"".join(payload_chunks)
    if not raw_b64:
        return 0
    try:
        decoded = base64.b64decode(raw_b64)
        if kv.get("o") == "z":
            decoded = zlib.decompress(decoded)
    except Exception:
        return 0
    fmt = kv.get("f", "32")
    if fmt == "24":
        return (len(decoded) // 3) * 4
    return len(decoded)


@dataclass
class Metrics:
    text_bytes: int = 0
    image_bytes: int = 0
    uploads: int = 0
    raw_rgba_bytes: int = 0
    a_t: int = 0
    a_f: int = 0
    a_p: int = 0
    a_d: int = 0

    @property
    def total_bytes(self) -> int:
        return self.text_bytes + self.image_bytes


class TerminalStream:
    """Stateful escape-sequence splitter and fake-terminal responder."""

    def __init__(
        self,
        *,
        kitty_capable: bool,
        cell_w: int,
        cell_h: int,
    ) -> None:
        self.kitty_capable = kitty_capable
        self.cell_w = cell_w
        self.cell_h = cell_h
        self.measuring = False
        self.metrics = Metrics()
        self.replies: bytearray = bytearray()

        self._buf = bytearray()
        self._cursor_col = 1
        # Active multi-chunk Kitty command (`m=1` ... `m=0`):
        self._chunk_kv: Optional[Dict[str, str]] = None
        self._chunk_payloads: List[bytes] = []
        self._chunk_wire_bytes: int = 0
        self._chunk_started_measuring: bool = False

    def take_replies(self) -> bytes:
        if not self.replies:
            return b""
        out = bytes(self.replies)
        self.replies.clear()
        return out

    def feed(self, data: bytes) -> None:
        if not data:
            return
        self._buf.extend(data)
        buf = self._buf
        n = len(buf)
        pos = 0

        while pos < n:
            esc = buf.find(0x1B, pos)
            if esc == -1:
                self._account_plain(buf[pos:n])
                pos = n
                break
            if esc > pos:
                self._account_plain(buf[pos:esc])
                pos = esc

            if pos + 1 >= n:
                break
            nxt = buf[pos + 1]

            if nxt == ord("_"):  # APC: ESC _ ... ESC \
                end = self._find_st(buf, pos + 2)
                if end is None:
                    if n - pos > MAX_SEQ_BYTES:
                        self._account_text_len(n - pos)
                        pos = n
                    break
                seq = bytes(buf[pos:end])
                self._handle_apc(seq)
                pos = end

            elif nxt == ord("]"):  # OSC: ESC ] ... (BEL | ESC \)
                end = self._find_osc_end(buf, pos + 2)
                if end is None:
                    if n - pos > 65536:
                        self._account_text_len(n - pos)
                        pos = n
                    break
                seq = bytes(buf[pos:end])
                self._account_text_len(len(seq))
                self._handle_osc(seq)
                pos = end

            elif nxt == ord("P"):  # DCS: ESC P ... ESC \
                end = self._find_st(buf, pos + 2)
                if end is None:
                    if n - pos > MAX_SEQ_BYTES:
                        self._account_text_len(n - pos)
                        pos = n
                    break
                seq = bytes(buf[pos:end])
                self._handle_dcs(seq)
                pos = end

            elif nxt == ord("["):  # CSI: ESC [ <0x20..0x3f>* <0x40..0x7e>
                j = pos + 2
                while j < n and 0x20 <= buf[j] <= 0x3F:
                    j += 1
                if j >= n:
                    if n - pos > 256:
                        self._account_text_len(n - pos)
                        pos = n
                    break
                final = buf[j]
                if 0x40 <= final <= 0x7E:
                    seq = bytes(buf[pos : j + 1])
                    self._account_text_len(len(seq))
                    self._handle_csi(seq[2:])
                    pos = j + 1
                else:
                    # Malformed CSI; consume ESC [ and continue.
                    self._account_text_len(2)
                    pos += 2

            else:
                # Two-byte ESC sequence (e.g. ESC \, ESC 7, ESC 8).
                self._account_text_len(2)
                pos += 2

        if pos > 0:
            del buf[:pos]

    @staticmethod
    def _find_st(buf: bytearray, start: int) -> Optional[int]:
        i = start
        n = len(buf)
        while True:
            esc = buf.find(0x1B, i)
            if esc == -1 or esc + 1 >= n:
                return None
            if buf[esc + 1] == ord("\\"):
                return esc + 2
            i = esc + 1

    @staticmethod
    def _find_osc_end(buf: bytearray, start: int) -> Optional[int]:
        bel = buf.find(0x07, start)
        st = TerminalStream._find_st(buf, start)
        if bel != -1 and (st is None or bel + 1 < st):
            return bel + 1
        return st

    def _account_plain(self, span: memoryview | bytearray | bytes) -> None:
        if 0x0D in span:
            self._cursor_col = 1
        if self.measuring:
            self.metrics.text_bytes += len(span)

    def _account_text_len(self, length: int) -> None:
        if self.measuring:
            self.metrics.text_bytes += length

    def _handle_csi(self, body: bytes) -> None:
        if body == b"?2026$p":
            self.replies.extend(b"\x1b[?2026;2$y")
        elif body == b"?u":
            self.replies.extend(b"\x1b[?0u")
        elif body in (b"c", b"0c"):
            self.replies.extend(b"\x1b[?62;c")
        elif body == b"16t":
            self.replies.extend(f"\x1b[6;{self.cell_h};{self.cell_w}t".encode("ascii"))
        elif body == b"6n":
            self.replies.extend(f"\x1b[1;{self._cursor_col}R".encode("ascii"))

    def _handle_osc(self, seq: bytes) -> None:
        body = seq[2:-2] if seq.endswith(b"\x1b\\") else seq[2:-1]
        if body == b"10;?":
            self.replies.extend(b"\x1b]10;rgb:e1e1/ebeb/f5f5\x1b\\")
        elif body == b"11;?":
            self.replies.extend(b"\x1b]11;rgb:1010/1414/1a1a\x1b\\")
        elif body.startswith(b"66;w=2; ") or body.startswith(b"66;s=2; "):
            self._cursor_col += 2

    def _handle_dcs(self, seq: bytes) -> None:
        body = seq[2:-2]
        if body.startswith(b"tmux;"):
            inner = body[5:].replace(b"\x1b\x1b", b"\x1b")
            if inner.startswith(b"\x1b_G") and inner.endswith(b"\x1b\\"):
                self._handle_kitty(inner[3:-2], wire_len=len(seq))
                return
        self._account_text_len(len(seq))
        if body == b"+q536d756c78;536574756c63":
            self.replies.extend(b"\x1bP1+r536d756c78=1;536574756c63=1\x1b\\")

    def _handle_apc(self, seq: bytes) -> None:
        body = seq[2:-2]
        if body.startswith(b"G"):
            self._handle_kitty(body[1:], wire_len=len(seq))
        else:
            self._account_text_len(len(seq))

    def _handle_kitty(self, body: bytes, *, wire_len: int) -> None:
        if self.measuring:
            self.metrics.image_bytes += wire_len

        if b";" in body:
            header, payload = body.split(b";", 1)
        else:
            header, payload = body, b""
        kv = parse_kitty_kv(header)
        more = kv.get("m") == "1"

        if self._chunk_kv is None:
            cmd_kv = dict(kv)
            started_measuring = self.measuring
            payloads = [payload] if payload else []
            if more:
                self._chunk_kv = cmd_kv
                self._chunk_payloads = payloads
                self._chunk_wire_bytes = wire_len
                self._chunk_started_measuring = started_measuring
                return
        else:
            # Continuation chunk: merge any extra keys without overwriting
            # first-chunk keys (`a`, `s`, `v`, `i`, `p`, `q`).
            for k, v in kv.items():
                if k != "m" and k not in self._chunk_kv:
                    self._chunk_kv[k] = v
            if payload:
                self._chunk_payloads.append(payload)
            self._chunk_wire_bytes += wire_len
            if more:
                return
            cmd_kv = self._chunk_kv
            payloads = self._chunk_payloads
            started_measuring = self._chunk_started_measuring or self.measuring
            self._chunk_kv = None
            self._chunk_payloads = []
            self._chunk_wire_bytes = 0
            self._chunk_started_measuring = False

        # Reply according to the Kitty graphics protocol (`q=0` default replies
        # OK or error; `q=1` replies errors only; `q=2` is silent).
        if self.kitty_capable:
            q_mode = cmd_kv.get("q", "0")
            id_str = cmd_kv.get("i")
            p_str = cmd_kv.get("p")
            if (id_str == str(SHM_PROBE_ID) or cmd_kv.get("t") == "s") and id_str:
                if q_mode != "2":
                    self.replies.extend(
                        f"\x1b_Gi={id_str};EBADF:shm disabled in harness\x1b\\".encode("ascii")
                    )
            elif q_mode == "0" and id_str:
                if p_str and cmd_kv.get("a", "t") in ("T", "t", "p"):
                    self.replies.extend(
                        f"\x1b_Gi={id_str},p={p_str};OK\x1b\\".encode("ascii")
                    )
                else:
                    self.replies.extend(f"\x1b_Gi={id_str};OK\x1b\\".encode("ascii"))

        if not started_measuring:
            return

        action = cmd_kv.get("a", "t")
        image_id: Optional[int] = None
        if "i" in cmd_kv:
            try:
                image_id = int(cmd_kv["i"])
            except ValueError:
                image_id = None

        if action == "q" or is_probe_image_id(image_id):
            return

        if action in ("T", "t"):
            self.metrics.a_t += 1
            self.metrics.uploads += 1
            self.metrics.raw_rgba_bytes += decoded_upload_rgba_bytes(cmd_kv, payloads)
        elif action == "f":
            # `r=2,c=1` in `frame_edit` clones the root frame before `a=c`
            # compose; pixel-uploading `a=f` edits carry no `c=` copy source.
            if "c" not in cmd_kv:
                self.metrics.a_f += 1
                self.metrics.uploads += 1
                self.metrics.raw_rgba_bytes += decoded_upload_rgba_bytes(cmd_kv, payloads)
        elif action == "p":
            self.metrics.a_p += 1
        elif action == "d":
            self.metrics.a_d += 1


@dataclass
class ModeResult:
    mode: str
    widget_name: str
    elapsed_s: float
    metrics: Metrics
    verdict: str
    reasons: List[str] = field(default_factory=list)
    exited_early: Optional[int] = None

    @property
    def total_kb_s(self) -> float:
        return self.metrics.total_bytes / KB / self.elapsed_s if self.elapsed_s > 0 else 0.0

    @property
    def text_kb_s(self) -> float:
        return self.metrics.text_bytes / KB / self.elapsed_s if self.elapsed_s > 0 else 0.0

    @property
    def image_kb_s(self) -> float:
        return self.metrics.image_bytes / KB / self.elapsed_s if self.elapsed_s > 0 else 0.0

    @property
    def uploads_s(self) -> float:
        return self.metrics.uploads / self.elapsed_s if self.elapsed_s > 0 else 0.0

    @property
    def raw_rgba_kb_s(self) -> float:
        return self.metrics.raw_rgba_bytes / KB / self.elapsed_s if self.elapsed_s > 0 else 0.0

    def to_dict(self) -> Dict[str, object]:
        return {
            "mode": self.mode,
            "widget": self.widget_name,
            "seconds": round(self.elapsed_s, 3),
            "total_kb_s": round(self.total_kb_s, 2),
            "text_kb_s": round(self.text_kb_s, 2),
            "image_kb_s": round(self.image_kb_s, 2),
            "uploads_s": round(self.uploads_s, 2),
            "raw_rgba_kb_s": round(self.raw_rgba_kb_s, 2),
            "a_T": self.metrics.a_t,
            "a_f": self.metrics.a_f,
            "a_p": self.metrics.a_p,
            "a_d": self.metrics.a_d,
            "verdict": self.verdict,
            "reasons": self.reasons,
        }


def evaluate_budget(
    *,
    mode: str,
    widget_name: str,
    elapsed_s: float,
    metrics: Metrics,
    steady: bool = False,
    exited_early: Optional[int] = None,
) -> Tuple[str, List[str]]:
    if exited_early is not None:
        return "FAIL", [f"exited early after {elapsed_s:.1f}s (status {exited_early})"]
    if elapsed_s <= 0:
        return "FAIL", ["zero measurement window"]

    total_kb_s = metrics.total_bytes / KB / elapsed_s
    raw_kb_s = metrics.raw_rgba_bytes / KB / elapsed_s
    uploads_s = metrics.uploads / elapsed_s
    whole_s = metrics.a_t / elapsed_s
    is_matrix = widget_name == "matrix"
    reasons: List[str] = []

    if mode == "text":
        if metrics.image_bytes > 0 or metrics.uploads > 0:
            reasons.append("emitted Kitty image bytes in text mode")
        if is_matrix:
            if total_kb_s > BUDGET_MATRIX_TEXT_KB_S:
                reasons.append(f"total {total_kb_s:.1f} > {BUDGET_MATRIX_TEXT_KB_S:.0f} KB/s")
            elif not reasons:
                return "EXEMPT", ["matrix 30fps text by design"]
        else:
            limit = BUDGET_STEADY_TOTAL_KB_S if steady else BUDGET_TOTAL_KB_S
            if total_kb_s > limit:
                reasons.append(f"total {total_kb_s:.2f} > {limit:.1f} KB/s")

    elif mode == "herdr":
        if metrics.image_bytes > 0 or metrics.uploads > 0 or metrics.a_p > 0:
            reasons.append("emitted Kitty graphics under default HERDR_ENV=1")
        if is_matrix:
            if total_kb_s > BUDGET_MATRIX_TEXT_KB_S:
                reasons.append(f"total {total_kb_s:.1f} > {BUDGET_MATRIX_TEXT_KB_S:.0f} KB/s")
            elif not reasons:
                return "EXEMPT", ["matrix 30fps text by design"]
        else:
            limit = BUDGET_STEADY_TOTAL_KB_S if steady else BUDGET_TOTAL_KB_S
            if total_kb_s > limit:
                reasons.append(f"total {total_kb_s:.2f} > {limit:.1f} KB/s")

    elif mode in ("kitty", "kitty-herdr"):
        if is_matrix:
            if metrics.image_bytes > 0 or metrics.uploads > 0:
                reasons.append("matrix emitted Kitty image bytes")
            if total_kb_s > BUDGET_MATRIX_TEXT_KB_S:
                reasons.append(f"total {total_kb_s:.1f} > {BUDGET_MATRIX_TEXT_KB_S:.0f} KB/s")
            elif not reasons:
                return "EXEMPT", ["matrix 30fps text by design"]
        else:
            limit = BUDGET_STEADY_TOTAL_KB_S if steady else BUDGET_TOTAL_KB_S
            if total_kb_s > limit:
                reasons.append(f"total {total_kb_s:.2f} > {limit:.1f} KB/s")
            if steady:
                if metrics.uploads > 0 or metrics.raw_rgba_bytes > 0:
                    reasons.append(
                        f"steady screen uploaded {metrics.uploads} image(s) ({raw_kb_s:.2f} KB/s raw)"
                    )
            else:
                if raw_kb_s > BUDGET_KITTY_RAW_RGBA_KB_S:
                    reasons.append(
                        f"raw-RGBA {raw_kb_s:.1f} > {BUDGET_KITTY_RAW_RGBA_KB_S:.0f} KB/s"
                    )
                if whole_s > BUDGET_KITTY_WHOLE_UPLOADS_S:
                    reasons.append(
                        f"whole uploads a=T {whole_s:.2f}/s > {BUDGET_KITTY_WHOLE_UPLOADS_S:.2f}/s"
                    )
                if uploads_s > BUDGET_KITTY_TOTAL_UPLOADS_S:
                    reasons.append(
                        f"uploads {uploads_s:.2f}/s > {BUDGET_KITTY_TOTAL_UPLOADS_S:.2f}/s"
                    )
            if mode == "kitty-herdr" and metrics.a_p > 0:
                reasons.append(f"sent {metrics.a_p} a=p re-placement(s) under HERDR_ENV=1")

    if reasons:
        return "FAIL", reasons
    return "PASS", []


def resolve_widget_binary(spec: str) -> Tuple[Path, str]:
    candidate = Path(spec)
    if "/" in spec or candidate.exists():
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            raise FileNotFoundError(f"widget binary not found or not executable: {spec}")
        return candidate.resolve(), candidate.name

    repo_root = Path(__file__).resolve().parent.parent
    search_roots = [Path.cwd(), repo_root]
    for root in search_roots:
        for profile in ("debug", "release"):
            path = root / "target" / profile / spec
            if path.is_file() and os.access(path, os.X_OK):
                return path.resolve(), spec

    raise FileNotFoundError(
        f"could not find built binary for '{spec}' under ./target/debug/ or ./target/release/ "
        f"(build it first with `cargo build --bin {spec}`)"
    )


def build_mode_env(mode: str) -> Dict[str, str]:
    env = dict(os.environ)
    for key in ("TMUX", "TMUX_PANE", "NO_COLOR", "OPSCOPE_GRAPHICS", "HERDR_ENV"):
        env.pop(key, None)

    if mode == "text":
        env["TERM"] = "xterm-256color"
        env["OPSCOPE_GRAPHICS"] = "text"
    elif mode == "kitty":
        env["TERM"] = "xterm-kitty"
    elif mode == "herdr":
        env["TERM"] = "xterm-kitty"
        env["HERDR_ENV"] = "1"
    elif mode == "kitty-herdr":
        env["TERM"] = "xterm-kitty"
        env["HERDR_ENV"] = "1"
        env["OPSCOPE_GRAPHICS"] = "kitty"
    else:
        raise ValueError(f"unknown mode: {mode}")
    return env


def set_winsize(fd: int, rows: int, cols: int, xpixel: int, ypixel: int) -> None:
    packed = struct.pack("HHHH", rows, cols, xpixel, ypixel)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, packed)


def terminate_process_group(pid: int, master_fd: Optional[int]) -> Optional[int]:
    if master_fd is not None:
        try:
            os.write(master_fd, b"q")
        except OSError:
            pass

    deadline = time.monotonic() + 0.4
    while time.monotonic() < deadline:
        try:
            waited_pid, status = os.waitpid(pid, os.WNOHANG)
            if waited_pid == pid:
                return os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            return None
        time.sleep(0.02)

    for sig, wait_s in ((signal.SIGTERM, 0.5), (signal.SIGKILL, 0.5)):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            break
        except PermissionError:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                break
        sig_deadline = time.monotonic() + wait_s
        while time.monotonic() < sig_deadline:
            try:
                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    return os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                return None
            time.sleep(0.02)

    try:
        _, status = os.waitpid(pid, 0)
        return os.waitstatus_to_exitcode(status)
    except ChildProcessError:
        return None


def run_mode(
    binary: Path,
    widget_name: str,
    widget_args: Sequence[str],
    *,
    mode: str,
    seconds: float,
    warmup: float,
    cols: int,
    rows: int,
    cell_w: int,
    cell_h: int,
    steady: bool,
) -> ModeResult:
    kitty_capable = mode in ("kitty", "herdr", "kitty-herdr")
    stream = TerminalStream(kitty_capable=kitty_capable, cell_w=cell_w, cell_h=cell_h)
    env = build_mode_env(mode)

    master_fd, slave_fd = pty.openpty()
    set_winsize(slave_fd, rows, cols, cols * cell_w, rows * cell_h)

    pid = os.fork()
    if pid == 0:
        try:
            os.close(master_fd)
            os.setsid()
            fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
            os.dup2(slave_fd, 0)
            os.dup2(slave_fd, 1)
            os.dup2(slave_fd, 2)
            if slave_fd > 2:
                os.close(slave_fd)
            argv = [str(binary), *widget_args]
            os.execve(str(binary), argv, env)
        except Exception as exc:
            os.write(2, f"exec failed: {exc}\n".encode("utf-8", "replace"))
            os._exit(127)

    os.close(slave_fd)
    flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
    fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)

    tx_buf = bytearray()
    exited_early: Optional[int] = None
    started_at = time.monotonic()
    measure_start = started_at + max(0.0, warmup)
    measure_end = measure_start + seconds
    measured_started_at: Optional[float] = None
    measured_ended_at: Optional[float] = None

    try:
        while True:
            now = time.monotonic()
            if not stream.measuring and now >= measure_start:
                stream.measuring = True
                measured_started_at = now
            if now >= measure_end:
                measured_ended_at = now
                break

            try:
                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    exited_early = os.waitstatus_to_exitcode(status)
                    measured_ended_at = time.monotonic()
                    break
            except ChildProcessError:
                exited_early = -1
                measured_ended_at = time.monotonic()
                break

            next_boundary = measure_start if not stream.measuring else measure_end
            timeout = max(0.0, min(0.05, next_boundary - now))
            wlist = [master_fd] if tx_buf else []
            try:
                rready, wready, _ = select.select([master_fd], wlist, [], timeout)
            except InterruptedError:
                continue

            if rready:
                while True:
                    try:
                        chunk = os.read(master_fd, 65536)
                    except OSError as err:
                        if err.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                            break
                        if err.errno == errno.EIO:
                            chunk = b""
                        else:
                            raise
                    if not chunk:
                        break
                    now_read = time.monotonic()
                    if not stream.measuring and now_read >= measure_start:
                        stream.measuring = True
                        measured_started_at = now_read
                    stream.feed(chunk)
                    reply = stream.take_replies()
                    if reply:
                        tx_buf.extend(reply)

            if tx_buf:
                if not wready:
                    _, wready, _ = select.select([], [master_fd], [], 0)
                while tx_buf and wready:
                    try:
                        sent = os.write(master_fd, tx_buf)
                        del tx_buf[:sent]
                    except OSError as err:
                        if err.errno in (errno.EAGAIN, errno.EWOULDBLOCK, errno.EIO):
                            break
                        raise
                    if tx_buf:
                        _, wready, _ = select.select([], [master_fd], [], 0)
    finally:
        terminate_process_group(pid, master_fd if exited_early is None else None)
        try:
            os.close(master_fd)
        except OSError:
            pass

    if measured_started_at is None:
        elapsed = max(0.001, (measured_ended_at or time.monotonic()) - started_at)
    else:
        elapsed = max(0.001, (measured_ended_at or time.monotonic()) - measured_started_at)

    verdict, reasons = evaluate_budget(
        mode=mode,
        widget_name=widget_name,
        elapsed_s=elapsed,
        metrics=stream.metrics,
        steady=steady,
        exited_early=exited_early,
    )
    return ModeResult(
        mode=mode,
        widget_name=widget_name,
        elapsed_s=elapsed,
        metrics=stream.metrics,
        verdict=verdict,
        reasons=reasons,
        exited_early=exited_early,
    )


def read_proc_wchar(pid: int) -> Optional[int]:
    try:
        with open(f"/proc/{pid}/io", "r", encoding="ascii") as f:
            for line in f:
                if line.startswith("wchar:"):
                    return int(line.split()[1])
    except OSError:
        return None
    return None


def read_proc_comm(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/comm", "r", encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except OSError:
        return "?"


def read_proc_descendants(pid: int) -> List[int]:
    found: List[int] = []
    seen = {pid}
    queue = [pid]
    while queue:
        cur = queue.pop(0)
        task_dir = Path(f"/proc/{cur}/task")
        try:
            tasks = list(task_dir.iterdir())
        except OSError:
            continue
        for tdir in tasks:
            try:
                raw = (tdir / "children").read_text(encoding="ascii")
            except OSError:
                continue
            for tok in raw.split():
                try:
                    cpid = int(tok)
                except ValueError:
                    continue
                if cpid not in seen:
                    seen.add(cpid)
                    found.append(cpid)
                    queue.append(cpid)
    return found


def run_proc_io_sampler(pid: int, seconds: float) -> int:
    if not Path("/proc").is_dir():
        sys.stderr.write("error: --proc-io requires Linux /proc/<pid>/io\n")
        return 2
    start_val = read_proc_wchar(pid)
    if start_val is None:
        sys.stderr.write(f"error: cannot read /proc/{pid}/io (process missing or insufficient permissions)\n")
        return 2

    comm = read_proc_comm(pid)
    children = read_proc_descendants(pid)
    if comm == "opscope" or children:
        child_desc = ", ".join(f"{c} ({read_proc_comm(c)})" for c in children) or "none yet"
        sys.stderr.write(
            f"warning: PID {pid} ({comm}) is a parent/launcher process (children: {child_desc}).\n"
            "         The opscope launcher runs widgets as child processes sharing its tty,\n"
            "         so /proc/<launcher>/io wchar misses the widget's own writes!\n"
            "         Measure the widget child PID directly (shown separately below).\n"
        )

    tracked = [pid, *children]
    initial: Dict[int, int] = {}
    names: Dict[int, str] = {}
    for p in tracked:
        val = read_proc_wchar(p)
        if val is not None:
            initial[p] = val
            names[p] = read_proc_comm(p)

    t0 = time.monotonic()
    time.sleep(max(0.1, seconds))
    dt = max(0.001, time.monotonic() - t0)

    print(f"{'pid':>8}  {'comm':<20}  {'wchar KB/s':>12}  {'note'}")
    for p, v0 in initial.items():
        v1 = read_proc_wchar(p)
        if v1 is None:
            print(f"{p:>8}  {names[p]:<20}  {'(exited)':>12}  process exited during window")
            continue
        rate = max(0, v1 - v0) / KB / dt
        note = "includes all write() syscalls (tty + files/sockets)"
        if p == pid and children:
            note = "parent only; excludes child tty writes"
        print(f"{p:>8}  {names[p]:<20}  {rate:>12.2f}  {note}")
    return 0


def run_herdr_watchdog(
    bridge_pid: int,
    kill_pids: Sequence[int],
    *,
    threshold_kb_s: float,
    sustained: int,
    seconds: float,
) -> int:
    if not Path("/proc").is_dir():
        sys.stderr.write("error: --watchdog-pid requires Linux /proc/<pid>/io\n")
        return 2
    prev = read_proc_wchar(bridge_pid)
    if prev is None:
        sys.stderr.write(f"error: cannot read /proc/{bridge_pid}/io\n")
        return 2

    comm = read_proc_comm(bridge_pid)
    print(
        f"watching PID {bridge_pid} ({comm}) for {seconds:.0f}s — "
        f"will kill {list(kill_pids)} if wchar >= {threshold_kb_s:.0f} KB/s "
        f"for {sustained} consecutive second(s)",
        flush=True,
    )
    start = prev
    t_start = time.monotonic()
    t_prev = t_start
    peak_kb_s = 0.0
    over_streak = 0

    while True:
        now = time.monotonic()
        if now - t_start >= seconds:
            break
        time.sleep(min(1.0, max(0.05, (t_start + seconds) - now)))
        t_now = time.monotonic()
        cur = read_proc_wchar(bridge_pid)
        if cur is None:
            print(f"bridge PID {bridge_pid} exited", flush=True)
            break
        dt = max(0.001, t_now - t_prev)
        kb_s = max(0, cur - prev) / KB / dt
        peak_kb_s = max(peak_kb_s, kb_s)
        prev = cur
        t_prev = t_now

        if kb_s >= threshold_kb_s:
            over_streak += 1
        else:
            over_streak = 0

        if over_streak >= sustained:
            killed: List[int] = []
            for kpid in kill_pids:
                for sig in (signal.SIGTERM, signal.SIGKILL):
                    try:
                        os.kill(kpid, sig)
                        if kpid not in killed:
                            killed.append(kpid)
                    except ProcessLookupError:
                        pass
                    time.sleep(0.05)
            print(
                f"STOPPED: bridge PID {bridge_pid} wrote {kb_s:.1f} KB/s "
                f"(>= {threshold_kb_s:.0f} KB/s for {over_streak}s, peak {peak_kb_s:.1f} KB/s); "
                f"killed {killed}",
                flush=True,
            )
            return 2

    total_dt = max(0.001, time.monotonic() - t_start)
    end_val = read_proc_wchar(bridge_pid)
    mean_kb_s = (max(0, (end_val or prev) - start) / KB / total_dt)
    print(
        f"OK: bridge PID {bridge_pid} mean {mean_kb_s:.2f} KB/s, peak {peak_kb_s:.2f} KB/s over {total_dt:.1f}s",
        flush=True,
    )
    return 0


def format_table(results: Sequence[ModeResult]) -> str:
    header = (
        f"{'mode':<11}  {'total KB/s':>10}  {'text KB/s':>9}  {'image KB/s':>10}  "
        f"{'uploads/s':>9}  {'raw-RGBA KB/s':>13}  {'a=T/f/p/d':<13}  {'status'}"
    )
    lines = [header]
    for r in results:
        cmds = f"{r.metrics.a_t}/{r.metrics.a_f}/{r.metrics.a_p}/{r.metrics.a_d}"
        status = r.verdict if not r.reasons else f"{r.verdict} ({'; '.join(r.reasons)})"
        lines.append(
            f"{r.mode:<11}  {r.total_kb_s:>10.2f}  {r.text_kb_s:>9.2f}  "
            f"{r.image_kb_s:>10.2f}  {r.uploads_s:>9.2f}  {r.raw_rgba_kb_s:>13.2f}  "
            f"{cmds:<13}  {status}"
        )
    return "\n".join(lines)


def run_self_tests() -> int:
    # 1. Startup negotiation + torn chunked upload + probes excluded.
    s = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    s.measuring = True
    startup = (
        b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c"
        b"\x1b_Gi=2147483646,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"
        b"\x1b_Ga=t,f=24,t=d,s=1,v=1,i=536870999,q=1;AAAA\x1b\\"
        b"\x1b_Ga=f,f=24,t=d,s=1,v=1,i=536870999,c=1,X=1,z=-1,q=0;AAAA\x1b\\"
        b"\x1b_Ga=c,i=536870999,r=2,c=1,w=1,h=1,C=1,q=0\x1b\\"
        b"\r\x1b[6n\x1b]66;w=2; \x07\x1b[6n\x1b]66;s=2; \x07\x1b[6n"
        b"\x1b[?2026$p\x1b[?u\x1b[c\x1b[16t"
        b"\x1bP+q536d756c78;536574756c63\x1b\\"
        b"\x1b_Ga=q,f=32,t=s,s=1,v=1,i=2147483645,q=0,S=4;c2ht\x1b\\"
    )
    # Feed byte-by-byte to prove torn reads never break parsing or replies.
    for b in startup:
        s.feed(bytes([b]))
    replies = s.take_replies()
    assert b"\x1b_Gi=2147483646;OK\x1b\\" in replies, replies
    assert b"\x1b_Gi=536870999;OK\x1b\\" in replies, replies
    assert b"\x1b_Gi=2147483645;EBADF:" in replies, replies
    assert b"\x1b[1;1R\x1b[1;3R\x1b[1;5R" in replies, replies
    assert b"\x1b[?2026;2$y" in replies, replies
    assert b"\x1b[?0u" in replies, replies
    assert b"\x1b[6;20;10t" in replies, replies
    assert b"\x1bP1+r536d756c78=1;536574756c63=1\x1b\\" in replies, replies
    assert s.metrics.uploads == 0, f"probes counted as uploads: {s.metrics}"
    assert s.metrics.raw_rgba_bytes == 0, f"probes counted as RGBA: {s.metrics}"

    # 2. Multi-chunk a=T + multi-chunk a=f + a=p + a=d accounting.
    s2 = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    s2.measuring = True
    raw_px = bytes([10, 20, 30, 255]) * (20 * 10)  # 20x10 RGBA = 800 bytes
    b64 = base64.b64encode(zlib.compress(raw_px))
    half = len(b64) // 2
    upload = (
        b"\x1b[2;1H"
        + b"\x1b_Ga=T,f=32,t=d,o=z,s=20,v=10,i=1610612768,p=3,c=2,r=1,C=1,q=0,m=1;"
        + b64[:half]
        + b"\x1b\\"
        + b"\x1b_Gm=0;"
        + b64[half:]
        + b"\x1b\\"
        + b"\x1b_Ga=d,d=i,i=1610612768,p=2,q=2\x1b\\"
        + b"\x1b_Ga=p,i=1610612768,p=3,c=2,r=1,C=1,q=1\x1b\\"
        + b"\x1b_Ga=f,f=32,t=d,o=z,s=5,v=4,x=0,y=0,i=1610612768,r=1,X=1,q=1,m=1;"
        + b64[:10]
        + b"\x1b\\"
        + b"\x1b_Ga=f,m=0;"
        + b64[10:20]
        + b"\x1b\\"
    )
    for i in range(0, len(upload), 7):
        s2.feed(upload[i : i + 7])
    rep2 = s2.take_replies()
    assert rep2 == b"\x1b_Gi=1610612768,p=3;OK\x1b\\", rep2
    assert s2.metrics.text_bytes == len(b"\x1b[2;1H")
    assert s2.metrics.image_bytes == len(upload) - len(b"\x1b[2;1H")
    assert s2.metrics.a_t == 1
    assert s2.metrics.a_f == 1
    assert s2.metrics.a_p == 1
    assert s2.metrics.a_d == 1
    assert s2.metrics.uploads == 2
    assert s2.metrics.raw_rgba_bytes == (20 * 10 * 4) + (5 * 4 * 4)

    # 3. Budget checks: pre-#469 flood fails, current latency passes, matrix exempt.
    flood = Metrics(
        text_bytes=int(4 * KB * 60),
        image_bytes=int(20 * KB * 60),
        uploads=120,
        raw_rgba_bytes=int(2275 * KB * 60),
        a_t=120,
    )
    verdict, reasons = evaluate_budget(mode="kitty", widget_name="latency", elapsed_s=60.0, metrics=flood)
    assert verdict == "FAIL" and any("raw-RGBA" in r for r in reasons), (verdict, reasons)

    healthy = Metrics(
        text_bytes=int(4.0 * KB * 60),
        image_bytes=int(2.2 * KB * 60),
        uploads=75,
        raw_rgba_bytes=int(138 * KB * 60),
        a_t=12,
        a_f=63,
        a_p=12,
    )
    verdict_ok, reasons_ok = evaluate_budget(
        mode="kitty", widget_name="latency", elapsed_s=60.0, metrics=healthy
    )
    assert verdict_ok == "PASS", (verdict_ok, reasons_ok)

    matrix_m = Metrics(text_bytes=int(240 * KB * 10))
    verdict_mx, _ = evaluate_budget(mode="text", widget_name="matrix", elapsed_s=10.0, metrics=matrix_m)
    assert verdict_mx == "EXEMPT"

    print("self-test: ok")
    return 0


def parse_modes(raw_modes: Optional[Sequence[str]]) -> List[str]:
    if not raw_modes:
        return list(ALL_DEFAULT_MODES)
    expanded: List[str] = []
    for item in raw_modes:
        for part in item.split(","):
            part = part.strip()
            if not part:
                continue
            if part not in VALID_MODES:
                raise ValueError(f"invalid --mode '{part}' (choose from {', '.join(VALID_MODES)})")
            if part == "all":
                for m in ALL_DEFAULT_MODES:
                    if m not in expanded:
                        expanded.append(m)
            elif part not in expanded:
                expanded.append(part)
    return expanded or list(ALL_DEFAULT_MODES)


def main(argv: Optional[Sequence[str]] = None) -> int:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    widget_args: List[str] = []
    if "--" in raw_argv:
        sep = raw_argv.index("--")
        widget_args = raw_argv[sep + 1 :]
        raw_argv = raw_argv[:sep]

    parser = argparse.ArgumentParser(
        description=(
            "Measure a widget's output rate (1 KB = 1024 B) in text, Kitty, and Herdr modes "
            "using a local PTY harness."
        )
    )
    parser.add_argument(
        "widget",
        nargs="?",
        help="Path to built widget binary (e.g. ./target/debug/latency) or widget name",
    )
    parser.add_argument(
        "-s",
        "--seconds",
        type=float,
        default=15.0,
        help="Measurement window in seconds per mode, after warm-up (default: 15)",
    )
    parser.add_argument(
        "-w",
        "--warmup",
        type=float,
        default=6.0,
        help="Warm-up seconds before counting bytes/uploads so startup paint settles (default: 6)",
    )
    parser.add_argument(
        "-m",
        "--mode",
        action="append",
        dest="modes",
        help="Mode to run: text, kitty, herdr, kitty-herdr, or all (repeatable; default: text,kitty,herdr)",
    )
    parser.add_argument("--cols", type=int, default=57, help="PTY width in cells (default: 57)")
    parser.add_argument("--rows", type=int, default=40, help="PTY height in cells (default: 40)")
    parser.add_argument("--cell-width", type=int, default=10, help="Cell width in pixels (default: 10)")
    parser.add_argument("--cell-height", type=int, default=20, help="Cell height in pixels (default: 20)")
    parser.add_argument(
        "--steady",
        action="store_true",
        help="Require zero image uploads in Kitty mode (for steady screens / meter-only widgets)",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON output instead of a table")
    parser.add_argument(
        "--no-fail",
        action="store_true",
        help="Exit 0 even when a mode exceeds the data budget",
    )
    parser.add_argument("--proc-io", type=int, metavar="PID", help="Sample /proc/PID/io wchar on Linux")
    parser.add_argument(
        "--watchdog-pid",
        type=int,
        metavar="BRIDGE_PID",
        help="Watch a live Herdr client PID's /proc/PID/io wchar and kill --kill-pid on flood",
    )
    parser.add_argument(
        "--kill-pid",
        type=str,
        default="",
        help="Comma-separated test widget PID(s) to kill if --watchdog-pid trips",
    )
    parser.add_argument(
        "--threshold-kb",
        type=float,
        default=300.0,
        help="Watchdog wchar KB/s ceiling before tripping (default: 300)",
    )
    parser.add_argument(
        "--sustained",
        type=int,
        default=2,
        help="Consecutive seconds over --threshold-kb before watchdog kills test widgets (default: 2)",
    )
    parser.add_argument("--self-test", action="store_true", help="Run parser and budget unit tests")

    args = parser.parse_args(raw_argv)

    if args.self_test:
        return run_self_tests()

    if args.proc_io is not None:
        return run_proc_io_sampler(args.proc_io, args.seconds)

    if args.watchdog_pid is not None:
        kill_pids = [int(x) for x in args.kill_pid.split(",") if x.strip()]
        if not kill_pids:
            parser.error("--watchdog-pid requires --kill-pid WIDGET_PID[,PID2...]")
        return run_herdr_watchdog(
            args.watchdog_pid,
            kill_pids,
            threshold_kb_s=args.threshold_kb,
            sustained=args.sustained,
            seconds=args.seconds,
        )

    if not args.widget:
        parser.error("widget is required (or pass --self-test / --proc-io / --watchdog-pid)")
    if args.seconds <= 0:
        parser.error("--seconds must be > 0")
    if args.cols <= 0 or args.rows <= 0 or args.cell_width <= 0 or args.cell_height <= 0:
        parser.error("pane and cell dimensions must be > 0")

    try:
        modes = parse_modes(args.modes)
        binary, widget_name = resolve_widget_binary(args.widget)
    except (FileNotFoundError, ValueError) as err:
        sys.stderr.write(f"error: {err}\n")
        return 2

    results: List[ModeResult] = []
    for mode in modes:
        results.append(
            run_mode(
                binary,
                widget_name,
                widget_args,
                mode=mode,
                seconds=args.seconds,
                warmup=args.warmup,
                cols=args.cols,
                rows=args.rows,
                cell_w=args.cell_width,
                cell_h=args.cell_height,
                steady=args.steady,
            )
        )

    try:
        shown_binary = os.path.relpath(binary, Path.cwd())
        if not shown_binary.startswith((".", "/")):
            shown_binary = f"./{shown_binary}"
    except ValueError:
        shown_binary = str(binary)

    if args.json:
        print(
            json.dumps(
                {
                    "widget": widget_name,
                    "binary": shown_binary,
                    "pane": f"{args.cols}x{args.rows}",
                    "cell_px": f"{args.cell_width}x{args.cell_height}",
                    "warmup_s": args.warmup,
                    "seconds": args.seconds,
                    "results": [r.to_dict() for r in results],
                },
                indent=2,
            )
        )
    else:
        print(
            f"# {widget_name} ({shown_binary}) · {args.cols}x{args.rows} cells "
            f"({args.cell_width}x{args.cell_height} px) · {args.warmup:g}s warmup + {args.seconds:g}s window"
        )
        print(format_table(results))

    failed = any(r.verdict == "FAIL" for r in results)
    return 1 if (failed and not args.no_fail) else 0


if __name__ == "__main__":
    sys.exit(main())
