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
    python3 -I scripts/measure-output.py ./target/debug/clocks --seconds 60 --steady

Also provides two opt-in Linux helpers for live session diagnostics:
  * `--proc-io PID` samples `/proc/PID/io` `wchar` (warning if PID is the
    `opscope` launcher or has child widgets sharing its tty).
  * `--watchdog-pid BRIDGE_PID --kill-pid WIDGET_PID` monitors a live Herdr
    client bridge and, only through a pidfd opened when the watchdog is armed,
    kills test widget PIDs if output exceeds a threshold. It refuses to arm
    when pidfd signalling is unavailable.

All KB/s rates use 1 KB = 1024 bytes.
"""

import argparse
import base64
import errno
import fcntl
import json
import math
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

# Cell raster bounds enforced by `sane_cell` in `core/src/terminal.rs`.
MIN_CELL_W = 4
MAX_CELL_W = 48
MIN_CELL_H = 8
MAX_CELL_H = 96
MAX_WINSIZE_USHORT = 65_535

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
MAX_UPLOAD_PAYLOAD_BYTES = 16 * 1024 * 1024
MAX_READS_PER_TICK = 4


def is_probe_image_id(image_id: Optional[int]) -> bool:
    if image_id is None:
        return False
    if image_id in (GRAPHICS_QUERY_ID, SHM_PROBE_ID):
        return True
    return (image_id & ANIMATION_PROBE_MASK) == ANIMATION_PROBE_PREFIX


def is_sane_cell(cell_w: int, cell_h: int) -> bool:
    """Match `sane_cell` in `core/src/terminal.rs` so core never substitutes 8x16."""
    if not (MIN_CELL_W <= cell_w <= MAX_CELL_W and MIN_CELL_H <= cell_h <= MAX_CELL_H):
        return False
    return cell_h * 4 >= cell_w * 5 and cell_h <= cell_w * 3


def sanitize_display_text(text: str) -> str:
    """Escape control characters in untrusted process names before printing."""
    out: List[str] = []
    for ch in text:
        if ch.isprintable():
            out.append(ch)
        else:
            out.append(f"\\x{ord(ch):02x}")
    return "".join(out)


def parse_kitty_kv(header: bytes) -> Dict[str, str]:
    kv: Dict[str, str] = {}
    if not header:
        return kv
    for item in header.decode("ascii", "replace").split(","):
        if "=" in item:
            k, v = item.split("=", 1)
            kv[k] = v
    return kv


def positive_dimensions(kv: Dict[str, str]) -> Optional[Tuple[int, int]]:
    s_str = kv.get("s")
    v_str = kv.get("v")
    if s_str is None or v_str is None:
        return None
    try:
        w = int(s_str)
        h = int(v_str)
    except ValueError:
        return None
    if w > 0 and h > 0:
        return (w, h)
    return None


def parse_png_dimensions(data: bytes) -> Optional[Tuple[int, int]]:
    if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        return None
    w, h = struct.unpack(">II", data[16:24])
    if w > 0 and h > 0:
        return (w, h)
    return None


def decoded_upload_rgba_bytes(kv: Dict[str, str], payload_chunks: List[bytes]) -> int:
    """Return the decoded RGBA byte count (`w * h * 4`) of an upload."""
    dims = positive_dimensions(kv)
    if dims is not None:
        return dims[0] * dims[1] * 4

    raw_b64 = b"".join(payload_chunks)
    if not raw_b64:
        return 0
    try:
        decoded = base64.b64decode(raw_b64)
        if kv.get("o") == "z":
            decomp = zlib.decompressobj()
            decoded = decomp.decompress(decoded, MAX_UPLOAD_PAYLOAD_BYTES + 1)
            if len(decoded) > MAX_UPLOAD_PAYLOAD_BYTES or decomp.unconsumed_tail:
                return MAX_UPLOAD_PAYLOAD_BYTES + 1
    except (ValueError, zlib.error):
        return 0

    fmt = kv.get("f", "32")
    if fmt == "100":
        png_dims = parse_png_dimensions(decoded)
        return (png_dims[0] * png_dims[1] * 4) if png_dims is not None else 0
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
        self.sample_invalid: Optional[str] = None

        self._buf = bytearray()
        self._warmup_prefix = 0
        self._cursor_col = 1
        # Active multi-chunk Kitty command (`m=1` ... `m=0`):
        self._chunk_kv: Optional[Dict[str, str]] = None
        self._chunk_needs_payload = False
        self._chunk_payloads: List[bytes] = []
        self._chunk_payload_bytes = 0
        self._chunk_started_measuring = False

    def start_measuring(self) -> None:
        if not self.measuring:
            self.measuring = True
            # Bytes already buffered before the boundary belong to warm-up.
            self._warmup_prefix = len(self._buf)

    def _measured_len(self, consumed_len: int) -> int:
        if self._warmup_prefix > 0:
            warm = min(self._warmup_prefix, consumed_len)
            self._warmup_prefix -= warm
            return (consumed_len - warm) if self.measuring else 0
        return consumed_len if self.measuring else 0

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
                end = self._find_dcs_st(buf, pos + 2)
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
                    self._account_text_len(2)
                    pos += 2

            else:
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
    def _find_dcs_st(buf: bytearray, start: int) -> Optional[int]:
        """Locate the outer ST of a DCS string, skipping doubled ESC inside `tmux;`."""
        n = len(buf)
        if buf[start : start + 5] == b"tmux;":
            i = start + 5
            while True:
                esc = buf.find(0x1B, i)
                if esc == -1 or esc + 1 >= n:
                    return None
                if buf[esc + 1] == 0x1B:
                    i = esc + 2
                    continue
                if buf[esc + 1] == ord("\\"):
                    return esc + 2
                i = esc + 1
        return TerminalStream._find_st(buf, start)

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
        self.metrics.text_bytes += self._measured_len(len(span))

    def _account_text_len(self, length: int) -> None:
        self.metrics.text_bytes += self._measured_len(length)

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

    def _append_chunk_payload(self, payload: bytes) -> None:
        if not payload or not self._chunk_needs_payload:
            return
        if self._chunk_payload_bytes + len(payload) > MAX_UPLOAD_PAYLOAD_BYTES:
            self.sample_invalid = (
                f"Kitty upload payload exceeded {MAX_UPLOAD_PAYLOAD_BYTES // (1024 * 1024)} MiB cap"
            )
            self._chunk_needs_payload = False
            self._chunk_payloads.clear()
            return
        self._chunk_payloads.append(payload)
        self._chunk_payload_bytes += len(payload)

    def _handle_kitty(self, body: bytes, *, wire_len: int) -> None:
        began_in_window = self.measuring and self._warmup_prefix == 0
        measured_wire = self._measured_len(wire_len)
        self.metrics.image_bytes += measured_wire

        if b";" in body:
            header, payload = body.split(b";", 1)
        else:
            header, payload = body, b""
        kv = parse_kitty_kv(header)
        more = kv.get("m") == "1"

        if self._chunk_kv is None:
            cmd_kv = dict(kv)
            started_measuring = began_in_window
            needs_payload = positive_dimensions(cmd_kv) is None and cmd_kv.get("a", "t") in (
                "T",
                "t",
                "f",
            )
            if more:
                self._chunk_kv = cmd_kv
                self._chunk_needs_payload = needs_payload
                self._chunk_payloads = []
                self._chunk_payload_bytes = 0
                self._chunk_started_measuring = started_measuring
                self._append_chunk_payload(payload)
                return
            payloads = [payload] if (needs_payload and payload) else []
        else:
            for k, v in kv.items():
                if k != "m" and k not in self._chunk_kv:
                    self._chunk_kv[k] = v
            if self._chunk_needs_payload and positive_dimensions(self._chunk_kv) is not None:
                self._chunk_needs_payload = False
                self._chunk_payloads.clear()
                self._chunk_payload_bytes = 0
            self._append_chunk_payload(payload)
            if more:
                return
            cmd_kv = self._chunk_kv
            payloads = list(self._chunk_payloads)
            started_measuring = self._chunk_started_measuring
            self._chunk_kv = None
            self._chunk_needs_payload = False
            self._chunk_payloads = []
            self._chunk_payload_bytes = 0
            self._chunk_started_measuring = False

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
            rgba = decoded_upload_rgba_bytes(cmd_kv, payloads)
            if rgba > MAX_UPLOAD_PAYLOAD_BYTES and positive_dimensions(cmd_kv) is None:
                self.sample_invalid = "decompressed Kitty upload exceeded safety cap"
            else:
                self.metrics.raw_rgba_bytes += rgba
        elif action == "f":
            self.metrics.a_f += 1
            self.metrics.uploads += 1
            rgba = decoded_upload_rgba_bytes(cmd_kv, payloads)
            if rgba > MAX_UPLOAD_PAYLOAD_BYTES and positive_dimensions(cmd_kv) is None:
                self.sample_invalid = "decompressed Kitty frame edit exceeded safety cap"
            else:
                self.metrics.raw_rgba_bytes += rgba
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
    sample_invalid: Optional[str] = None

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
    sample_invalid: Optional[str] = None,
) -> Tuple[str, List[str]]:
    if exited_early is not None:
        return "FAIL", [f"exited early after {elapsed_s:.1f}s (status {exited_early})"]
    if sample_invalid is not None:
        return "FAIL", [sample_invalid]
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


def validate_pty_geometry(cols: int, rows: int, cell_w: int, cell_h: int) -> Optional[str]:
    if cols <= 0 or rows <= 0:
        return "--cols and --rows must be > 0"
    if cols > MAX_WINSIZE_USHORT or rows > MAX_WINSIZE_USHORT:
        return f"--cols and --rows must be <= {MAX_WINSIZE_USHORT}"
    if not is_sane_cell(cell_w, cell_h):
        return (
            f"cell size {cell_w}x{cell_h} is outside opscope-core's supported raster range "
            f"({MIN_CELL_W}..={MAX_CELL_W} x {MIN_CELL_H}..={MAX_CELL_H} px, height/width in 5:4..=3:1)"
        )
    if cols * cell_w > MAX_WINSIZE_USHORT or rows * cell_h > MAX_WINSIZE_USHORT:
        return (
            f"pixel window size {cols * cell_w}x{rows * cell_h} exceeds TIOCSWINSZ limit "
            f"({MAX_WINSIZE_USHORT})"
        )
    return None


def set_winsize(fd: int, rows: int, cols: int, xpixel: int, ypixel: int) -> None:
    packed = struct.pack("HHHH", rows, cols, xpixel, ypixel)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, packed)


def peek_child_exit(pid: int) -> Tuple[Optional[int], bool]:
    """Check if `pid` exited without reaping when `waitid(WNOWAIT)` is available.

    Returns `(exit_code_or_none, was_reaped)`. Keeping the leader unreaped until
    `terminate_process_group` finishes guarantees the kernel cannot recycle its
    PID/PGID while group helpers are being signalled.
    """
    if hasattr(os, "waitid") and hasattr(os, "WNOWAIT"):
        try:
            info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            return (-1, True)
        except OSError:
            # waitid(WNOWAIT) unsupported here; fall through to waitpid below.
            pass
        else:
            if info is None:
                return (None, False)
            if info.si_code == os.CLD_EXITED:
                return (int(info.si_status), False)
            return (-int(info.si_status), False)

    try:
        waited_pid, status = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        return (-1, True)
    if waited_pid == pid:
        return (os.waitstatus_to_exitcode(status), True)
    return (None, False)


def terminate_process_group(
    pid: int,
    master_fd: Optional[int],
    *,
    leader_reaped: bool,
) -> Optional[int]:
    exit_code: Optional[int] = None

    if master_fd is not None and not leader_reaped:
        try:
            os.write(master_fd, b"q")
        except OSError:
            # Slave side may already be closed; proceed to process-group signals.
            pass

        deadline = time.monotonic() + 0.4
        while time.monotonic() < deadline:
            code, reaped = peek_child_exit(pid)
            if code is not None:
                exit_code = code
                leader_reaped = reaped
                break
            time.sleep(0.02)

    # While `pid` has not been reaped, its PGID (`pid`) cannot be recycled by
    # the kernel; always sweep the group so same-group helpers (`ping`, `curl`)
    # cannot survive even when the leader exited cleanly on `q`.
    if not leader_reaped:
        for sig, wait_s in ((signal.SIGTERM, 0.25), (signal.SIGKILL, 0.15)):
            if leader_reaped:
                break
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
                code, reaped = peek_child_exit(pid)
                if code is not None:
                    exit_code = code
                    leader_reaped = reaped
                    if sig == signal.SIGTERM and not leader_reaped:
                        # Give helpers a brief moment to honour SIGTERM before SIGKILL.
                        time.sleep(0.05)
                    break
                time.sleep(0.02)

        if not leader_reaped:
            try:
                _, status = os.waitpid(pid, 0)
                if exit_code is None:
                    exit_code = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                # Leader was already reaped before final waitpid.
                pass

    return exit_code


def flush_tx_buf(master_fd: int, tx_buf: bytearray) -> None:
    if not tx_buf:
        return
    try:
        _, wready, _ = select.select([], [master_fd], [], 0)
    except InterruptedError:
        return
    while tx_buf and wready:
        try:
            sent = os.write(master_fd, tx_buf)
            del tx_buf[:sent]
        except OSError as err:
            if err.errno in (errno.EAGAIN, errno.EWOULDBLOCK, errno.EIO):
                break
            raise
        if tx_buf:
            try:
                _, wready, _ = select.select([], [master_fd], [], 0)
            except InterruptedError:
                break


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
    geom_err = validate_pty_geometry(cols, rows, cell_w, cell_h)
    if geom_err is not None:
        raise ValueError(geom_err)

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
            os.execve(str(binary), argv, env)  # noqa: S606
        except OSError as exc:
            os.write(2, f"exec failed: {exc}\n".encode("utf-8", "replace"))
        finally:
            os._exit(127)

    os.close(slave_fd)
    flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
    fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)

    tx_buf = bytearray()
    exited_early: Optional[int] = None
    leader_reaped = False
    started_at = time.monotonic()
    measure_start = started_at + max(0.0, warmup)
    measure_end = measure_start + seconds
    measured_started_at: Optional[float] = None
    measured_ended_at: Optional[float] = None

    try:
        while True:
            code, reaped = peek_child_exit(pid)
            if code is not None:
                exited_early = code
                leader_reaped = reaped
                measured_ended_at = time.monotonic()
                break

            now = time.monotonic()
            if not stream.measuring and now >= measure_start:
                stream.start_measuring()
                measured_started_at = now
            if now >= measure_end:
                measured_ended_at = now
                break

            next_boundary = measure_start if not stream.measuring else measure_end
            timeout = max(0.0, min(0.05, next_boundary - now))
            wlist = [master_fd] if tx_buf else []
            try:
                rready, wready, _ = select.select([master_fd], wlist, [], timeout)
            except InterruptedError:
                continue

            if rready:
                reads = 0
                while reads < MAX_READS_PER_TICK:
                    now_read = time.monotonic()
                    if not stream.measuring and now_read >= measure_start:
                        stream.start_measuring()
                        measured_started_at = now_read
                    if now_read >= measure_end:
                        measured_ended_at = now_read
                        break
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
                    reads += 1
                    stream.feed(chunk)
                    reply = stream.take_replies()
                    if reply:
                        tx_buf.extend(reply)

            if tx_buf and (wready or rready):
                flush_tx_buf(master_fd, tx_buf)

            if measured_ended_at is not None:
                code, reaped = peek_child_exit(pid)
                if code is not None:
                    exited_early = code
                    leader_reaped = reaped
                break
    finally:
        terminate_process_group(
            pid,
            master_fd if exited_early is None else None,
            leader_reaped=leader_reaped,
        )
        try:
            os.close(master_fd)
        except OSError:
            # Ignore EBADF/EIO when closing the master PTY descriptor during cleanup.
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
        sample_invalid=stream.sample_invalid,
    )
    return ModeResult(
        mode=mode,
        widget_name=widget_name,
        elapsed_s=elapsed,
        metrics=stream.metrics,
        verdict=verdict,
        reasons=reasons,
        exited_early=exited_early,
        sample_invalid=stream.sample_invalid,
    )


def proc_dir_for(pid: int) -> Path:
    if pid <= 0:
        raise ValueError(f"PID must be positive, got {pid}")
    return Path("/proc") / str(int(pid))


def read_proc_wchar(pid: int) -> Optional[int]:
    try:
        io_text = (proc_dir_for(pid) / "io").read_text(encoding="ascii")
    except (OSError, ValueError):
        return None
    for line in io_text.splitlines():
        if line.startswith("wchar:"):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def read_proc_comm(pid: int) -> str:
    try:
        return (proc_dir_for(pid) / "comm").read_text(encoding="utf-8", errors="replace").strip()
    except (OSError, ValueError):
        return "?"


def read_proc_descendants(pid: int) -> List[int]:
    found: List[int] = []
    seen = {pid}
    queue = [pid]
    while queue:
        cur = queue.pop(0)
        try:
            tasks = list((proc_dir_for(cur) / "task").iterdir())
        except (OSError, ValueError):
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
                if cpid > 0 and cpid not in seen:
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
        sys.stderr.write(
            f"error: cannot read /proc/{pid}/io (process missing or insufficient permissions)\n"
        )
        return 2

    raw_comm = read_proc_comm(pid)
    shown_comm = sanitize_display_text(raw_comm)
    children = read_proc_descendants(pid)
    if raw_comm == "opscope" or children:
        child_desc = (
            ", ".join(f"{c} ({sanitize_display_text(read_proc_comm(c))})" for c in children)
            or "none yet"
        )
        sys.stderr.write(
            f"warning: PID {pid} ({shown_comm}) is a parent/launcher process (children: {child_desc}).\n"
            "         The opscope launcher runs widgets as child processes sharing its tty,\n"
            "         so /proc/<launcher>/io wchar misses live widget writes until children are reaped!\n"
            "         Measure the live widget child PID directly (shown separately below).\n"
        )

    tracked = [pid, *children]
    initial: Dict[int, int] = {}
    names: Dict[int, str] = {}
    for p in tracked:
        val = read_proc_wchar(p)
        if val is not None:
            initial[p] = val
            names[p] = sanitize_display_text(read_proc_comm(p))

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
        if p == pid and (raw_comm == "opscope" or children):
            note = "parent (+ any reaped children); live child writes are separate"
        print(f"{p:>8}  {names[p]:<20}  {rate:>12.2f}  {note}")
    return 0


def pidfd_signalling_available() -> bool:
    return hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal")


@dataclass
class PinnedTarget:
    pid: int
    pidfd: int


def pin_watchdog_targets(kill_pids: Sequence[int]) -> List[PinnedTarget]:
    """Open a pidfd for every target. Never fall back to signalling by PID.

    A PID checked at arm time can be recycled before `os.kill`. pidfd_send_signal
    stays bound to the process that was open when the watchdog was armed.
    """
    if not pidfd_signalling_available():
        raise OSError(
            errno.ENOTSUP,
            "pidfd is required to signal watchdog targets; refusing to signal by PID",
        )
    pinned: List[PinnedTarget] = []
    try:
        for kpid in kill_pids:
            pinned.append(PinnedTarget(pid=kpid, pidfd=os.pidfd_open(kpid, 0)))
    except Exception:
        for t in pinned:
            try:
                os.close(t.pidfd)
            except OSError:
                # Ignore close errors while unwinding target setup.
                pass
        raise
    return pinned


def send_pinned_signal(target: PinnedTarget, sig: signal.Signals) -> None:
    if not pidfd_signalling_available():
        raise OSError(
            errno.ENOTSUP,
            "pidfd is required to signal watchdog targets; refusing to signal by PID",
        )
    signal.pidfd_send_signal(target.pidfd, sig)


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

    try:
        targets = pin_watchdog_targets(kill_pids)
    except OSError as err:
        sys.stderr.write(f"error: cannot arm watchdog on --kill-pid target: {err}\n")
        return 2

    try:
        comm = sanitize_display_text(read_proc_comm(bridge_pid))
        print(
            f"watching PID {bridge_pid} ({comm}) for {seconds:g}s — "
            f"will kill {[t.pid for t in targets]} if wchar >= {threshold_kb_s:.0f} KB/s "
            f"for {sustained}s sustained",
            flush=True,
        )
        start = prev
        t_start = time.monotonic()
        t_prev = t_start
        peak_kb_s = 0.0
        over_seconds = 0.0

        while True:
            now = time.monotonic()
            if now - t_start >= seconds:
                break
            time.sleep(min(1.0, max(0.05, (t_start + seconds) - now)))
            t_now = time.monotonic()
            cur = read_proc_wchar(bridge_pid)
            if cur is None:
                elapsed_err = t_now - t_start
                sys.stderr.write(
                    f"error: /proc/{bridge_pid}/io became unreadable after {elapsed_err:.1f}s "
                    f"(bridge exited or access lost before {seconds:g}s window completed)\n"
                )
                return 2
            dt = max(0.001, t_now - t_prev)
            kb_s = max(0, cur - prev) / KB / dt
            peak_kb_s = max(peak_kb_s, kb_s)
            prev = cur
            t_prev = t_now

            if kb_s >= threshold_kb_s:
                over_seconds += dt
            else:
                over_seconds = 0.0

            if over_seconds + 1e-6 >= float(sustained):
                killed: List[int] = []
                denied: List[int] = []
                for target in targets:
                    for sig in (signal.SIGTERM, signal.SIGKILL):
                        try:
                            send_pinned_signal(target, sig)
                            if target.pid not in killed:
                                killed.append(target.pid)
                        except ProcessLookupError:
                            # Target process already exited before this signal arrived.
                            break
                        except PermissionError:
                            if target.pid not in denied:
                                denied.append(target.pid)
                            break
                        time.sleep(0.05)
                denied_note = f"; permission denied for {denied}" if denied else ""
                print(
                    f"STOPPED: bridge PID {bridge_pid} wrote {kb_s:.1f} KB/s "
                    f"(>= {threshold_kb_s:.0f} KB/s for {over_seconds:.1f}s, peak {peak_kb_s:.1f} KB/s); "
                    f"killed {killed}{denied_note}",
                    flush=True,
                )
                return 2

        total_dt = max(0.001, time.monotonic() - t_start)
        end_val = read_proc_wchar(bridge_pid)
        if end_val is None:
            sys.stderr.write(
                f"error: /proc/{bridge_pid}/io became unreadable at end of {total_dt:.1f}s window\n"
            )
            return 2
        mean_kb_s = max(0, end_val - start) / KB / total_dt
        print(
            f"OK: bridge PID {bridge_pid} mean {mean_kb_s:.2f} KB/s, peak {peak_kb_s:.2f} KB/s over {total_dt:.1f}s",
            flush=True,
        )
        return 0
    finally:
        for target in targets:
            try:
                os.close(target.pidfd)
            except OSError:
                # Ignore close errors during watchdog handle cleanup.
                pass


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
    import tempfile

    # 1. Startup negotiation + torn reads + probes excluded.
    s = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    s.start_measuring()
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

    # 2. Multi-chunk a=T + a=f (including c=1) + a=p + a=d + tmux passthrough + PNG f=100.
    s2 = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    s2.start_measuring()
    raw_px = bytes([10, 20, 30, 255]) * (20 * 10)
    b64 = base64.b64encode(zlib.compress(raw_px))
    half = len(b64) // 2
    png_ihdr = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR" + struct.pack(">II", 16, 8) + b"\x08\x06\x00\x00\x00"
    png_b64 = base64.b64encode(png_ihdr)
    tmux_wrapped = b"\x1bPtmux;\x1b\x1b_Ga=p,i=1610612768,p=3,c=2,r=1,C=1,q=1\x1b\x1b\\\x1b\\"
    upload = (
        b"\x1b[2;1H"
        + b"\x1b_Ga=T,f=32,t=d,o=z,s=20,v=10,i=1610612768,p=3,c=2,r=1,C=1,q=0,m=1;"
        + b64[:half]
        + b"\x1b\\"
        + b"\x1b_Gm=0;"
        + b64[half:]
        + b"\x1b\\"
        + b"\x1b_Ga=d,d=i,i=1610612768,p=2,q=2\x1b\\"
        + tmux_wrapped
        + b"\x1b_Ga=f,f=32,t=d,s=1,v=1,i=1610612768,r=2,c=1,X=1,q=1;AAAAAA==\x1b\\"
        + b"\x1b_Ga=f,f=32,t=d,o=z,s=5,v=4,x=0,y=0,i=1610612768,r=1,X=1,q=1,m=1;"
        + b64[:10]
        + b"\x1b\\"
        + b"\x1b_Ga=f,m=0;"
        + b64[10:20]
        + b"\x1b\\"
        + b"\x1b_Ga=T,f=100,t=d,i=1610612769,p=1,q=2;"
        + png_b64
        + b"\x1b\\"
    )
    for i in range(0, len(upload), 7):
        s2.feed(upload[i : i + 7])
    rep2 = s2.take_replies()
    assert rep2 == b"\x1b_Gi=1610612768,p=3;OK\x1b\\", rep2
    assert s2.metrics.text_bytes == len(b"\x1b[2;1H")
    assert s2.metrics.image_bytes == len(upload) - len(b"\x1b[2;1H")
    assert s2.metrics.a_t == 2
    assert s2.metrics.a_f == 2
    assert s2.metrics.a_p == 1
    assert s2.metrics.a_d == 1
    assert s2.metrics.uploads == 4
    assert s2.metrics.raw_rgba_bytes == (20 * 10 * 4) + 4 + (5 * 4 * 4) + (16 * 8 * 4)

    # 3. Warm-up straddling sequence charges only post-warm-up bytes.
    s3 = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    warm_part = b"\x1b_Ga=T,f=32,t=d,s=10,v=10,i=1610612768,p=1,q=1;AAAA"
    post_part = b"BBBB\x1b\\"
    s3.feed(warm_part)
    s3.start_measuring()
    s3.feed(post_part)
    assert s3.metrics.image_bytes == len(post_part), s3.metrics
    assert s3.metrics.uploads == 0, "upload started during warm-up should not count"

    # 4. Budget & geometry checks.
    assert validate_pty_geometry(57, 40, 10, 20) is None
    assert validate_pty_geometry(57, 40, 10, 10) is not None
    assert validate_pty_geometry(2000, 40, 40, 80) is not None
    assert sanitize_display_text("bad\x1b[31mname") == "bad\\x1b[31mname"

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

    # 5. Non-stop PTY flood terminates promptly at `--seconds` deadline.
    with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as tf:
        tf.write(
            "#!/usr/bin/env python3\n"
            "import os\n"
            "chunk = b'\\x1b_Ga=p,i=1,p=1,q=2\\x1b\\\\' * 400\n"
            "while True:\n"
            "    try:\n"
            "        os.write(1, chunk)\n"
            "    except OSError:\n"
            "        break\n"
        )
        flood_script = Path(tf.name)
    try:
        flood_script.chmod(0o755)
        t_flood = time.monotonic()
        res = run_mode(
            flood_script,
            "flood-test",
            [],
            mode="kitty",
            seconds=0.25,
            warmup=0.0,
            cols=57,
            rows=40,
            cell_w=10,
            cell_h=20,
            steady=False,
        )
        dt_flood = time.monotonic() - t_flood
        assert dt_flood < 2.0, f"flood starved deadline ({dt_flood:.2f}s)"
        assert res.verdict == "FAIL", res
    finally:
        flood_script.unlink(missing_ok=True)

    # 6. Watchdog signalling fails closed without a pidfd. Never os.kill by PID.
    killed: List[int] = []
    real_kill = os.kill

    def refuse_pid_signal(pid: int, sig: int) -> None:
        killed.append(pid)
        raise AssertionError(f"signalled PID {pid} with {sig}")

    os.kill = refuse_pid_signal  # type: ignore[method-assign]
    try:
        try:
            send_pinned_signal(PinnedTarget(pid=os.getpid(), pidfd=-1), signal.SIGTERM)
        except OSError:
            pass
        else:
            raise AssertionError("send_pinned_signal must fail closed without a live pidfd")
        assert killed == [], killed

        saved_open = getattr(os, "pidfd_open", None)
        saved_send = getattr(signal, "pidfd_send_signal", None)
        if saved_open is not None:
            del os.pidfd_open
        if saved_send is not None:
            del signal.pidfd_send_signal
        try:
            try:
                pin_watchdog_targets([os.getpid()])
            except OSError as err:
                assert err.errno == errno.ENOTSUP, err
            else:
                raise AssertionError("watchdog armed without pidfd")
            try:
                send_pinned_signal(PinnedTarget(pid=os.getpid(), pidfd=1), signal.SIGTERM)
            except OSError as err:
                assert err.errno == errno.ENOTSUP, err
            else:
                raise AssertionError("send_pinned_signal armed without pidfd")
        finally:
            if saved_open is not None:
                os.pidfd_open = saved_open  # type: ignore[attr-defined]
            if saved_send is not None:
                signal.pidfd_send_signal = saved_send  # type: ignore[attr-defined]

        if pidfd_signalling_available():
            pinned = pin_watchdog_targets([os.getpid()])
            try:
                assert pinned[0].pidfd >= 0
            finally:
                for target in pinned:
                    os.close(target.pidfd)
    finally:
        os.kill = real_kill  # type: ignore[method-assign]

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


def parse_positive_pids(spec: str) -> List[int]:
    pids: List[int] = []
    for raw in spec.split(","):
        tok = raw.strip()
        if not tok:
            continue
        try:
            pid = int(tok)
        except ValueError as err:
            raise ValueError(f"invalid PID '{tok}' in --kill-pid") from err
        if pid <= 0:
            raise ValueError(f"--kill-pid entries must be positive PIDs, got {pid}")
        if pid not in pids:
            pids.append(pid)
    return pids


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
        help="Exit 0 when a completed run exceeds the data budget (early exits still fail)",
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
        help="Comma-separated positive test widget PID(s) to kill if --watchdog-pid trips",
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
        help="Sustained seconds over --threshold-kb before watchdog kills test widgets (default: 2)",
    )
    parser.add_argument("--self-test", action="store_true", help="Run parser and budget unit tests")

    args = parser.parse_args(raw_argv)

    if not math.isfinite(args.seconds) or args.seconds <= 0:
        parser.error("--seconds must be a finite number > 0")
    if not math.isfinite(args.warmup) or args.warmup < 0:
        parser.error("--warmup must be a finite number >= 0")
    if not math.isfinite(args.threshold_kb) or args.threshold_kb <= 0:
        parser.error("--threshold-kb must be a finite number > 0")
    if args.sustained <= 0:
        parser.error("--sustained must be >= 1")

    if args.self_test:
        return run_self_tests()

    if args.proc_io is not None:
        if args.proc_io <= 0:
            parser.error("--proc-io requires a positive PID")
        return run_proc_io_sampler(args.proc_io, args.seconds)

    if args.watchdog_pid is not None:
        if args.watchdog_pid <= 0:
            parser.error("--watchdog-pid requires a positive PID")
        try:
            kill_pids = parse_positive_pids(args.kill_pid)
        except ValueError as err:
            parser.error(str(err))
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

    geom_err = validate_pty_geometry(args.cols, args.rows, args.cell_width, args.cell_height)
    if geom_err is not None:
        parser.error(geom_err)

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

    broken = any(r.exited_early is not None or r.sample_invalid is not None for r in results)
    if broken:
        return 1
    failed = any(r.verdict == "FAIL" for r in results)
    return 1 if (failed and not args.no_fail) else 0


if __name__ == "__main__":
    sys.exit(main())
