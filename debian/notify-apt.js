'use strict';
const { execFileSync } = require('node:child_process');
const { stableVersion } = require('../homebrew/formula');

function payload(repository, tag, commit) {
  if (repository !== 'stealth-factory/opscope') throw new Error('Unexpected release source');
  if (typeof tag !== 'string' || !tag.startsWith('v')) throw new Error('Expected vX.Y.Z tag');
  stableVersion(tag.slice(1));
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error('Expected exact release commit SHA');
  return { event_type: 'opscope-release', client_payload: { repository, tag, commit } };
}

function notify(env = process.env, execute = execFileSync) {
  const body = payload(env.GITHUB_REPOSITORY, env.GITHUB_REF_NAME, env.GITHUB_SHA);
  if (!env.GH_TOKEN) throw new Error('Existing Homebrew bot token GH_TOKEN / RELEASE_TOKEN is unavailable');
  // Same bot secret as Homebrew, never the default per-repository token.
  // repository_dispatch needs Contents write, not Actions write.
  execute('gh', ['api', '--method', 'POST', 'repos/stealth-factory/apt-repo/dispatches', '--input', '-'],
    { input: JSON.stringify(body), stdio: ['pipe', 'inherit', 'inherit'], env });
}
if (require.main === module) notify();
module.exports = { payload, notify };
