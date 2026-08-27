// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

'use strict';

const fs = require('fs');
const path = require('path');

// The unscoped name is free, which is why this is `opscope` and
// not a scoped package. `npx opscope` is the command somebody
// with only Node is asked to type.
const LAUNCHER = 'opscope';

// One row per artefact the release workflow actually produces. A
// platform that is only a wish is not a row: Alpine, Windows and
// Linux arm64 fail at install because they are absent here, which is
// the sentence the issue asked for rather than a package that
// installs and then cannot run.
//
// `os` / `cpu` / `libc` are npm's own selectors. npm installs only
// the optionalDependency whose selectors match and skips the rest,
// so a Mac never downloads the Linux tarball. `rust` is the release
// tarball suffix, and is how pack.js finds the right artefact.
const PLATFORMS = [
  {
    pkg: 'opscope-linux-x64',
    os: 'linux',
    cpu: 'x64',
    libc: 'glibc',
    rust: 'x86_64-unknown-linux-gnu',
    label: 'Linux x86_64 (glibc)',
  },
  {
    pkg: 'opscope-darwin-arm64',
    os: 'darwin',
    cpu: 'arm64',
    rust: 'aarch64-apple-darwin',
    label: 'macOS Apple Silicon',
  },
  {
    pkg: 'opscope-darwin-x64',
    os: 'darwin',
    cpu: 'x64',
    rust: 'x86_64-apple-darwin',
    label: 'macOS Intel',
  },
];

// The Linux binary is built on Ubuntu 22.04. That is glibc 2.35, and
// the oldest glibc it will load on — a 20.04 machine has 2.31, the
// package would install, and then the linker would fail with a
// missing GLIBC_* symbol. npm's `libc` selector cannot see the
// version, so we compare it here and fail at install the same way
// we fail for musl.
const MIN_GLIBC = '2.35';

function parseGlibc(version) {
  const m = String(version).trim().match(/^(\d+)\.(\d+)/);
  if (!m) return null;
  return [Number(m[1]), Number(m[2])];
}

function glibcAtLeast(have, need) {
  const a = parseGlibc(have);
  const b = parseGlibc(need);
  if (!a || !b) return false;
  return a[0] > b[0] || (a[0] === b[0] && a[1] >= b[1]);
}

function glibcTooOld(h) {
  return Boolean(h && h.os === 'linux' && h.glibc && !glibcAtLeast(h.glibc, MIN_GLIBC));
}

// Every binary, read from the manifest rather than restated.
// A restated list is how a fifteenth widget ships in the tarball and
// not in the npm package, and the launcher then cannot launch it.
// `[[bin]]` is the same list `opscope` already asserts against.
function binsFromManifest(repoRoot) {
  const toml = fs.readFileSync(path.join(repoRoot, 'widgets/Cargo.toml'), 'utf8');
  const bins = [];
  for (const part of toml.split('[[bin]]').slice(1)) {
    const m = part.match(/name = "([a-z0-9-]+)"/);
    if (m) bins.push(m[1]);
  }
  bins.sort();
  if (bins.length === 0) {
    throw new Error('widgets/Cargo.toml named no [[bin]] entries');
  }
  if (!bins.includes('opscope')) {
    throw new Error('widgets/Cargo.toml has no opscope binary; the launcher cannot launch');
  }
  return bins;
}

function linuxRuntime() {
  let glibc;
  try {
    const report = process.report && process.report.getReport && process.report.getReport();
    if (report && report.header && report.header.glibcVersionRuntime) {
      glibc = report.header.glibcVersionRuntime;
    }
  } catch (_) {
    // getReport can throw in some embeddings. Fall through.
  }
  if (glibc) {
    return { libc: 'glibc', glibc };
  }
  try {
    if (
      fs.existsSync('/lib/ld-musl-x86_64.so.1') ||
      fs.existsSync('/lib/ld-musl-aarch64.so.1')
    ) {
      return { libc: 'musl' };
    }
  } catch (_) {
    // A filesystem we cannot read is not evidence of musl.
  }
  // Linux and nothing said musl: the release target is gnu, so this
  // is the match that target is for. A missing version is not proof
  // of an old one — refuse only when we can read it and it is old.
  return { libc: 'glibc' };
}

