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
import ctypes
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
# pane after warm-up. Typical tabs hold 2-6 panes (~12 across tabs), so every
# limit is set for a dozen panes running at once, over SSH on mobile data:
# that is where a per-widget number that looks harmless (15 KB/s) becomes
# 650 MB an hour.
#
# Calibrated on 2026-11-18 against every local widget on main, 20 s per mode:
# text output was 1.0-6.9 KB/s (latency the highest); in Kitty mode latency
# made ~5.6 a=f edits/s (40-66 KB/s raw), ports ~0.4 whole a=T/s, and
# tailnet 4 a=T/s - the last a real defect, which these limits fail.
TYPICAL_PANES = 12
BUDGET_TOTAL_KB_S = 8.0
BUDGET_STEADY_TOTAL_KB_S = 5.0
# Kitty on a local terminal: the wire carries zlib-compressed pixels, so the
# raw figure bounds how much image churn a widget causes, not the link.
BUDGET_KITTY_RAW_RGBA_KB_S = 100.0
BUDGET_KITTY_WHOLE_UPLOADS_S = 0.5
BUDGET_KITTY_TOTAL_UPLOADS_S = 8.0
# Kitty pixels under a relay that re-sends them decoded (Herdr with
# OPSCOPE_GRAPHICS=kitty): the raw RGBA *is* the link. OPS-199 was this case
# at up to 4.9 MB/s; 16 KB/s a widget keeps a dozen panes near 200 KB/s.
BUDGET_RELAY_RAW_RGBA_KB_S = 16.0
BUDGET_MATRIX_TEXT_KB_S = 500.0

ALL_DEFAULT_MODES = ("text", "kitty", "herdr")
VALID_MODES = ("text", "kitty", "herdr", "kitty-herdr", "all")

MAX_SEQ_BYTES = 1_048_576
MAX_UPLOAD_PAYLOAD_BYTES = 16 * 1024 * 1024
MAX_READS_PER_TICK = 4
MAX_PENDING_REPLIES = 256 * 1024
# Kitty graphics `s`/`v`/`i`/`p` are unsigned 32-bit protocol integers.
KITTY_U32_MAX = 0xFFFFFFFF


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


def kitty_u32(text: Optional[str]) -> Optional[int]:
    """Return a Kitty protocol integer, or None when the field is absent or illegal."""
    if text is None:
        return None
    if not text.isascii() or not text.isdigit() or len(text) > 10:
        return None
    value = int(text)
    if 0 <= value <= KITTY_U32_MAX:
        return value
    return None


def positive_dimensions(kv: Dict[str, str]) -> Optional[Tuple[int, int]]:
    """Pixel size from `s` and `v` when both are protocol integers.

    A present but illegal field returns None, same as an omitted pair. Callers
    that must tell those apart check the keys themselves.
    """
    if "s" not in kv or "v" not in kv:
        return None
    w = kitty_u32(kv.get("s"))
    h = kitty_u32(kv.get("v"))
    if w is None or h is None or w <= 0 or h <= 0:
        return None
    return (w, h)


def parse_png_dimensions(data: bytes) -> Optional[Tuple[int, int]]:
    if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        return None
    w, h = struct.unpack(">II", data[16:24])
    if w > 0 and h > 0:
        return (w, h)
    return None


def decode_upload_payload(kv: Dict[str, str], payload_chunks: List[bytes]) -> Optional[bytes]:
    """Return the upload payload, or None when zlib output exceeds the cap."""
    raw_b64 = b"".join(payload_chunks)
    if not raw_b64:
        return b""
    try:
        decoded = base64.b64decode(raw_b64)
        if kv.get("o") == "z":
            decomp = zlib.decompressobj()
            decoded = decomp.decompress(decoded, MAX_UPLOAD_PAYLOAD_BYTES + 1)
            if len(decoded) > MAX_UPLOAD_PAYLOAD_BYTES or decomp.unconsumed_tail:
                return None
        return decoded
    except (ValueError, zlib.error):
        return b""


