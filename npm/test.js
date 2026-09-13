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

// Every directory under widgets/src/widgets that holds a main.rs. Read off
// the filesystem so the expectation below never needs bumping by hand.
function widgetFolders() {
  const dir = path.join(repoRoot, 'widgets/src/widgets');
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && fs.existsSync(path.join(dir, e.name, 'main.rs')))
    .map((e) => e.name)
    .sort();
}

test('the packer takes every [[bin]], including opscope', () => {
  const bins = platform.binsFromManifest(repoRoot);
  assert.ok(bins.includes('opscope'));
  assert.ok(!bins.includes('config'));
  // Checked against the folders on disk, which is a *different* source from
  // the manifest the packer itself reads. A hardcoded number was a gate that
  // had to be bumped by hand; asserting the manifest against itself would be
  // no gate at all. This still fails on a widget folder with no [[bin]], and
  // on a [[bin]] with no folder, and it names which.
  const expected = [...widgetFolders(), 'opscope'].sort();
  assert.deepEqual(
    bins,
    expected,
    'widgets/Cargo.toml [[bin]] entries and widgets/src/widgets folders have drifted',
  );
  assert.deepEqual(bins, [...bins].sort());
});

test('the launcher exposes one bin name, not one per widget', () => {
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

// The job a line belongs to, so a check can say *where* as well as
// *what*. Jobs sit at two spaces and everything inside them is deeper,
// which is enough structure to slice on without a YAML parser.
function jobBlock(yml, name) {
  const lines = yml.split('\n');
  const start = lines.findIndex((l) => l === `  ${name}:`);
  assert.notEqual(start, -1, `release.yml has no ${name} job`);
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    if (/^ {2}[a-z]/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start, end).join('\n');
}

test('release.yml publishes untagged, so one write creates and advertises', () => {
  // The other shape was tried: `--tag next` here and a `promote` job that
  // moved `latest` once two clean runners had installed the version. It
  // bought a real thing - npm cannot serve a version for some minutes
  // after taking it, four measured on v0.16.0, and during those minutes
  // `latest` named a version npx resolved and then failed to fetch. It
  // was backed out because `npm dist-tag add` needs a stored credential
  // and trusted publishing cannot supply one, so promote never ran once
  // and four releases stopped at that fence. npm/cli#8547 is where it
  // comes back. Until then the propagation window is accepted, and this
  // test is what stops `--tag` being reintroduced by half.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  const publishes = yml.match(/npm publish "\.\/\$dir"[^\n]*/g) || [];
  assert.equal(publishes.length, 1, 'expected exactly one npm publish line');
  assert.doesNotMatch(
    publishes[0],
    /--tag\b/,
    'a --tag on publish leaves latest where it was, and nothing here moves it',
  );
});

test('nothing in release.yml moves a dist-tag or wants a token', () => {
  // The promote job is gone. A `dist-tag add` anywhere in this workflow
  // would be that job growing back a step at a time, and it cannot work:
  // the npm CLI performs the OIDC exchange inside `npm publish` and
  // nowhere else, so a dist-tag write wants NPM_TOKEN - a credential npm
  // now expires after 90 days, which is a release-day failure waiting
  // months to happen. Trusted publishing is the whole of the auth story.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  // A command, not the word. The publish step's note explains at length
  // what `npm dist-tag add` would have been for, and that prose is the
  // point of it - a reader who deletes the note is the reader this test
  // cannot help. What must not come back is a line that runs it.
  assert.equal(
    (yml.match(/^[^#\n]*\bnpm dist-tag add\b/gm) || []).length,
    0,
    'release.yml moves a dist-tag again; promote is growing back',
  );
  assert.equal(
    (yml.match(/^ {2}promote:$/m) || []).length,
    0,
    'release.yml has a promote job again',
  );
  assert.doesNotMatch(
    yml,
    /NPM_TOKEN/,
    'release.yml wants a stored npm credential again',
  );
});

test('the smoke jobs say they report rather than gate', () => {
  // They run after publish, and publish has already moved `latest`. So a
  // failure here says a release that has already happened is bad; it
  // stops nothing. A job whose failure prevents nothing must not read as
  // a gate - that misreading is how somebody later concludes a release
  // was held back when it was not.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  assert.match(
    yml,
    /NEITHER OF THESE IS A GATE/,
    'the smoke jobs no longer say they decide nothing',
  );
  // And the failure message may not describe the pipeline that was
  // removed: `latest` has moved by the time this job can fail.
  const smoke = jobBlock(yml, 'smoke-npm');
  assert.doesNotMatch(
    smoke,
    /latest has not been moved|under the next tag/,
    'smoke-npm still tells the reader latest was held back',
  );
});

test('release.yml runs the two smoke checks as jobs, not as two steps', () => {
  // They were two steps of one job, npx first, under `set -e`. The npx
  // retry loop can spend eight minutes failing, and the module check
  // then never ran - so the second question went unanswered, which on
  // screen is indistinguishable from an answer of no.
  const yml = fs.readFileSync(
    path.join(repoRoot, '.github/workflows/release.yml'),
    'utf8',
  );
  for (const job of ['smoke-npm', 'smoke-module']) {
    const block = jobBlock(yml, job);
    assert.match(block, /^ {4}needs: publish$/m, `${job} must follow publish`);
    assert.match(
      block,
      /macos-15/,
      `${job} must run on macOS as well as Linux`,
    );
  }
  assert.equal(
    (jobBlock(yml, 'smoke-npm').match(/luvus/g) || []).length,
    0,
    'the module check is back inside the job that gates promotion',
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