function detectLibc() {
  if (process.platform !== 'linux') return undefined;
  return linuxRuntime().libc;
}

function host() {
  if (process.platform !== 'linux') {
    return { os: process.platform, cpu: process.arch };
  }
  const linux = linuxRuntime();
  return {
    os: process.platform,
    cpu: process.arch,
    libc: linux.libc,
    glibc: linux.glibc,
  };
}

function describeHost(h) {
  const who = h || host();
  if (glibcTooOld(who)) {
    return `${who.os}-${who.cpu} (glibc ${who.glibc})`;
  }
  const libc = who.libc ? ` (${who.libc})` : '';
  return `${who.os}-${who.cpu}${libc}`;
}

function currentPlatform(h) {
  const who = h || host();
  if (glibcTooOld(who)) return null;
  return (
    PLATFORMS.find(
      (p) =>
        p.os === who.os &&
        p.cpu === who.cpu &&
        (p.libc === undefined || p.libc === who.libc),
    ) || null
  );
}

function publishedPlatforms() {
  return PLATFORMS.map((p) => `  ${p.label}`).join('\n');
}

function unsupportedMessage(h) {
  const who = h || host();
  if (glibcTooOld(who)) {
    return [
      `${LAUNCHER} needs glibc ${MIN_GLIBC} or newer; this machine has ${who.glibc}.`,
      '',
      'The Linux binaries are built against glibc 2.35. Build from source',
      'on this machine, or run them where glibc is new enough.',
      '',
      'It publishes:',
      publishedPlatforms(),
      '',
      'Build from source: https://github.com/stealth-factory/opscope',
    ].join('\n');
  }
  return [
    `${LAUNCHER} has no binaries for ${describeHost(who)}.`,
    '',
    'It publishes:',
    publishedPlatforms(),
    '',
    'Windows, Alpine/musl, 32-bit and Linux arm64 are not published.',
    'Build from source: https://github.com/stealth-factory/opscope',
  ].join('\n');
}

function missingOptionalMessage(wanted) {
  return [
    `this machine is ${wanted.label}, which is published, but`,
    `${wanted.pkg} did not install.`,
    '',
    'npm skipped it — --omit=optional, an offline registry, or a',
    'network error. Retry without omitting optional dependencies,',
    `or install ${wanted.pkg} at this same version.`,
  ].join('\n');
}

function installedPackageDir(pkg) {
  try {
    return path.dirname(require.resolve(`${pkg}/package.json`));
  } catch (_) {
    return null;
  }
}

function resolveStart(h) {
  const wanted = currentPlatform(h);
  if (!wanted) {
    const err = new Error(unsupportedMessage(h));
    err.code = 'UNSUPPORTED_PLATFORM';
    throw err;
  }
  const dir = installedPackageDir(wanted.pkg);
  if (!dir) {
    const err = new Error(missingOptionalMessage(wanted));
    err.code = 'MISSING_OPTIONAL';
    throw err;
  }
  const start = path.join(dir, 'bin', LAUNCHER);
  if (!fs.existsSync(start)) {
    const err = new Error(
      `${wanted.pkg} is installed but bin/${LAUNCHER} is missing. Reinstall the package.`,
    );
    err.code = 'CORRUPT_OPTIONAL';
    throw err;
  }
  return start;
}

function requireInstalled(h) {
  resolveStart(h);
}

module.exports = {
  LAUNCHER,
  PLATFORMS,
  MIN_GLIBC,
  glibcAtLeast,
  binsFromManifest,
  detectLibc,
  host,
  describeHost,
  currentPlatform,
  publishedPlatforms,
  unsupportedMessage,
  missingOptionalMessage,
  installedPackageDir,
  resolveStart,
  requireInstalled,
};
