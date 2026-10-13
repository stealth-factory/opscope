# Debian packaging (OPS-67)

Implementation in progress for [OPS-67](https://linear.app/stealth-company/issue/OPS-67).

The release will package Linux amd64 binaries under `/usr/libexec/opscope`,
with only an exec wrapper at `/usr/bin/opscope`. The supported runtime floor
is glibc 2.35. npm/npx remains supported and the default installation route.

The shared `stealth-factory/apt-repo` implementation will validate release
checksums, generate indexes, and sign repository metadata. Signing credentials
and live publishing configuration require separate approval; this change does
not enable them. Official distribution archive inclusion is out of scope.
