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

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const platform = require('./platform');
const packer = require('./pack.js');

const repoRoot = path.resolve(__dirname, '..');

function rustTargetsFromReleaseYml() {
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  return [...yml.matchAll(/^\s+- target: ([a-z0-9_-]+)$/gm)].map((m) => m[1]);
}

function makeTarball(dir, rustTarget, tag, bins) {
  const name = `opscope-${tag}-${rustTarget}`;
  const root = path.join(dir, name);
  fs.mkdirSync(root, { recursive: true });
  for (const b of bins) {
    const file = path.join(root, b);
    fs.writeFileSync(file, `#!/bin/sh\necho ${b} ${tag}\n`);
    fs.chmodSync(file, 0o755);
  }
  fs.writeFileSync(path.join(root, 'README.md'), 'not a binary');
  const r = spawnSync('tar', ['-czf', `${name}.tar.gz`, name], {
    cwd: dir,
    encoding: 'utf8',
  });
  assert.equal(r.status, 0, r.stderr);
}

function scratch() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-npm-'));
}

test('every release target has an npm platform, and no extra ones', () => {
  const rust = rustTargetsFromReleaseYml().sort();
  const npm = platform.PLATFORMS.map((p) => p.rust).sort();
  assert.deepEqual(
    npm,
    rust,
    'platform.js and release.yml have drifted — a new target without a package would publish a tarball nobody can npx',
  );
});

test('the packer takes every [[bin]], including opscope', () => {
  const bins = platform.binsFromManifest(repoRoot);
  assert.ok(bins.includes('opscope'));
  assert.ok(!bins.includes('config'));
  assert.equal(bins.length, 16);
  assert.deepEqual(bins, [...bins].sort());
});

test('the launcher exposes one bin name, not fifteen', () => {
  const manifest = require('./package.json');
  assert.deepEqual(Object.keys(manifest.bin), ['opscope']);
});

test('this machine, on Linux glibc x64, is the linux-x64 package', () => {
  const wanted = platform.currentPlatform({
    os: 'linux',
    cpu: 'x64',
    libc: 'glibc',
  });
  assert.equal(wanted.pkg, 'opscope-linux-x64');
});

test('Windows, musl and 32-bit match nothing', () => {
  assert.equal(platform.currentPlatform({ os: 'win32', cpu: 'x64' }), null);
  assert.equal(
    platform.currentPlatform({ os: 'linux', cpu: 'x64', libc: 'musl' }),
    null,
  );
  assert.equal(
    platform.currentPlatform({ os: 'linux', cpu: 'ia32', libc: 'glibc' }),
    null,
  );
  assert.equal(
    platform.currentPlatform({ os: 'linux', cpu: 'arm64', libc: 'glibc' }),
    null,
  );
});

test('glibc older than the build baseline matches nothing', () => {
  assert.equal(platform.glibcAtLeast('2.31', platform.MIN_GLIBC), false);
  assert.equal(platform.glibcAtLeast('2.35', platform.MIN_GLIBC), true);
  assert.equal(platform.glibcAtLeast('2.39', platform.MIN_GLIBC), true);
  assert.equal(platform.glibcAtLeast('3.0', platform.MIN_GLIBC), true);
  assert.equal(
    platform.currentPlatform({
      os: 'linux',
      cpu: 'x64',
      libc: 'glibc',
      glibc: '2.31',
    }),
    null,
  );
  assert.equal(
    platform.currentPlatform({
      os: 'linux',
      cpu: 'x64',
      libc: 'glibc',
      glibc: '2.35',
    }).pkg,
    'opscope-linux-x64',
  );
  const msg = platform.unsupportedMessage({
    os: 'linux',
    cpu: 'x64',
    libc: 'glibc',
    glibc: '2.31',
  });
  assert.match(msg, /glibc 2\.35/);
  assert.match(msg, /2\.31/);
});

test('the unsupported sentence names every published platform', () => {
  const msg = platform.unsupportedMessage({ os: 'win32', cpu: 'x64' });
  assert.match(msg, /win32-x64/);
  for (const p of platform.PLATFORMS) {
    assert.match(msg, new RegExp(p.label.replace(/[()]/g, '\\$&')));
  }
});

