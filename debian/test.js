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