def upload_needs_payload(cmd_kv: Dict[str, str]) -> bool:
    """PNG carries its pixel size in the file; raw uploads can use `s` and `v`."""
    if cmd_kv.get("a", "t") not in ("T", "t", "f"):
        return False
    if cmd_kv.get("f") == "100":
        return True
    return positive_dimensions(cmd_kv) is None


def decoded_upload_rgba_bytes(kv: Dict[str, str], payload_chunks: List[bytes]) -> Optional[int]:
    """Return the RGBA byte count of an upload.

    `None` means zlib output exceeded the retained-payload cap. PNG (`f=100`)
    uses the IHDR size even when `s` and `v` are present or incomplete. A PNG
    estimate is returned even when it is larger than the payload cap, because
    the PNG bytes are not expanded. Declared raw `s`/`v` that are not protocol
    integers yield 0; the caller marks the sample invalid.
    """
    fmt = kv.get("f", "32")
    if fmt == "100":
        decoded = decode_upload_payload(kv, payload_chunks)
        if decoded is None:
            return None
        png_dims = parse_png_dimensions(decoded)
        return (png_dims[0] * png_dims[1] * 4) if png_dims is not None else 0

    if "s" in kv or "v" in kv:
        dims = positive_dimensions(kv)
        if dims is None:
            return 0
        return dims[0] * dims[1] * 4

    decoded = decode_upload_payload(kv, payload_chunks)
    if decoded is None:
        return None
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

    def _account_rgba(self, cmd_kv: Dict[str, str], payloads: List[bytes], kind: str) -> None:
        if cmd_kv.get("f") != "100" and ("s" in cmd_kv or "v" in cmd_kv):
            if positive_dimensions(cmd_kv) is None:
                self.sample_invalid = "Kitty s/v dimensions are outside the protocol range"
                return
        rgba = decoded_upload_rgba_bytes(cmd_kv, payloads)
        if rgba is None:
            self.sample_invalid = f"decompressed Kitty {kind} exceeded safety cap"
            return
        self.metrics.raw_rgba_bytes += rgba

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
            needs_payload = upload_needs_payload(cmd_kv)
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
            if self._chunk_needs_payload and not upload_needs_payload(self._chunk_kv):
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
            if kitty_u32(id_str) is None:
                id_str = None
            if kitty_u32(p_str) is None:
                p_str = None
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
            self._account_rgba(cmd_kv, payloads, "upload")
        elif action == "f":
            self.metrics.a_f += 1
            self.metrics.uploads += 1
            self._account_rgba(cmd_kv, payloads, "frame edit")
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

    @property
    def link_kb_s(self) -> float:
        """What crosses the link to the person: the wire bytes, except under a
        relay that re-sends Kitty images decoded, where it is the text plus
        the raw RGBA."""
        if self.mode == "kitty-herdr":
            return self.text_kb_s + self.raw_rgba_kb_s
        return self.total_kb_s

    @property
    def fleet_mb_h(self) -> float:
        """`link_kb_s` for `TYPICAL_PANES` panes like this one, per hour."""
        return self.link_kb_s * TYPICAL_PANES * 3600 / KB

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
            "link_kb_s": round(self.link_kb_s, 2),
            f"mb_per_hour_x{TYPICAL_PANES}": round(self.fleet_mb_h, 1),
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
                relayed = mode == "kitty-herdr"
                raw_limit = BUDGET_RELAY_RAW_RGBA_KB_S if relayed else BUDGET_KITTY_RAW_RGBA_KB_S
                if raw_kb_s > raw_limit:
                    reasons.append(
                        f"raw-RGBA {raw_kb_s:.1f} > {raw_limit:.0f} KB/s"
                        + (" (relayed decoded)" if relayed else "")
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


def _libc_waitid_nowait(pid: int) -> Tuple[Optional[int], bool, bool]:
    """Call libc `waitid(WNOWAIT)` when `os.waitid` is missing.

    macOS builds before Python 3.13 do not expose `os.waitid`. `waitpid` is not
    a substitute: it reaps the leader and frees the PGID before group cleanup.
    Returns `(exit_code_or_none, was_reaped, observation_failed)` and never
    reaps on success. `observation_failed` means the call could not tell whether
    the child is still running; that is not the same as "still running".
    """
    if sys.platform == "darwin":
        p_pid, wexited, wnohang, wnowait = 1, 4, 1, 0x20
        off_code, off_pid, off_status = 8, 12, 20
    elif sys.platform == "linux":
        p_pid = getattr(os, "P_PID", 1)
        wexited = getattr(os, "WEXITED", 4)
        wnohang = getattr(os, "WNOHANG", 1)
        wnowait = getattr(os, "WNOWAIT", 0x01000000)
        if struct.calcsize("P") == 8:
            off_code, off_pid, off_status = 8, 16, 24
        else:
            off_code, off_pid, off_status = 8, 12, 20
    else:
        return (None, False, True)

    try:
        libc = ctypes.CDLL(None, use_errno=True)
        waitid = libc.waitid
    except (OSError, AttributeError):
        return (None, False, True)

    info = ctypes.create_string_buffer(128)
    waitid.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
    waitid.restype = ctypes.c_int
    rc = waitid(p_pid, pid, ctypes.cast(info, ctypes.c_void_p), wexited | wnohang | wnowait)
    if rc != 0:
        if ctypes.get_errno() == errno.ECHILD:
            return (-1, True, False)
        return (None, False, True)

    raw = bytes(info)
    si_pid = struct.unpack_from("i", raw, off_pid)[0]
    if si_pid == 0:
        return (None, False, False)
    si_code = struct.unpack_from("i", raw, off_code)[0]
    si_status = struct.unpack_from("i", raw, off_status)[0]
    if si_code == getattr(os, "CLD_EXITED", 1):
        return (int(si_status), False, False)
    return (-int(si_status), False, False)


def peek_child_exit(pid: int) -> Tuple[Optional[int], bool, bool]:
    """Return `(exit_code_or_none, was_reaped, observation_failed)`.

    The leader stays unreaped until `terminate_process_group` has signalled the
    group, so its PGID cannot be recycled into `killpg`. This never calls
    `waitpid`, including when `os.waitid` is absent. A failed observation is
    reported separately from a child that is still running.
    """
    if hasattr(os, "waitid") and hasattr(os, "WNOWAIT") and hasattr(os, "P_PID"):
        try:
            info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            return (-1, True, False)
        except OSError:
            # os.waitid rejected the call; try libc before giving up. Do not reap.
            return _libc_waitid_nowait(pid)
        else:
            if info is None:
                return (None, False, False)
            if info.si_code == os.CLD_EXITED:
                return (int(info.si_status), False, False)
            return (-int(info.si_status), False, False)
    return _libc_waitid_nowait(pid)


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
            code, reaped, failed = peek_child_exit(pid)
            if failed:
                time.sleep(0.02)
                continue
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
                code, reaped, failed = peek_child_exit(pid)
                if failed:
                    time.sleep(0.02)
                    continue
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


def queue_terminal_reply(tx_buf: bytearray, reply: bytes, stream: "TerminalStream") -> None:
    """Append a terminal reply unless the widget has stopped draining stdin."""
    if not reply:
        return
    if len(tx_buf) + len(reply) > MAX_PENDING_REPLIES:
        if stream.sample_invalid is None:
            stream.sample_invalid = (
                "terminal reply queue exceeded "
                f"{MAX_PENDING_REPLIES // 1024} KiB; widget stopped reading replies"
            )
        return
    tx_buf.extend(reply)


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
            code, reaped, failed = peek_child_exit(pid)
            if failed and stream.sample_invalid is None:
                stream.sample_invalid = "could not observe whether the widget exited"
            elif code is not None:
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
                    queue_terminal_reply(tx_buf, stream.take_replies(), stream)

            if tx_buf and (wready or rready):
                flush_tx_buf(master_fd, tx_buf)

            if measured_ended_at is not None:
                code, reaped, failed = peek_child_exit(pid)
                if failed and stream.sample_invalid is None:
                    stream.sample_invalid = "could not observe whether the widget exited"
                elif code is not None:
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


def wchar_from_io_text(io_text: str) -> Optional[int]:
    for line in io_text.splitlines():
        if line.startswith("wchar:"):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def read_proc_wchar(pid: int) -> Optional[int]:
    try:
        io_text = (proc_dir_for(pid) / "io").read_text(encoding="ascii")
    except (OSError, ValueError):
        return None
    return wchar_from_io_text(io_text)


class ProcIoHandle:
    """`/proc/<pid>` directory fd pinned to the process that was open at arm time.

    Reopening by PID can observe a recycled process. Reads through this fd keep
    failing after the original process exits instead of following the new one.
    """

    def __init__(self, pid: int) -> None:
        self.pid = pid
        self._dirfd = os.open(f"/proc/{pid}", os.O_RDONLY | os.O_DIRECTORY)

    def _read_file(self, name: str) -> Optional[str]:
        try:
            fd = os.open(name, os.O_RDONLY, dir_fd=self._dirfd)
        except OSError:
            return None
        try:
            chunks: List[bytes] = []
            total = 0
            while total < 65536:
                block = os.read(fd, 4096)
                if not block:
                    break
                chunks.append(block)
                total += len(block)
        except OSError:
            return None
        finally:
            os.close(fd)
        return b"".join(chunks).decode("ascii", "replace")

    def read_wchar(self) -> Optional[int]:
        text = self._read_file("io")
        if text is None:
            return None
        return wchar_from_io_text(text)

    def presence(self) -> str:
        """`alive`, `gone`, or `unreadable`.

        A failed `comm` read is not proof of exit. `hidepid` can hide a live
        process. Only `ENOENT` on this pinned directory fd means the original
        process is gone.
        """
        try:
            fd = os.open("comm", os.O_RDONLY, dir_fd=self._dirfd)
        except OSError as err:
            if err.errno in (errno.ENOENT, errno.ESRCH):
                return "gone"
            return "unreadable"
        os.close(fd)
        return "alive"

    def close(self) -> None:
        try:
            os.close(self._dirfd)
        except OSError:
            # The directory fd may already be closed during shutdown.
            pass


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
    handles: Dict[int, ProcIoHandle] = {}
    try:
        try:
            primary = ProcIoHandle(pid)
        except OSError as err:
            sys.stderr.write(f"error: cannot pin /proc/{pid}: {err}\n")
            return 2
        handles[pid] = primary
        start_val = primary.read_wchar()
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

        initial: Dict[int, Tuple[int, float]] = {pid: (start_val, time.monotonic())}
        names: Dict[int, str] = {pid: shown_comm}
        for child in children:
            try:
                handle = ProcIoHandle(child)
            except OSError:
                continue
            val = handle.read_wchar()
            if val is None:
                handle.close()
                continue
            handles[child] = handle
            initial[child] = (val, time.monotonic())
            names[child] = sanitize_display_text(read_proc_comm(child))

        time.sleep(max(0.1, seconds))

        print(f"{'pid':>8}  {'comm':<20}  {'wchar KB/s':>12}  {'note'}")
        for p, (v0, t0) in initial.items():
            handle = handles[p]
            t1 = time.monotonic()
            v1 = handle.read_wchar()
            if v1 is None:
                if handle.presence() == "gone":
                    label, note = "(gone)", "original process is gone"
                else:
                    label, note = "(unread)", "I/O counter unavailable"
                print(f"{p:>8}  {names[p]:<20}  {label:>12}  {note}")
                continue
            rate = max(0, v1 - v0) / KB / max(0.001, t1 - t0)
            note = "includes all write() syscalls (tty + files/sockets)"
            if p == pid and (raw_comm == "opscope" or children):
                note = "parent (+ any reaped children); live child writes are separate"
            print(f"{p:>8}  {names[p]:<20}  {rate:>12.2f}  {note}")
    finally:
        for handle in handles.values():
            handle.close()
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
            pidfd = os.pidfd_open(kpid, 0)
            pinned.append(PinnedTarget(pid=kpid, pidfd=pidfd))
            # Signal 0 checks that this pidfd can be signalled before the
            # watchdog promises to stop the target. ENOSYS and EPERM abort
            # arming; the handles opened so far are closed below.
            signal.pidfd_send_signal(pidfd, 0)
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
    try:
        bridge = ProcIoHandle(bridge_pid)
    except OSError as err:
        sys.stderr.write(f"error: cannot pin /proc/{bridge_pid}: {err}\n")
        return 2
    prev = bridge.read_wchar()
    if prev is None:
        bridge.close()
        sys.stderr.write(f"error: cannot read /proc/{bridge_pid}/io\n")
        return 2

    try:
        targets = pin_watchdog_targets(kill_pids)
    except OSError as err:
        bridge.close()
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
            cur = bridge.read_wchar()
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
        end_val = bridge.read_wchar()
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
        bridge.close()


def format_table(results: Sequence[ModeResult]) -> str:
    header = (
        f"{'mode':<11}  {'total KB/s':>10}  {'text KB/s':>9}  {'image KB/s':>10}  "
        f"{'uploads/s':>9}  {'raw-RGBA KB/s':>13}  {f'x{TYPICAL_PANES} MB/h':>9}  "
        f"{'a=T/f/p/d':<13}  {'status'}"
    )
    lines = [header]
    for r in results:
        cmds = f"{r.metrics.a_t}/{r.metrics.a_f}/{r.metrics.a_p}/{r.metrics.a_d}"
        status = r.verdict if not r.reasons else f"{r.verdict} ({'; '.join(r.reasons)})"
        lines.append(
            f"{r.mode:<11}  {r.total_kb_s:>10.2f}  {r.text_kb_s:>9.2f}  "
            f"{r.image_kb_s:>10.2f}  {r.uploads_s:>9.2f}  {r.raw_rgba_kb_s:>13.2f}  "
            f"{r.fleet_mb_h:>9.0f}  {cmds:<13}  {status}"
        )
    lines.append(
        f"x{TYPICAL_PANES} MB/h: this widget's link bytes (wire bytes; text + raw RGBA under "
        f"kitty-herdr) for {TYPICAL_PANES} panes at once, per hour."
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

    # latency on main, measured 2026-11-18: ~5.6 a=f edits/s, ~66 KB/s raw.
    healthy = Metrics(
        text_bytes=int(2.1 * KB * 60),
        image_bytes=int(2.7 * KB * 60),
        uploads=336,
        raw_rgba_bytes=int(66 * KB * 60),
        a_f=336,
        a_p=12,
    )
    verdict_ok, reasons_ok = evaluate_budget(
        mode="kitty", widget_name="latency", elapsed_s=60.0, metrics=healthy
    )
    assert verdict_ok == "PASS", (verdict_ok, reasons_ok)
    # ...but the same pixels relayed decoded are 66 KB/s a pane: fails.
    v_relay, r_relay = evaluate_budget(
        mode="kitty-herdr", widget_name="latency", elapsed_s=60.0,
        metrics=Metrics(**{**healthy.__dict__, "a_p": 0}),
    )
    assert v_relay == "FAIL" and any("relayed" in r for r in r_relay), (v_relay, r_relay)

    # OPS-199's shape: one 570x200 chart re-uploaded whole every second.
    churn = Metrics(
        text_bytes=int(2 * KB * 60),
        image_bytes=int(3 * KB * 60),
        uploads=60,
        raw_rgba_bytes=570 * 200 * 4 * 60,
        a_t=60,
    )
    for m in ("kitty", "kitty-herdr"):
        v_churn, r_churn = evaluate_budget(mode=m, widget_name="latency", elapsed_s=60.0, metrics=churn)
        assert v_churn == "FAIL" and any("raw-RGBA" in r for r in r_churn), (m, v_churn, r_churn)
        assert any("whole uploads" in r for r in r_churn), (m, r_churn)

    # Text: the busiest healthy widget (~6.9 KB/s) passes; 9 KB/s does not.
    for kb_s, want in ((6.9, "PASS"), (9.0, "FAIL")):
        v_text, _ = evaluate_budget(
            mode="text", widget_name="latency", elapsed_s=60.0,
            metrics=Metrics(text_bytes=int(kb_s * KB * 60)),
        )
        assert v_text == want, (kb_s, v_text)

    # The 12-pane projection reads the link: raw RGBA under a relay.
    proj = ModeResult(
        mode="kitty-herdr", widget_name="x", elapsed_s=1.0,
        metrics=Metrics(text_bytes=int(1 * KB), raw_rgba_bytes=int(15 * KB)),
        verdict="PASS",
    )
    assert abs(proj.fleet_mb_h - 16 * TYPICAL_PANES * 3600 / KB) < 0.01, proj.fleet_mb_h

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
        t_flood = time.monotonic()
        res = run_mode(
            Path(sys.executable),
            "flood-test",
            [str(flood_script)],
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
        assert res.exited_early is None, res.exited_early
        assert res.metrics.a_p > 0, res.metrics
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
            try:
                pinned = pin_watchdog_targets([os.getpid()])
            except OSError as err:
                if err.errno != errno.ENOSYS:
                    raise
            else:
                try:
                    assert pinned[0].pidfd >= 0
                finally:
                    for target in pinned:
                        os.close(target.pidfd)
    finally:
        os.kill = real_kill  # type: ignore[method-assign]

    # 7. Observing a leader's exit must not reap it, or group cleanup is skipped.
    def assert_peek_leaves_leader_unreaped() -> None:
        child = os.fork()
        if child == 0:
            os._exit(42)
        try:
            deadline = time.monotonic() + 1.0
            code: Optional[int] = None
            reaped = False
            while time.monotonic() < deadline:
                code, reaped, failed = peek_child_exit(child)
                assert not failed, "could not observe child exit"
                if code is not None or reaped:
                    break
                time.sleep(0.01)
            assert reaped is False, "peek_child_exit reaped the leader"
            assert code == 42, code
            waited, status = os.waitpid(child, os.WNOHANG)
            assert waited == child, "leader was reaped before group cleanup"
            assert os.waitstatus_to_exitcode(status) == 42
            child = 0
        finally:
            if child:
                try:
                    os.kill(child, signal.SIGKILL)
                except OSError:
                    # The leader may already have exited after a failed assertion.
                    pass
                try:
                    os.waitpid(child, 0)
                except ChildProcessError:
                    # An earlier waitpid in this test already reaped the leader.
                    pass

    assert_peek_leaves_leader_unreaped()
    saved_waitid = getattr(os, "waitid", None)
    if saved_waitid is not None:
        del os.waitid
    try:
        assert_peek_leaves_leader_unreaped()
    finally:
        if saved_waitid is not None:
            os.waitid = saved_waitid  # type: ignore[attr-defined]

    def assert_helper_dies_with_leader(hide_waitid: bool) -> None:
        with tempfile.TemporaryDirectory(prefix="opscope-measure-") as tmp:
            root = Path(tmp)
            script_path = root / "leader.py"
            pid_path = root / "helper.pid"
            read_fd, write_fd = os.pipe()
            os.set_inheritable(write_fd, True)
            cancel_r, cancel_w = os.pipe()
            os.set_inheritable(cancel_r, True)
            script_path.write_text(
                "import os, select, signal, sys, time\n"
                "write_fd = int(sys.argv[1])\n"
                "cancel_r = int(sys.argv[2])\n"
                "pid_path = sys.argv[3]\n"
                "child = os.fork()\n"
                "if child == 0:\n"
                "    signal.signal(signal.SIGHUP, signal.SIG_IGN)\n"
                "    with open(pid_path, 'w') as fh:\n"
                "        fh.write(str(os.getpid()))\n"
                "    select.select([cancel_r], [], [], 30)\n"
                "    os._exit(0)\n"
                "os.close(write_fd)\n"
                "os.close(cancel_r)\n"
                "for _ in range(100):\n"
                "    if os.path.isfile(pid_path) and os.path.getsize(pid_path) > 0:\n"
                "        os._exit(0)\n"
                "    time.sleep(0.01)\n"
                "os._exit(0)\n",
                encoding="utf-8",
            )
            removed = False
            held = getattr(os, "waitid", None)
            if hide_waitid and held is not None:
                del os.waitid
                removed = True
            try:
                res = run_mode(
                    Path(sys.executable),
                    "group-test",
                    [str(script_path), str(write_fd), str(cancel_r), str(pid_path)],
                    mode="text",
                    seconds=0.4,
                    warmup=0.0,
                    cols=57,
                    rows=40,
                    cell_w=10,
                    cell_h=20,
                    steady=False,
                )
                assert res.exited_early == 0, res
                os.close(write_fd)
                write_fd = -1
                ready, _, _ = select.select([read_fd], [], [], 1.0)
                got = os.read(read_fd, 16) if ready else b"pending"
                assert ready and got == b"", (
                    f"helper still running after leader exit (hide_waitid={hide_waitid})"
                )
            finally:
                if removed and held is not None:
                    os.waitid = held  # type: ignore[attr-defined]
                if write_fd >= 0:
                    os.close(write_fd)
                os.close(read_fd)
                # Closing the cancel pipe asks a still-running helper to exit.
                # Do not signal the recorded PID: it may have been reused.
                os.close(cancel_w)
                os.close(cancel_r)

    assert_helper_dies_with_leader(False)
    assert_helper_dies_with_leader(True)

    # 8. Illegal Kitty dimensions and a non-ASCII id must not raise.
    assert positive_dimensions({"s": "9" * 310, "v": "1"}) is None
    assert positive_dimensions({"s": "10", "v": "20"}) == (10, 20)
    huge = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    huge.start_measuring()
    huge.feed(b"\x1b_Ga=T,s=" + b"9" * 40 + b",v=1,i=4,q=2\x1b\\")
    assert huge.sample_invalid == "Kitty s/v dimensions are outside the protocol range"
    verdict_huge, _reasons_huge = evaluate_budget(
        mode="kitty",
        widget_name="latency",
        elapsed_s=1.0,
        metrics=huge.metrics,
        sample_invalid=huge.sample_invalid,
    )
    assert verdict_huge == "FAIL"
    bad_id = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    bad_id.start_measuring()
    bad_id.feed("\x1b_Ga=T,i=1\ufffd,q=0\x1b\\".encode("utf-8"))
    assert bad_id.take_replies() == b""

    png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR" + struct.pack(">II", 2560, 1700) + b"\x08\x06\x00\x00\x00"
    png_est = decoded_upload_rgba_bytes({"f": "100"}, [base64.b64encode(png)])
    assert png_est == 2560 * 1700 * 4
    assert png_est is not None and png_est > MAX_UPLOAD_PAYLOAD_BYTES
    png_stream = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    png_stream.start_measuring()
    png_stream.feed(b"\x1b_Ga=T,f=100,i=3,q=2;" + base64.b64encode(png) + b"\x1b\\")
    assert png_stream.sample_invalid is None
    assert png_stream.metrics.raw_rgba_bytes == png_est
    png_header = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    png_header.start_measuring()
    png_header.feed(b"\x1b_Ga=T,f=100,s=1,v=1,i=5,q=2;" + base64.b64encode(png) + b"\x1b\\")
    assert png_header.sample_invalid is None
    assert png_header.metrics.raw_rgba_bytes == png_est

    tx = bytearray()
    capped = TerminalStream(kitty_capable=True, cell_w=10, cell_h=20)
    queue_terminal_reply(tx, b"a" * (MAX_PENDING_REPLIES + 1), capped)
    assert capped.sample_invalid is not None
    assert tx == bytearray()

    if Path("/proc").is_dir():
        pinned_io = ProcIoHandle(os.getpid())
        try:
            assert pinned_io.read_wchar() is not None
            assert pinned_io.presence() == "alive"
        finally:
            pinned_io.close()

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
    parser.add_argument(
        "--name",
        help=(
            "Widget name the budget is judged as (default: the binary's file name). "
            "Only `matrix` is judged differently, so pass `--name matrix` for a renamed copy."
        ),
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
    if args.name:
        widget_name = args.name

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
