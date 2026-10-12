'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync, spawnSync } = require('node:child_process');
const { generateFormula, stableVersion, newer } = require('./formula');
const { binsFromManifest } = require('../npm/platform');

function fixture(t, missing, bins = ['opscope', 'link', 'future-widget']) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-brew-test-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'widgets'));
  fs.writeFileSync(path.join(root, 'widgets/Cargo.toml'),
    bins.map(bin => `[[bin]]\nname = "${bin}"\n`).join(''));
  for (const target of ['aarch64-apple-darwin', 'x86_64-apple-darwin']) {
    const name = `opscope-v1.2.3-${target}`;
    fs.mkdirSync(path.join(root, name));
    for (const bin of bins) {
      if (bin !== missing) fs.writeFileSync(path.join(root, name, bin), 'fixture');
    }
    const file = path.join(root, `${name}.tar.gz`);
    execFileSync('/usr/bin/tar', ['-czf', file, '-C', root, name]);
    const hash = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    fs.writeFileSync(`${file}.sha256`, `${hash}  ${name}.tar.gz\n`);
  }
  return root;
}

test('uses both verified Mac archives and includes a newly added widget', t => {
  const root = fixture(t);
  const formula = generateFormula('1.2.3', root, root);
  assert.match(formula, /on_arm do[\s\S]*aarch64-apple-darwin/);
  assert.match(formula, /on_intel do[\s\S]*x86_64-apple-darwin/);
  assert.match(formula, /    future-widget\n/);
  assert.match(formula, /bin.write_exec_script libexec\/"opscope"/);
  assert.doesNotMatch(formula, /bin.install BINARY_NAMES/);
});

function updateFixture(t) {
  const root = fixture(t, undefined, binsFromManifest(path.resolve(__dirname, '..')));
  const formula = generateFormula('1.2.3', root);
  const previous = path.join(root, 'current.rb');
  fs.writeFileSync(previous, formula);
  const gh = path.join(root, 'gh');
  fs.writeFileSync(gh, `#!${process.execPath}
const fs = require('node:fs');
const root = process.env.BREW_TEST_ROOT;
const args = process.argv.slice(2);
fs.appendFileSync(root + '/calls', args.includes('PUT') ? 'PUT\\n' : 'GET\\n');
if (!args.includes('PUT')) {
  console.log(JSON.stringify({sha: 'file-sha', content: fs.readFileSync(root + '/current.rb').toString('base64')}));
} else {
  const payload = JSON.parse(fs.readFileSync(args[args.indexOf('--input') + 1]));
  if (payload.sha !== 'file-sha' || payload.branch !== 'main') process.exit(2);
  fs.writeFileSync(root + '/payload.rb', Buffer.from(payload.content, 'base64'));
  if (process.env.BREW_TEST_CONFLICT) {
    fs.writeFileSync(root + '/current.rb', '# Release: 9.0.0\\n');
    process.exit(1);
  }
}
`);
  fs.chmodSync(gh, 0o755);
  return { root, formula, previous };
}

function runUpdater(root, conflict = false) {
  return spawnSync('/bin/bash', [path.join(__dirname, 'update-tap.sh'), '1.2.3', root], {
    encoding: 'utf8',
    env: { ...process.env, PATH: `${root}:${process.env.PATH}`, BREW_TEST_ROOT: root,
      BREW_TEST_CONFLICT: conflict ? '1' : '' },
  });
}

test('an identical retry performs no API write', t => {
  const { root } = updateFixture(t);
  const run = runUpdater(root);
  assert.equal(run.status, 0, run.stderr);
  assert.equal(fs.readFileSync(path.join(root, 'calls'), 'utf8'), 'GET\n');
});

test('updater changes only its formula using the current file SHA', t => {
  const { root, formula, previous } = updateFixture(t);
  fs.writeFileSync(previous, formula.replaceAll('1.2.3', '1.2.2'));
  const run = runUpdater(root);
  assert.equal(run.status, 0, run.stderr);
  assert.equal(fs.readFileSync(path.join(root, 'calls'), 'utf8'), 'GET\nPUT\n');
  assert.equal(fs.readFileSync(path.join(root, 'payload.rb'), 'utf8'), formula);
});

test('a concurrent newer release is reread and never overwritten', t => {
  const { root, formula, previous } = updateFixture(t);
  fs.writeFileSync(previous, formula.replaceAll('1.2.3', '1.2.2'));
  const run = runUpdater(root, true);
  assert.equal(run.status, 0, run.stderr);
  assert.equal(fs.readFileSync(path.join(root, 'calls'), 'utf8'), 'GET\nPUT\nGET\n');
  assert.equal(fs.readFileSync(previous, 'utf8'), '# Release: 9.0.0\n');
});

test('a same-version retry with different formula bytes fails before writing', t => {
  const { root, formula, previous } = updateFixture(t);
  fs.writeFileSync(previous, formula.replace('Terminal widgets', 'Modified widgets'));
  const run = runUpdater(root);
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /already has a different formula/);
  assert.equal(fs.readFileSync(path.join(root, 'calls'), 'utf8'), 'GET\n');
});

test('rejects tampered bytes even when the archive still lists correctly', t => {
  const root = fixture(t);
  fs.appendFileSync(path.join(root, 'opscope-v1.2.3-aarch64-apple-darwin.tar.gz'), 'tamper');
  assert.throws(() => generateFormula('1.2.3', root, root), /Checksum mismatch/);
});

test('rejects a checksummed archive missing a declared widget', t => {
  const root = fixture(t, 'future-widget');
  assert.throws(() => generateFormula('1.2.3', root, root), /missing binary future-widget/);
});

test('requires the Intel archive too', t => {
  const root = fixture(t);
  fs.unlinkSync(path.join(root, 'opscope-v1.2.3-x86_64-apple-darwin.tar.gz'));
  assert.throws(() => generateFormula('1.2.3', root, root), /ENOENT/);
});

test('stable numeric ordering protects against out-of-order releases', () => {
  assert.equal(newer('1.10.0', '1.9.9'), true);
  assert.equal(newer('2.0.0', '1.99.0'), true);
  assert.equal(newer('1.2.3', '1.2.3'), false);
  assert.equal(newer('1.2.2', '1.2.3'), false);
  for (const invalid of ['v1.2.3', '1.2.3-beta', '01.2.3', '1.2.3\n', '1.2']) {
    assert.throws(() => stableVersion(invalid));
  }
});

test('a stale invocation leaves no candidate formula to publish', t => {
  const root = fixture(t);
  const previous = path.join(root, 'previous.rb');
  const output = path.join(root, 'candidate.rb');
  fs.writeFileSync(previous, '# Release: 9.0.0\nclass Opscope < Formula\nend\n');
  const run = spawnSync(process.execPath,
    [path.join(__dirname, 'formula.js'), '1.2.3', root, output, previous], { encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.match(run.stdout, /refusing to downgrade/);
  assert.equal(fs.existsSync(output), false);
  assert.match(fs.readFileSync(previous, 'utf8'), /9.0.0/);
});
