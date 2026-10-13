'use strict';

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { binsFromManifest, MIN_GLIBC } = require('../npm/platform');
const { stableVersion } = require('../homebrew/formula');

function stage(version, binaries, destination, repo = path.resolve(__dirname, '..')) {
  stableVersion(version);
  const bins = binsFromManifest(repo);
  // Reject foreign architectures, scripts, symlinks and missing widgets before
  // creating a package whose metadata promises Linux amd64.
  for (const bin of bins) {
    const file = path.join(binaries, bin);
    const stat = fs.lstatSync(file);
    const bytes = fs.readFileSync(file);
    if (!stat.isFile() || !(stat.mode & 0o111) || bytes.length < 20 ||
        bytes.subarray(0, 4).toString('hex') !== '7f454c46' ||
        bytes[4] !== 2 || bytes[5] !== 1 || bytes.readUInt16LE(18) !== 62) {
      throw new Error(`${bin} must be an executable ELF64 little-endian amd64 file`);
    }
  }
  fs.mkdirSync(destination); // Never reuse a tree with stale widget files.
  const put = (name, contents, mode = 0o644) => {
    const file = path.join(destination, name);
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o755 });
    fs.writeFileSync(file, contents, { mode });
  };
  for (const bin of bins) put(`usr/libexec/opscope/${bin}`, fs.readFileSync(path.join(binaries, bin)), 0o755);
  put('usr/bin/opscope', '#!/bin/sh\nexec /usr/libexec/opscope/opscope "$@"\n', 0o755);
  for (const name of ['README.md', 'config.example.json']) {
    put(`usr/share/doc/opscope/${name}`, fs.readFileSync(path.join(repo, name)));
  }
  put('usr/share/doc/opscope/copyright', fs.readFileSync(path.join(repo, 'LICENSE')));
  put('usr/share/doc/opscope/binaries', bins.join('\n') + '\n');
  const files = [];
  function walk(dir) {
    for (const entry of fs.readdirSync(path.join(destination, dir), { withFileTypes: true })) {
      const name = path.posix.join(dir, entry.name);
      if (entry.isDirectory()) walk(name); else files.push(name);
    }
  }
  walk('usr');
  const size = Math.ceil(files.reduce((n, file) => n + fs.statSync(path.join(destination, file)).size, 0) / 1024);
  put('DEBIAN/control', `Package: opscope\nVersion: ${version}\nSection: utils\nPriority: optional\nArchitecture: amd64\nMaintainer: William Li <email@wiiiimm.codes>\nDepends: libc6 (>= ${MIN_GLIBC}), libgcc-s1\nInstalled-Size: ${size}\nHomepage: https://github.com/stealth-factory/opscope\nDescription: terminal widgets showing real operational data\n Sixteen widgets and their launcher, with private sibling binaries.\n No Node or Rust runtime is required.\n`);
  put('DEBIAN/md5sums', files.sort().map(file =>
    `${crypto.createHash('md5').update(fs.readFileSync(path.join(destination, file))).digest('hex')}  ${file}\n`).join(''));
  return bins;
}

function build(version, binaries, output) {
  stableVersion(version);
  if (process.platform !== 'linux') throw new Error('Build Debian packages on Linux');
  const root = path.resolve(__dirname, '..');
  const manifest = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)[1];
  if (version !== manifest) throw new Error('Package version must match Cargo.toml');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'opscope-deb-'));
  try {
    const tree = path.join(temp, 'root');
    const bins = stage(version, binaries, tree);
    let stamp;
    for (const bin of bins) {
      const reported = execFileSync(path.join(path.resolve(binaries), bin), ['--version'], { encoding: 'utf8' }).trim();
      const suffix = reported.slice(bin.length);
      if (!reported.startsWith(`${bin} ${version} (`) || /unknown|dirty/.test(reported) || (stamp && stamp !== suffix)) {
        throw new Error(`Inconsistent release stamp: ${reported}`);
      }
      stamp = suffix;
    }
    const epoch = execFileSync('git', ['show', '-s', '--format=%ct', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
    fs.mkdirSync(output, { recursive: true });
    const name = `opscope_${version}_amd64.deb`;
    const file = path.resolve(output, name);
    if (fs.existsSync(file)) throw new Error(`Refusing to overwrite ${file}`);
    execFileSync('dpkg-deb', ['--root-owner-group', '-Zxz', '--build', tree, file], {
      stdio: 'inherit', env: { ...process.env, SOURCE_DATE_EPOCH: epoch },
    });
    const hash = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    fs.writeFileSync(`${file}.sha256`, `${hash}  ${name}\n`);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
}

if (require.main === module) {
  if (process.argv.length !== 5) throw new Error('Usage: node debian/package.js X.Y.Z BINARY_DIR OUTPUT_DIR');
  build(...process.argv.slice(2));
}
module.exports = { stage, build };
