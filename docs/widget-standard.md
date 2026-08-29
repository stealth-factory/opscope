# Widget package standard

[← documentation](README.md)

An opscope widget owns its complete experience in one folder:

```text
widgets/src/widgets/<name>/
├── main.rs         Rust entry point and local tests
├── help.txt        `--help` summary and controls
├── README.md       data provenance, preview, keys, and configuration
├── CONFIGURE.md    plain guidance for an AI helping a user configure it
└── settings.json   defaults, field order, and field help (when configurable)
```

Private Rust modules stay in the same folder. `agent-usage` is the largest
example.

The public `opscope` binary is different. Its source is
`widgets/src/launcher/`: it launches widgets and embeds their help and README
previews, but it is not counted or packaged as a widget. Its own `,` screen
contains only shared terminal behavior such as mouse reporting.

## Settings

Every configurable widget answers `,` in its normal navigation state. The
shared screen shows only that widget's section:

- current value and declared default
- whether the value is set or inherited
- field-specific help from `settings.json`
- the resolved config path that `load_config` reads
- a warning when the widget is using an explicitly declared legacy section

Arrow keys and `j`/`k` move the selected field. Ctrl-Y/Ctrl-E and the mouse
wheel move the viewport without changing what `Enter` will edit. `Enter`
edits (or toggles a boolean), `d` removes an override and returns to
the default, `s` reveals declared secrets, `r` reloads, `c` copies non-secret
values, and `Esc`, `q`, or `,` returns to the widget. A running widget keeps
the values it started with; the screen says to restart after a write. While
editing text, `Esc` cancels and `Enter` writes; letter and punctuation keys
belong to the value.

The writer re-reads immediately before each mutation, changes only the
selected effective JSON value, validates the result, writes a private
same-directory temporary file with exclusive no-follow creation, then
renames it. Unrelated concurrent edits are retained. Duplicate keys follow
the same last-value-wins behavior as `serde_json` and `load_config`.

The root `config.example.json` is generated from every `settings.json`:

```sh
tools/config-example.py
tools/config-example.py --check
```

The second form is part of `cargo test`.

An optional `_schema` object beside the defaults carries UI-only constraints
such as choices, integer/list element types, and numeric bounds. The
generator omits `_schema` from `config.example.json`; it exists to stop the
settings screen accepting a value the widget would silently ignore.

## AI configuration guide

`CONFIGURE.md` is documentation, not an agent skill and not authorization.
It ships inside the widget binary:

```sh
opscope latency --configure-help
```

The guide names real data sources, safe inspection, secrets, verification,
and facts that must be asked rather than guessed. It must never contain a
credential, internal hostname, LAN address, or fabricated successful
reading.

## Adding a widget

1. Add its folder and required files.
2. Add one `[[bin]]` entry in `widgets/Cargo.toml`.
3. Add its name to the launcher's registry.
4. Add its row to the root README and docs index.
5. If configurable, declare `SETTINGS`, open it on `,`, and add
   `settings.json`.
6. Regenerate `config.example.json`.
7. Run `cargo test`.

`widgets/tests/check.rs` enforces the folder contract, launcher/docs rows,
settings/example parity, key hints, help controls, config reads, poller
failures, and selected-row contrast.
