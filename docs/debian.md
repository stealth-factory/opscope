# Debian and APT installation

Opscope's Debian package targets **Linux amd64 with glibc 2.35 or newer**.
The lifecycle checks run on Ubuntu 22.04 and Debian 12; Debian 11 must refuse
installation because its glibc is below the declared dependency. Linux arm64
is not built. npm/npx remains the default and recommended installation route.

## Downloadable package

After a release containing this packaging change is published, download its
`opscope_X.Y.Z_amd64.deb` and matching `.sha256` from the
[GitHub release](https://github.com/stealth-factory/opscope/releases). In that
directory, replacing `X.Y.Z` with the release number:

```sh
sha256sum -c opscope_X.Y.Z_amd64.deb.sha256
sudo apt install ./opscope_X.Y.Z_amd64.deb
opscope clocks
```

Install a newer local `.deb` with the same `apt install ./...` command to
upgrade. Without the shared repository configured, APT cannot discover future
Opscope versions automatically. Remove with `sudo apt remove opscope`.

Every manifest-declared binary is kept under `/usr/libexec/opscope`. The only
public command is `/usr/bin/opscope`, an exec wrapper that preserves sibling
lookup and forwards arguments/signals. Coreutils' `/usr/bin/link` is untouched.
There is no Node or Rust runtime dependency. `libc6 (>= 2.35)` and `libgcc-s1`
are declared; optional host tools remain visible through `opscope doctor`.
Configuration in `~/.config/opscope/config.json`, `XDG_CONFIG_HOME` or the path
selected by `OPSCOPE_CONFIG` is outside package ownership and survives removal.
The example configuration and binary manifest are packaged in
`/usr/share/doc/opscope`; minimal images may deliberately exclude documentation.

## Signed repository

The shared [stealth-factory/apt-repo](https://github.com/stealth-factory/apt-repo)
implementation provides signed indexes and release reconciliation. **It is not
live until the owner approves signing and hosting activation.** Follow its
README for the approved key fingerprint and scoped `Signed-By` setup once it
is activated; never disable signature verification to work around setup.

After configuring the source once, use:

```sh
sudo apt update
sudo apt install opscope
sudo apt upgrade
```

The repository checks public stable releases hourly and imports the `.deb` plus
checksum automatically, using its own GitHub token. Opscope needs no new secret
or cross-repository write permission. The repository retains immutable versions,
refuses downgrades/changed bytes, and publishes signed metadata as a complete
snapshot. See its operations guide for removal of the source, key trust,
rotation, recovery, and approval requirements.

## Checks

`node --test debian/test.js` checks manifest additions, private layout, wrapper,
metadata, missing/foreign/symlinked binaries and invalid versions on any host.
PR CI builds a Linux test package using the manifest version (never `dev`) and
runs `debian/lifecycle.sh` inside disposable containers. It tests local APT
install, upgrade from an older fixture containing a retired widget, clean
removal, unchanged user config/coreutils and every binary's full build stamp.
A Debian 11 container verifies dependency refusal.

On tags, the release matrix packages only the Linux build and generates its
SHA-256 file. Tagged lifecycle tests consume the uploaded package and gate
publication. macOS never runs the Debian packer. The release's explicit asset
list includes both new files. A real approved tagged release is still needed
to prove public asset delivery; no tag or live publication is part of this PR.
