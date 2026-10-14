# Debian and APT installation

Opscope's Debian package targets **Linux amd64 with glibc 2.35 or newer**.
The lifecycle checks run on Ubuntu 22.04 and Debian 12; Debian 11 must refuse
installation because its glibc is below the declared dependency. Linux arm64
is not built. npm/npx remains the default and recommended installation route.

## Downloadable package

Download
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

The live [Stealth Factory repository](https://github.com/stealth-factory/apt-repo)
provides signed repository metadata and package updates. It is a third-party
source, not part of Debian or Ubuntu's default archives. npm/npx remains the
recommended method.

### One-time setup

Install the setup tools:

```sh
sudo apt update
sudo apt install ca-certificates curl gnupg
```

Download the signing key and add the source:

```sh
sudo install -d -m 0755 /etc/apt/keyrings && curl -fsSL https://stealth-factory.github.io/apt-repo/stealth-factory.asc | sudo tee /etc/apt/keyrings/stealth-factory.asc >/dev/null
echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/stealth-factory.asc] https://stealth-factory.github.io/apt-repo stable main' | sudo tee /etc/apt/sources.list.d/stealth-factory.list
```

Before installing, verify the key with
`gpg --show-keys --fingerprint /etc/apt/keyrings/stealth-factory.asc`.
It should contain exactly one primary key with fingerprint
`95AD77762F38D1AECCD7BFC92A5C45AEC00E7388`. Stop if it differs or the download
failed. These short commands do not compare the fingerprint automatically.
`signed-by` scopes the key to this source; never use `apt-key`, `trusted=yes`,
or disable signature verification.

### Install and upgrade

Only after setup succeeds, install and check the package:

```sh
sudo apt update
sudo apt install opscope
opscope --version
opscope clocks
```

APT installs the declared runtime libraries automatically. External widget
tools such as `ip`/`ss`, `ping`, Tailscale and Herdr must currently be installed
manually; they are not package dependencies. Some widgets require these tools
to run. Use `opscope doctor` to see what is missing and how to install it.

For subsequent Opscope upgrades:

```sh
sudo apt update
sudo apt install --only-upgrade opscope
```

After release asset publication, Opscope notifies the shared repository using
the existing Homebrew bot token (`GH_TOKEN` / `RELEASE_TOKEN`), carrying only the
exact source, tag and commit. The importer verifies that release and its `.deb`
checksum. No new credential is created. A separate monthly job refreshes
signed metadata for already imported packages before expiry. The repository retains immutable versions,
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
list includes both files. Publication validates signed installation and upgrade
before deploying the repository snapshot; local PR checks alone do not prove
live delivery.
