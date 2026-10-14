'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { stage } = require('./package');
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-deb-test-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'widgets'));
  fs.mkdirSync(path.join(root, 'bin'));
  fs.writeFileSync(path.join(root, 'widgets/Cargo.toml'), ['opscope', 'link', 'new-widget'].map(name => `[[bin]]\nname = "${name}"\n`).join(''));
  for (const name of ['README.md', 'LICENSE', 'config.example.json']) fs.writeFileSync(path.join(root, name), 'fixture');
  for (const name of ['opscope', 'link', 'new-widget']) {
    const elf = Buffer.alloc(64); elf.write('7f454c460201', 'hex'); elf.writeUInt16LE(62, 18);
    fs.writeFileSync(path.join(root, 'bin', name), elf, { mode: 0o755 });
  }
  return root;
}
test('manifest additions ship privately; only exec wrapper is on PATH', t => {
  const root = fixture(t);
  const out = path.join(root, 'out');
  stage('1.2.3', path.join(root, 'bin'), out, root);
  assert.deepEqual(fs.readdirSync(path.join(out, 'usr/bin')), ['opscope']);
  assert.deepEqual(fs.readdirSync(path.join(out, 'usr/libexec/opscope')), ['link', 'new-widget', 'opscope']);
  assert.equal(fs.readFileSync(path.join(out, 'usr/bin/opscope'), 'utf8'), '#!/bin/sh\nexec /usr/libexec/opscope/opscope "$@"\n');
  assert.match(fs.readFileSync(path.join(out, 'DEBIAN/control'), 'utf8'), /Architecture: amd64\n[\s\S]*Depends: libc6 \(>= 2.35\), libgcc-s1/);
  assert.throws(() => stage('1.2.3', path.join(root, 'bin'), out, root), /EEXIST/);
});
test('reject missing, symlinked and foreign binaries and invalid versions', t => {
  const root = fixture(t); const bin = path.join(root, 'bin'); const out = path.join(root, 'out');
  for (const v of ['dev', 'v1.2.3', '1.2.3\n', '1.2.3-rc.1']) assert.throws(() => stage(v, bin, out, root), /stable release/);
  fs.unlinkSync(path.join(bin, 'link'));
  assert.throws(() => stage('1.2.3', bin, out, root), /ENOENT/);
  fs.symlinkSync('opscope', path.join(bin, 'link'));
  assert.throws(() => stage('1.2.3', bin, out, root), /ELF64/);
  fs.unlinkSync(path.join(bin, 'link'));
  fs.writeFileSync(path.join(bin, 'link'), '#!/bin/sh\n', { mode: 0o755 });
  assert.throws(() => stage('1.2.3', bin, out, root), /ELF64/);
});

const { execFileSync } = require('node:child_process');
const { build } = require('./package');
const { binsFromManifest } = require('../npm/platform');
test('build refuses dirty, unknown, mixed and wrong-version release stamps before packaging',
  { skip: process.platform !== 'linux' }, t => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-stamp-test-'));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const repo = path.resolve(__dirname, '..');
    const version = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)[1];
    const source = path.join(root, 'stamp.c');
    fs.writeFileSync(source, `#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
  const char *name = strrchr(argv[0], '/'); name = name ? name + 1 : argv[0];
  const char *scenario = getenv("OPSCOPE_TEST_STAMP");
  const char *version = "${version}", *commit = "abcdef0", *date = "2026-10-13";
  if (!strcmp(scenario, "dirty")) commit = "abcdef0-dirty";
  if (!strcmp(scenario, "unknown")) commit = "unknown";
  if (!strcmp(scenario, "mixed") && !strcmp(name, "opscope")) commit = "fedcba0";
  if (!strcmp(scenario, "date") && !strcmp(name, "opscope")) date = "2026-10-12";
  if (!strcmp(scenario, "version")) version = "0.0.0";
  printf("%s %s (%s, %s)\\n", name, version, commit, date);
  return 0;
}
`);
    const executable = path.join(root, 'fixture');
    execFileSync('cc', [source, '-o', executable]);
    const binaries = path.join(root, 'bin'); fs.mkdirSync(binaries);
    for (const bin of binsFromManifest(repo)) fs.copyFileSync(executable, path.join(binaries, bin));
    const previous = process.env.OPSCOPE_TEST_STAMP;
    try {
      for (const scenario of ['dirty', 'unknown', 'mixed', 'date', 'version']) {
        process.env.OPSCOPE_TEST_STAMP = scenario;
        const out = path.join(root, scenario);
        assert.throws(() => build(version, binaries, out), /Inconsistent release stamp/);
        assert.equal(fs.existsSync(out), false);
      }
    } finally {
      if (previous === undefined) delete process.env.OPSCOPE_TEST_STAMP;
      else process.env.OPSCOPE_TEST_STAMP = previous;
    }
  });

test('APT notification carries only the exact allowlisted release, never credentials', () => {
  const { payload, notify } = require('./notify-apt');
  const env = { GITHUB_REPOSITORY: 'stealth-factory/opscope', GITHUB_REF_NAME: 'v1.2.3', GITHUB_SHA: 'a'.repeat(40), GH_TOKEN: 'non-secret-fixture' };
  let call;
  notify(env, (...args) => { call = args; });
  assert.deepEqual(JSON.parse(call[2].input), payload(env.GITHUB_REPOSITORY, env.GITHUB_REF_NAME, env.GITHUB_SHA));
  assert.equal(call[1][3], 'repos/stealth-factory/apt-repo/dispatches');
  assert.equal(call[2].input.includes(env.GH_TOKEN), false);
  for (const tag of ['dev', '1.2.3', 'v1.2.3-rc.1', 'v1.2.3\n']) assert.throws(() => payload(env.GITHUB_REPOSITORY, tag, env.GITHUB_SHA));
  assert.throws(() => payload('another/repo', 'v1.2.3', env.GITHUB_SHA));
  assert.throws(() => payload(env.GITHUB_REPOSITORY, 'v1.2.3', 'main'));
  assert.throws(() => notify({ ...env, GH_TOKEN: '' }), /unavailable/);
});
