# Link acquisition on Linux and macOS

`link` keeps one session model, table, selection path, scroll behaviour and
RTT chart. Only acquisition differs, and both sources read existing kernel TCP
accounting without sending a probe.

| Reading | Linux | macOS |
|---|---|---|
| inbound sockets | `ss -tlnH` plus `ss -tinH state established` | listener and connection rows from a persistent `nettop -m tcp` logger |
| NOW | `rtt` | `rtt_avg` |
| FLOOR | `minrtt` | `rtt_min` |
| JITTER | the variance half of `rtt` | `rtt_var` |
| LOSS | `bytes_retrans` against `bytes_sent` deltas | unavailable: `re-tx` is a segment count, not bytes |
| ACHIEVED | `delivery_rate` | unavailable |

The macOS boundary is deliberate. `nettop` exposes cumulative bytes and a
retransmission *segment* count, so a percentage against `bytes_out` would mix
units. The list says `n/a` and the detail screen shows the segment count with
an explanation. `nettop` also has no Linux-style delivery-rate estimate.
Calculating bytes written during the polling interval would describe a
different thing, so the list says `macOS n/a` and the detail screen explains
why.

A one-shot `nettop -L 1` takes longer to exit than this widget's command
bound, so macOS keeps a persistent logger (the same pattern `netwatch` uses)
and reads the latest sample each refresh.

Both parsers live in `widgets/src/widgets/link/parse.rs` and compile on every
target. `linux.rs` and `macos.rs` contain only command acquisition and the
mapping into the shared model. A failed command reaches the pane as an error;
it is never converted into an apparently quiet connection list.
