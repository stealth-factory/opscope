# Link acquisition on Linux and macOS

`link` keeps one session model, table, selection path, scroll behaviour and
RTT chart. Only acquisition differs, and both sources read existing kernel TCP
accounting without sending a probe.

| Reading | Linux | macOS |
|---|---|---|
| inbound sockets | `ss -tlnH` plus `ss -tinH state established` | listener and connection rows from `nettop -m tcp -L 1` |
| NOW | `rtt` | `rtt_avg` |
| FLOOR | `minrtt` | `rtt_min` |
| JITTER | the variance half of `rtt` | `rtt_var` |
| LOSS | `bytes_retrans` against `bytes_sent` deltas | `re-tx` against `bytes_out` deltas |
| ACHIEVED | `delivery_rate` | unavailable |

The macOS boundary is deliberate. `nettop` exposes cumulative bytes and
retransmissions, so interval loss remains a measured percentage. It does not
expose Linux's kernel delivery-rate estimate. Calculating bytes written during
the polling interval would describe a different thing, so the list says
`macOS n/a` and the detail screen explains why.

Both parsers live in `widgets/src/widgets/link/parse.rs` and compile on every
target. `linux.rs` and `macos.rs` contain only command acquisition and the
mapping into the shared model. A failed command reaches the pane as an error;
it is never converted into an apparently quiet connection list.