test('pack.js stamps one version onto all four packages', () => {
  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const bins = platform.binsFromManifest(repoRoot);
    const artifacts = path.join(dir, 'artifacts');
    fs.mkdirSync(artifacts);
    for (const p of platform.PLATFORMS) {
      makeTarball(artifacts, p.rust, `v${version}`, bins);
    }
    const out = path.join(dir, 'out');
    const result = packer.pack({
      repoRoot,
      version,
      artifacts,
      out,
    });
    assert.equal(result.version, version);
    assert.equal(result.platforms.length, 3);

    const launcher = JSON.parse(
      fs.readFileSync(path.join(result.launcher, 'package.json'), 'utf8'),
    );
    assert.equal(launcher.name, platform.LAUNCHER);
    assert.equal(launcher.version, version);
    assert.deepEqual(Object.keys(launcher.bin), ['opscope']);
    assert.deepEqual(launcher.scripts, { postinstall: 'node postinstall.js' });
    for (const p of platform.PLATFORMS) {
      assert.equal(launcher.optionalDependencies[p.pkg], version);
    }
    assert.ok(fs.existsSync(path.join(result.launcher, 'bin/opscope')));
    assert.ok(fs.existsSync(path.join(result.launcher, 'postinstall.js')));
    assert.ok(fs.existsSync(path.join(result.launcher, 'LICENSE')));

    for (let i = 0; i < platform.PLATFORMS.length; i++) {
      const p = platform.PLATFORMS[i];
      const manifest = JSON.parse(
        fs.readFileSync(path.join(result.platforms[i], 'package.json'), 'utf8'),
      );
      assert.equal(manifest.name, p.pkg);
      assert.equal(manifest.version, version);
      assert.deepEqual(manifest.os, [p.os]);
      assert.deepEqual(manifest.cpu, [p.cpu]);
      if (p.libc) assert.deepEqual(manifest.libc, [p.libc]);
      else assert.equal(manifest.libc, undefined);
      for (const b of bins) {
        const file = path.join(result.platforms[i], 'bin', b);
        assert.ok(fs.existsSync(file), `${p.pkg} missing ${b}`);
        const mode = fs.statSync(file).mode;
        assert.ok(mode & 0o100, `${b} is not executable`);
      }
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('pack.js refuses a version that is not the manifest', () => {
  assert.throws(
    () =>
      packer.pack({
        repoRoot,
        version: '9.9.9',
        artifacts: repoRoot,
        out: path.join(os.tmpdir(), 'nope'),
      }),
    /Cargo\.toml says/,
  );
});

test('pack.js refuses a tarball stamped with a different version', () => {
  // The rust-target suffix used to be enough. A leftover from
  // another tag would then publish those binaries under this
  // version, and the Cargo.toml check would not see it.
  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    // A leftover must differ from Cargo.toml, not just look like
    // another version today. 9.9.9 would stop being a mismatch
    // the day the manifest is that number.
    const leftover = version === '0.0.0' ? '0.0.1' : '0.0.0';
    const bins = platform.binsFromManifest(repoRoot);
    const artifacts = path.join(dir, 'artifacts');
    fs.mkdirSync(artifacts);
    for (const p of platform.PLATFORMS) {
      makeTarball(artifacts, p.rust, `v${leftover}`, bins);
    }
    assert.throws(
      () =>
        packer.pack({
          repoRoot,
          version,
          artifacts,
          out: path.join(dir, 'out'),
        }),
      /no tarball/,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('pack.js refuses to publish when a platform tarball is missing', () => {
  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const bins = platform.binsFromManifest(repoRoot);
    const artifacts = path.join(dir, 'artifacts');
    fs.mkdirSync(artifacts);
    makeTarball(artifacts, platform.PLATFORMS[0].rust, `v${version}`, bins);
    assert.throws(
      () =>
        packer.pack({
          repoRoot,
          version,
          artifacts,
          out: path.join(dir, 'out'),
        }),
      /no tarball/,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('pack.js refuses a tarball that is missing a widget', () => {
  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const bins = platform.binsFromManifest(repoRoot).filter((b) => b !== 'matrix');
    const artifacts = path.join(dir, 'artifacts');
    fs.mkdirSync(artifacts);
    for (const p of platform.PLATFORMS) {
      makeTarball(artifacts, p.rust, `v${version}`, bins);
    }
    assert.throws(
      () =>
        packer.pack({
          repoRoot,
          version,
          artifacts,
          out: path.join(dir, 'out'),
        }),
      /missing matrix/,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the shim execs opscope from the matching platform package', () => {
  const wanted = platform.currentPlatform();
  // The fake layout below is for this host. A runner we do not publish
  // for would fail earlier, at currentPlatform(), and that path is
  // covered by the unsupported-sentence test.
  assert.ok(wanted, `this test host is ${platform.describeHost()}`);

  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const modules = path.join(dir, 'node_modules');
    const launcherDir = path.join(modules, 'opscope');
    const platDir = path.join(modules, wanted.pkg);
    fs.mkdirSync(path.join(launcherDir, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(platDir, 'bin'), { recursive: true });

    fs.copyFileSync(path.join(__dirname, 'platform.js'), path.join(launcherDir, 'platform.js'));
    fs.copyFileSync(path.join(__dirname, 'postinstall.js'), path.join(launcherDir, 'postinstall.js'));
    fs.copyFileSync(
      path.join(__dirname, 'bin/opscope'),
      path.join(launcherDir, 'bin/opscope'),
    );
    fs.chmodSync(path.join(launcherDir, 'bin/opscope'), 0o755);
    fs.writeFileSync(
      path.join(launcherDir, 'package.json'),
      JSON.stringify({ name: platform.LAUNCHER, version, bin: { opscope: 'bin/opscope' } }),
    );
    fs.writeFileSync(
      path.join(platDir, 'package.json'),
      JSON.stringify({ name: wanted.pkg, version }),
    );
    fs.writeFileSync(
      path.join(platDir, 'bin/opscope'),
      '#!/bin/sh\necho "opscope 0.1.2 (deadbeef, 2026-08-26)"\n',
    );
    fs.chmodSync(path.join(platDir, 'bin/opscope'), 0o755);

    const run = spawnSync('node', [path.join(launcherDir, 'bin/opscope'), '--version'], {
      encoding: 'utf8',
    });
    assert.equal(run.status, 0, run.stderr);
    assert.match(run.stdout, /opscope 0\.1\.2 \(deadbeef, 2026-08-26\)/);

    const post = spawnSync('node', [path.join(launcherDir, 'postinstall.js')], {
      encoding: 'utf8',
    });
    assert.equal(post.status, 0, post.stderr + post.stdout);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the shim reports 130 when opscope dies of SIGINT', () => {
  const wanted = platform.currentPlatform();
  assert.ok(wanted, `this test host is ${platform.describeHost()}`);

  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const modules = path.join(dir, 'node_modules');
    const launcherDir = path.join(modules, 'opscope');
    const platDir = path.join(modules, wanted.pkg);
    fs.mkdirSync(path.join(launcherDir, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(platDir, 'bin'), { recursive: true });

    fs.copyFileSync(path.join(__dirname, 'platform.js'), path.join(launcherDir, 'platform.js'));
    fs.copyFileSync(
      path.join(__dirname, 'bin/opscope'),
      path.join(launcherDir, 'bin/opscope'),
    );
    fs.chmodSync(path.join(launcherDir, 'bin/opscope'), 0o755);
    fs.writeFileSync(
      path.join(launcherDir, 'package.json'),
      JSON.stringify({ name: platform.LAUNCHER, version, bin: { opscope: 'bin/opscope' } }),
    );
    fs.writeFileSync(
      path.join(platDir, 'package.json'),
      JSON.stringify({ name: wanted.pkg, version }),
    );
    // The child dies of SIGINT. Re-raising that signal on the shim
    // used to re-enter the handler and exit 0.
    fs.writeFileSync(path.join(platDir, 'bin/opscope'), '#!/bin/sh\nkill -s INT $$\n');
    fs.chmodSync(path.join(platDir, 'bin/opscope'), 0o755);

    const run = spawnSync('node', [path.join(launcherDir, 'bin/opscope')], {
      encoding: 'utf8',
    });
    assert.equal(run.status, 130, `status=${run.status} stderr=${run.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the shim reports 129 when opscope dies of SIGHUP', () => {
  // SIGHUP is not one of the two signals the shim forwards. The
  // exit map still has to name it, or a child that dies of it
  // reports as a generic 1.
  const wanted = platform.currentPlatform();
  assert.ok(wanted, `this test host is ${platform.describeHost()}`);

  const dir = scratch();
  try {
    const version = packer.versionFromCargo(repoRoot);
    const modules = path.join(dir, 'node_modules');
    const launcherDir = path.join(modules, 'opscope');
    const platDir = path.join(modules, wanted.pkg);
    fs.mkdirSync(path.join(launcherDir, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(platDir, 'bin'), { recursive: true });

    fs.copyFileSync(path.join(__dirname, 'platform.js'), path.join(launcherDir, 'platform.js'));
    fs.copyFileSync(
      path.join(__dirname, 'bin/opscope'),
      path.join(launcherDir, 'bin/opscope'),
    );
    fs.chmodSync(path.join(launcherDir, 'bin/opscope'), 0o755);
    fs.writeFileSync(
      path.join(launcherDir, 'package.json'),
      JSON.stringify({ name: platform.LAUNCHER, version, bin: { opscope: 'bin/opscope' } }),
    );
    fs.writeFileSync(
      path.join(platDir, 'package.json'),
      JSON.stringify({ name: wanted.pkg, version }),
    );
    fs.writeFileSync(path.join(platDir, 'bin/opscope'), '#!/bin/sh\nkill -s HUP $$\n');
    fs.chmodSync(path.join(platDir, 'bin/opscope'), 0o755);

    const run = spawnSync('node', [path.join(launcherDir, 'bin/opscope')], {
      encoding: 'utf8',
    });
    assert.equal(run.status, 129, `status=${run.status} stderr=${run.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('postinstall fails on an unsupported platform with the sentence', () => {
  const dir = scratch();
  try {
    const launcherDir = path.join(dir, 'node_modules', 'opscope');
    fs.mkdirSync(launcherDir, { recursive: true });
    fs.copyFileSync(path.join(__dirname, 'platform.js'), path.join(launcherDir, 'platform.js'));
    fs.copyFileSync(path.join(__dirname, 'postinstall.js'), path.join(launcherDir, 'postinstall.js'));

    // Force the resolver onto Windows without moving this process there:
    // rewrite the copy to pin host() so postinstall sees win32.
    const stub = `
      const real = require('./platform.real.js');
      function host() { return { os: 'win32', cpu: 'x64' }; }
      module.exports = Object.assign({}, real, {
        host,
        currentPlatform: () => real.currentPlatform(host()),
        requireInstalled: () => real.requireInstalled(host()),
        resolveStart: () => real.resolveStart(host()),
      });
    `;
    fs.renameSync(path.join(launcherDir, 'platform.js'), path.join(launcherDir, 'platform.real.js'));
    fs.writeFileSync(path.join(launcherDir, 'platform.js'), stub);

    const post = spawnSync('node', [path.join(launcherDir, 'postinstall.js')], {
      encoding: 'utf8',
    });
    assert.notEqual(post.status, 0);
    assert.match(post.stderr, /win32-x64/);
    assert.match(post.stderr, /Linux x86_64 \(glibc\)/);
    assert.match(post.stderr, /macOS Apple Silicon/);
    assert.match(post.stderr, /macOS Intel/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('release.yml reads a packed package.json as a file, not as a module', () => {
  // `node -p 'require(process.argv[1])'` with a relative path looks in
  // node_modules. The first tagged npm publish died on that before it
  // reached the registry: Cannot find module
  // 'npm-dist/opscope-darwin-arm64/package.json'. The expression is
  // taken from the workflow so a rewrite that goes back to require()
  // fails here the same way it failed there.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  const m = yml.match(
    /name=\$\(node -p '([^']+)' "\$dir\/package\.json"\)/,
  );
  assert.ok(m, 'publish step no longer reads the package name with node -p');
  const dir = scratch();
  try {
    const rel = 'npm-dist/opscope-darwin-arm64';
    fs.mkdirSync(path.join(dir, rel), { recursive: true });
    fs.writeFileSync(
      path.join(dir, rel, 'package.json'),
      JSON.stringify({ name: 'opscope-darwin-arm64', version: '0.0.0' }),
    );
    const run = spawnSync('node', ['-p', m[1], `${rel}/package.json`], {
      cwd: dir,
      encoding: 'utf8',
    });
    assert.equal(run.status, 0, run.stderr);
    assert.equal(run.stdout.trim(), 'opscope-darwin-arm64');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('release.yml publishes a local directory, not a github shorthand', () => {
  // `npm publish npm-dist/opscope-darwin-arm64` is owner/repo, so npm
  // tried to git-clone github.com/npm-dist/opscope-darwin-arm64.git and
  // died 128. A leading ./ is a path. Taken from the workflow so a
  // rewrite that drops the prefix fails here the same way it failed there.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  assert.match(yml, /npm publish "\.\/\$dir"/);
  assert.equal(
    (yml.match(/npm publish "\$dir"/) || []).length,
    0,
    'npm publish "$dir" is github shorthand for npm-dist/<name>',
  );
});

test('nothing under npm/ still says the old project name', () => {
  // The leftover name is how npx would install a different package.
  // Built, not written, so this file is not itself a hit.
  const old = ['terminal', 'toys'].join('-');
  const hits = [];
  function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === 'node_modules') continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (fs.readFileSync(full, 'utf8').includes(old)) hits.push(path.relative(__dirname, full));
    }
  }
  walk(__dirname);
  assert.deepEqual(hits, [], `still named ${old}: ${hits.join(', ')}`);
});
