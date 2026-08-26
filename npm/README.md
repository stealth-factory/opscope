# opscope

Sci-fi terminal widgets that show only real data.

```sh
npx opscope
```

That is the launcher: a menu of the fourteen widgets, or name one and skip
it (`npx opscope latency`). The binaries it starts are the ones attached
to the GitHub release of the same version — `--version` on any of them
answers with that version, commit and date.

## Platforms

Linux x86_64 (glibc), macOS Apple Silicon, and macOS Intel. Windows,
Alpine/musl, 32-bit and Linux arm64 fail at install with a sentence
saying so.

The launcher is a thin package. Each platform is an optional dependency
npm installs only when `os` / `cpu` / `libc` match, so a Mac never
downloads the Linux binaries.

## Source

<https://github.com/stealth-factory/opscope>
