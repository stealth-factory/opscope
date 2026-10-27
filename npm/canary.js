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
// The version a canary build of main is published under.
//
// A canary is not a release, and nothing here edits Cargo.toml: the
// manifest still names the last release, as it does everywhere on main.
// The canary takes the next patch after it with a prerelease suffix, so
// it sorts above the release it was built on and below the next one
// whatever that turns out to be. A prerelease is also something npm will
// only publish under an explicit --tag, which is what keeps it off
// `latest`.
//
//   2.2.0 on main, committed 2026-10-27 04:07:00 UTC at 1a2b3c4...
//   -> 2.2.1-canary.20261027040700.g1a2b3c4
//
// The timestamp is the commit's, not the build's, so two builds of one
// commit agree on its name and a re-run finds it already published. It
// comes first so canaries sort in the order they landed. The sha is
// prefixed with g, as git describe does, because seven hex digits can be
// all digits with a leading zero, which semver forbids.

const { execFileSync } = require('child_process');
const path = require('path');

function utcStamp(epochSeconds) {
  const d = new Date(epochSeconds * 1000);
  if (!Number.isFinite(epochSeconds) || Number.isNaN(d.getTime())) {
    throw new Error(`not a commit time: ${epochSeconds}`);
  }
  const pad = (n) => String(n).padStart(2, '0');
  return (
    `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}` +
    `${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}`
  );
}

function canaryVersion(release, epochSeconds, sha) {
  const m = String(release).match(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/);
  if (!m) throw new Error(`Cargo.toml version is not x.y.z: ${release}`);
  if (!/^[0-9a-f]{40}$/.test(String(sha))) {
    throw new Error(`not a full commit sha: ${sha}`);
  }
  const patch = Number(m[3]) + 1;
  return `${m[1]}.${m[2]}.${patch}-canary.${utcStamp(epochSeconds)}.g${sha.slice(0, 7)}`;
}

// Whether `version` is a canary of the release `release` names. pack.js
// uses this to accept a canary version that is not the manifest's own.
function isCanaryOf(version, release) {
  const m = String(release).match(/^(\d+)\.(\d+)\.(\d+)$/);
  if (!m) return false;
  const base = `${m[1]}.${m[2]}.${Number(m[3]) + 1}`;
  return new RegExp(
    `^${base.replace(/\./g, '\\.')}-canary\\.\\d{14}\\.g[0-9a-f]{7}$`,
  ).test(String(version));
}

function fromCheckout(repoRoot) {
  const { versionFromCargo } = require('./pack');
  const [time, sha] = execFileSync('git', ['log', '-1', '--format=%ct %H'], {
    cwd: repoRoot,
    encoding: 'utf8',
  })
    .trim()
    .split(' ');
  return canaryVersion(versionFromCargo(repoRoot), Number(time), sha);
}

if (require.main === module) {
  try {
    process.stdout.write(`${fromCheckout(path.resolve(__dirname, '..'))}\n`);
  } catch (err) {
    console.error(err.message);
    process.exit(1);
  }
}

module.exports = { canaryVersion, isCanaryOf, utcStamp, fromCheckout };
