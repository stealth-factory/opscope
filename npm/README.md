# opscope

Sci-fi terminal widgets that show only real data.

```sh
npx opscope                 # the menu
npx opscope link            # skip the menu; any widget name works
npx opscope@latest link     # pin to latest, or @0.3.0, etc.
```

That is the launcher: a menu of the fifteen widgets, or name one and skip
it. Only `opscope` is installed as a command; the other binaries stay beside
it, so `pr` and `link` never replace the coreutils commands of the same
name. The binaries it starts are the ones attached to the GitHub release of
the same version — `--version` on any of them answers with that version,
commit and date.

## Platforms

- Linux x86_64 (glibc 2.35 or newer)
- macOS Apple Silicon
- macOS Intel

The launcher is a thin package. Each platform is an optional dependency
npm installs only when `os` / `cpu` / `libc` match, so a Mac never
downloads the Linux binaries.

## Source

<https://github.com/stealth-factory/opscope>

## License

[GNU AGPL-3.0](LICENSE). Commercial licenses are available; write to
[email@wiiiimm.codes](mailto:email@wiiiimm.codes).
