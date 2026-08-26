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

// Builds the four publishable packages from a release's tarballs.
//
// Nothing in npm/ is a platform package sitting in git. Those would
// have to be rewritten on every tag, and a hand-maintained copy is
// how the npm version and the GitHub release come to disagree. This
// script is what the publish job runs against the artefacts that job
// just attached to the release, so the two cannot name different
// binaries.

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const platform = require('./platform');

function versionFromCargo(repoRoot) {
  const toml = fs.readFileSync(path.join(repoRoot, 'Cargo.toml'), 'utf8');
  const m = toml.match(/^version = "([^"]+)"/m);
  if (!m) throw new Error('Cargo.toml has no version');
  return m[1];
}

function walkFiles(dir, acc) {
  acc = acc || [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walkFiles(full, acc);
    else acc.push(full);
  }
  return acc;
}

function findTarball(artifacts, rustTarget) {
  const files = walkFiles(artifacts).filter((f) => {
    const base = path.basename(f);
    return base.endsWith(`-${rustTarget}.tar.gz`) && !base.endsWith('.sha256');
  });
  if (files.length === 0) {
    throw new Error(
      `no tarball for ${rustTarget} under ${artifacts} — the publish job cannot invent a platform the build did not produce`,
    );
  }
  if (files.length > 1) {
    throw new Error(
      `several tarballs for ${rustTarget}: ${files.join(', ')}`,
    );
  }
  return files[0];
}

function extract(tarball, dest) {
  fs.mkdirSync(dest, { recursive: true });
  const r = spawnSync('tar', ['-xzf', tarball, '-C', dest], { encoding: 'utf8' });
  if (r.status !== 0) {
    throw new Error(`tar -xzf ${tarball} failed: ${r.stderr || r.stdout}`);
  }
}

function findBinDir(extracted, bins) {
  // The tarball wraps a directory named after itself. Look for the
  // launcher rather than assuming the wrapper's name, so a `dev`
  // artefact and a `v0.1.2` artefact unpack the same way.
  const files = walkFiles(extracted);
  const launcher = files.find((f) => path.basename(f) === platform.LAUNCHER);
  if (!launcher) {
    throw new Error(
      `extracted ${extracted} but there is no ${platform.LAUNCHER} binary in it`,
    );
  }
  const dir = path.dirname(launcher);
  const missing = bins.filter((b) => !fs.existsSync(path.join(dir, b)));
  if (missing.length) {
    throw new Error(
      `tarball is missing ${missing.join(', ')} — refusing to publish a subset the launcher cannot launch`,
    );
  }
  return dir;
}

function writeJson(file, obj) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(obj, null, 2)}\n`);
}

function copyFile(src, dest) {
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(src, dest);
}

function platformManifest(p, version) {
  const manifest = {
    name: p.pkg,
    version,
    description: `${p.label} binaries for ${platform.LAUNCHER}`,
    os: [p.os],
    cpu: [p.cpu],
    files: ['bin'],
    license: 'AGPL-3.0-or-later',
    repository: {
      type: 'git',
      url: 'git+https://github.com/stealth-factory/opscope.git',
    },
    publishConfig: { access: 'public' },
  };
  if (p.libc) manifest.libc = [p.libc];
  return manifest;
}

function launcherManifest(version) {
  const template = JSON.parse(
    fs.readFileSync(path.join(__dirname, 'package.json'), 'utf8'),
  );
  template.version = version;
  template.optionalDependencies = {};
  for (const p of platform.PLATFORMS) {
    template.optionalDependencies[p.pkg] = version;
  }
  // The source package.json names a test script so `npm test` in npm/
  // works. The published tarball does not include test.js, and a
  // script that cannot run is a lie on the registry page.
  template.scripts = { postinstall: 'node postinstall.js' };
  return template;
}

function pack(opts) {
  const repoRoot = opts.repoRoot;
  const version = opts.version;
  const artifacts = opts.artifacts;
  const out = opts.out;
  const cargo = versionFromCargo(repoRoot);
  if (version !== cargo) {
    throw new Error(
      `asked to pack ${version} but Cargo.toml says ${cargo} — the npm version is the release version, not a number this script invents`,
    );
  }

  const bins = platform.binsFromManifest(repoRoot);
  const license = path.join(repoRoot, 'LICENSE');
  if (!fs.existsSync(license)) {
    throw new Error('LICENSE is missing from the repo root');
  }

  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });

  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-pack-'));
  try {
    for (const p of platform.PLATFORMS) {
      const tarball = findTarball(artifacts, p.rust);
      const extracted = path.join(scratch, p.rust);
      extract(tarball, extracted);
      const src = findBinDir(extracted, bins);
      const dest = path.join(out, ...p.pkg.split('/'), 'bin');
      fs.mkdirSync(dest, { recursive: true });
      for (const b of bins) {
        const to = path.join(dest, b);
        fs.copyFileSync(path.join(src, b), to);
        fs.chmodSync(to, 0o755);
      }
      writeJson(path.join(out, ...p.pkg.split('/'), 'package.json'), platformManifest(p, version));
      copyFile(license, path.join(out, ...p.pkg.split('/'), 'LICENSE'));
    }

    const launcherDir = path.join(out, ...platform.LAUNCHER.split('/'));
    writeJson(path.join(launcherDir, 'package.json'), launcherManifest(version));
    copyFile(path.join(__dirname, 'platform.js'), path.join(launcherDir, 'platform.js'));
    copyFile(path.join(__dirname, 'postinstall.js'), path.join(launcherDir, 'postinstall.js'));
    copyFile(path.join(__dirname, 'bin/opscope'), path.join(launcherDir, 'bin/opscope'));
    fs.chmodSync(path.join(launcherDir, 'bin/opscope'), 0o755);
    copyFile(path.join(__dirname, 'README.md'), path.join(launcherDir, 'README.md'));
    copyFile(license, path.join(launcherDir, 'LICENSE'));
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }

  return {
    version,
    bins,
    launcher: path.join(out, ...platform.LAUNCHER.split('/')),
    platforms: platform.PLATFORMS.map((p) => path.join(out, ...p.pkg.split('/'))),
  };
}

function parseArgs(argv) {
  const opts = {
    repoRoot: path.resolve(__dirname, '..'),
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--version') opts.version = argv[++i];
    else if (a === '--artifacts') opts.artifacts = argv[++i];
    else if (a === '--out') opts.out = argv[++i];
    else if (a === '--repo') opts.repoRoot = argv[++i];
    else if (a === '--help' || a === '-h') {
      process.stdout.write(
        'pack.js --version X.Y.Z --artifacts DIR --out DIR\n' +
          'Build the four npm packages from a release\'s tarballs.\n',
      );
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${a}`);
    }
  }
  if (!opts.version) throw new Error('--version is required');
  if (!opts.artifacts) throw new Error('--artifacts is required');
  if (!opts.out) throw new Error('--out is required');
  opts.version = opts.version.replace(/^v/, '');
  opts.artifacts = path.resolve(opts.artifacts);
  opts.out = path.resolve(opts.out);
  opts.repoRoot = path.resolve(opts.repoRoot);
  return opts;
}

if (require.main === module) {
  try {
    const result = pack(parseArgs(process.argv.slice(2)));
    process.stdout.write(`packed ${result.version}: ${platform.LAUNCHER} + ${result.platforms.length} platforms\n`);
  } catch (err) {
    console.error(err.message);
    process.exit(1);
  }
}

module.exports = {
  pack,
  parseArgs,
  versionFromCargo,
  findTarball,
  platformManifest,
  launcherManifest,
};
