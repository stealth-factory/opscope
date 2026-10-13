# Releasing

A release is one decision: **merge the release pull request.** Everything
either side of it is automatic.

## What happens, in order

```
merge a PR to main
        │
        ├─► ci.yml            tests and builds it
        │
        └─► release-pr.yml    works out the next version from the commit
                              subjects, writes the changelog entry, bumps
                              the manifests, and force-pushes the `release`
                              branch — opening or updating one standing PR
                              titled `release: vX.Y.Z`

           ... this repeats for every merge. Nothing is tagged or
               published. The PR accumulates until you decide.

merge the release PR
        │
        └─► tag-release.yml   sees the manifest names a version with no
                              tag, tags the merge commit, and starts
                              release.yml at that tag
                    │
                    └─► release.yml   three targets built and tested,
                                      checked for anything dynamically
                                      linked beyond the C runtime,
                                      tarballed with checksums, packed
                                      into four npm packages from those
                                      same tarballs, published to npm —
                                      which moves `latest` — then
                                      attached to a GitHub Release
                              │
                              ├─► smoke-npm     two clean runners, one
                              │                 Linux and one macOS,
                              │                 install this exact
                              │                 version from npm
                              │
                              ├─► homebrew      test the Mac formula,
                              │                 update the shared tap
                              │
                              └─► smoke-module  the Luvus module's
                                                build step fetches
                                                this release's assets

           These jobs run after publication. The smoke jobs report on
           the release rather than deciding it; a red one is a release
           to fix forward, not a release held back.
```

## What decides the version

The commit subjects since the last tag, read by `git-cliff` under the rules
in `cliff.toml`. Squash merge means **the pull request title is the commit
subject**, so in practice the version is decided by PR titles.

| Title starts with | Effect |
|---|---|
| `feat:` / `feat(scope):` | minor — `0.1.0` → `0.2.0` |
| `fix:`, `perf:`, `revert:` | patch — `0.1.0` → `0.1.1` |
| any type with `!:` | breaking — minor below 1.0.0, see below |
| `docs:`, `chore:`, `ci:`, `style:`, `test:`, `refactor:` | rides along with the next release; starts none by itself |
| `release:` | skipped entirely — these are the machinery's own commits |

**A batch of documentation and chores raises no release pull request.**
git-cliff would bump the patch for any commit at all, which means a README
edit could offer a version whose binaries are byte-for-byte the previous
one's. That is noise in a project that ships binaries, and worse noise in a
pooled model, where every release PR is something somebody has to form an
opinion about. Those commits are not dropped — they appear in the changelog
of whatever release comes next.

Below 1.0.0 a breaking change moves the minor rather than the major. That
is semver's own rule for `0.x`, and it is why this project can still make
breaking changes without claiming to be finished.

`pr-title.yml` enforces the format on every pull request, because a
badly-titled PR is not untidy — it is a change the next release will not
mention and may not count towards the version.

## Cutting one

1. Open the pull request titled `release: vX.Y.Z`.
2. Read the changelog entry. If something reads badly, fix the *commit
   subject* problem on main — do not commit to the `release` branch, which
   is force-pushed and will discard it.
3. Merge it.
4. Watch `release.yml`. Roughly five minutes for three platforms.

If no release PR exists, nothing releasable has landed since the last tag —
only documentation and chores, or nothing at all. The run log says which,
and gives the count it found.

## Things worth knowing

**The manifest moves ahead of the tag, deliberately.** The release PR bumps
`Cargo.toml` so the tag can point at a commit whose version already matches
it. `release.yml` checks the two agree and refuses to build otherwise —
which is only a meaningful check because nothing in the pipeline edits the
manifest on the way past. A build that rewrites its own source produces a
binary no checkout can reproduce.

**The release PR gets ordinary CI when it is opened by a user token.**
`release-pr.yml` uses `GH_TOKEN` (or `RELEASE_TOKEN`) for the checkout and
to open the PR. A classic PAT with `repo` is enough; a fine-grained one
needs Contents and Pull requests read/write. The workflow talks to the
pulls REST API rather than `gh pr create`, because that command's GraphQL
asks for org fields that `repo` does not cover. Opened with
`GITHUB_TOKEN` instead, the PR is `github-actions[bot]` and GitHub holds
`ci` for a maintainer click.
`release-pr.yml` still runs `cargo metadata --locked` on the bumped tree
before offering it — the one thing that step can get wrong is leaving the
lock disagreeing with the manifests. Editing an existing bot PR does not
change its author: close it so the next run opens a new one.

**`GH_TOKEN` / `RELEASE_TOKEN` are optional for release preparation.** Either
is a user PAT; if neither is set, the workflow falls back to `GITHUB_TOKEN`.
A tag pushed with `GITHUB_TOKEN` triggers no workflow,
so `tag-release.yml` starts the build with an explicit `workflow_dispatch`
at the tag — the documented exception to that rule — rather than relying
on the push to do it.

**npm publish uses trusted publishing, and nothing else.** The four
packages are unscoped (`opscope` and one optional dependency per
platform). Each is configured as a trusted publisher for this
repository's `release.yml`: no token, `id-token: write`, published
with provenance. There is no secret in the release pipeline and no
fallback to one — classic automation tokens were revoked in November
2025, and the granular tokens that replaced them expire after 90 days,
which is a release-day failure sitting quietly in the calendar. The
publish step deletes the `_authToken` line `setup-node` writes,
because any credential at all, an empty one included, makes npm skip
the OIDC exchange.

Publish fails if trusted publishing is not configured, and the GitHub
release is then not created.

**Publishing advertises, and there is a propagation window.** `npm
publish` moves the `latest` dist-tag in the same registry write that
creates the version, and npm cannot serve the version for some minutes
after taking it — four, measured on v0.16.0. For those minutes
`latest` names a version `npx opscope@latest` resolves and then fails
to fetch, which is the worst message npm has: the pointer npm itself
served is what named the version it says does not exist. Nothing
downstream can compensate, because by the time anything can look, the
pointer is already wrong.

Publish jobs for different tags share one concurrency group and
do not cancel each other, so two overlapping tagged runs cannot
interleave their writes. `tag-release.yml` dispatches this
workflow without waiting, which is how two tags overlap. An older
run that was already queued is refused if `latest` already names
a newer version.

That is a known cost, weighed and accepted, rather than something
nobody noticed. The careful shape was tried and backed out. It
published all four packages under `--tag next`, which nobody follows,
and moved `latest` from a separate `promote` job once `smoke-npm` had
installed that exact version on a clean Linux runner and a clean macOS
one — so during the propagation window `latest` still named the
previous release, which is what you want it to name. It worked on
paper and never once ran: `npm dist-tag add` needs a stored
credential, and trusted publishing cannot supply one, because the npm
CLI performs the OIDC exchange inside `npm publish` and nowhere else.
`npm stage` is not an alternative either — approving a staged publish
wants interactive proof of presence, which a workflow cannot give.

So the choice was a secret that expires every 90 days and fails on
some release day months from now, against a window that is brief,
self-correcting, and what most of the ecosystem lives with. Four
releases stopped at the promote fence before it was removed.

[npm/cli#8547](https://github.com/npm/cli/issues/8547) asks npm to
cover dist-tags with OIDC. The day it lands this can come back exactly
as it was: `--tag next` on the publish line, and a promote job after
`smoke-npm`.

**The smoke jobs report; they do not gate.** `smoke-npm` and
`smoke-module` both run after `publish`, and `publish` has already
moved `latest`. A failure in either fails the release run loudly, and
that is all it does — the version is out and `npx opscope` is already
pointing at it. Read a red smoke job as *this release is bad*, not as
*this release was stopped*. The fix is forward: cut the next version.
Do not unpublish.

`smoke-npm` retries for eight minutes against an observed four,
because a registry that has not finished propagating is not a release
that failed.

The four packages — the launcher and one optional dependency per
platform — are generated by `npm/pack.js` from the tarballs on the
release, not maintained in git. Their version is the tag. A Mac never
downloads the Linux binaries; an unsupported platform fails at install
with a sentence naming the three that exist.

**Every platform artefact contains every binary.** Each release tarball and
each platform-specific npm package carries `opscope` plus all sixteen widgets;
there are no per-widget downloads and no platform package may publish only the
widgets that happen to work there. `npm/pack.js` reads the authoritative
`[[bin]]` list from `widgets/Cargo.toml` and refuses a tarball missing any one
of them, so `npx opscope` cannot install a launcher whose menu names a binary
the package does not carry. A widget without a source on the current kernel
must still ship and explain that state on screen. The canonical source layout
and platform boundary live in the
[widget-creation wiki](../wiki/making-a-widget.md); the release workflows only
build and verify that contract rather than maintaining a second diagram.

**Both macOS architectures are built on one Apple Silicon runner.**
`macos-13` is retired, so a job asking for it queues forever; the Intel
images that replaced it are `-large` runners and bill even on a public
repository. Apple's toolchain cross-compiles `x86_64` natively at no cost.
What it cannot do is *run* the result, so that leg builds and packages but
does not execute its tests — the same sources are tested on the native arm64
leg in the same run.

## Homebrew

After `publish` has created the public GitHub release, the `homebrew` job
downloads both Mac archives and their checksum files. `homebrew/formula.js`
verifies the hashes and checks every binary against the tagged
`widgets/Cargo.toml`, then generates `Formula/opscope.rb`. The job installs
and tests that candidate on an Apple Silicon runner before updating the
shared [tap](https://github.com/stealth-factory/homebrew-tap).

The formula carries the native release binaries with no Node or Rust
dependency. All binaries live in `libexec`; only the `opscope` launcher is
started through an executable wrapper on PATH. A symlink is insufficient
on macOS: the launcher resolves widgets relative to the invoked executable
path, so it would search `bin` instead of `libexec`. Documentation and
example configuration live in
`share/opscope`. User configuration stays outside the Cellar, so upgrading
does not replace settings or tokens. Intel uses its separately checksummed
archive; Intel execution is not covered by the Apple Silicon smoke test.

Like gh-manager-cli's tap update, this needs **`GH_TOKEN` with Contents
read/write access to `stealth-factory/homebrew-tap`**, or `RELEASE_TOKEN`
with the same access. The default `GITHUB_TOKEN` cannot write a different
repository. An organisation secret must be made available to opscope;
otherwise set a repository secret. The job fails clearly if neither exists.

`homebrew/update-tap.sh` updates only `Formula/opscope.rb` through GitHub's
Contents API, preserving other applications' formulae. It compares the file
SHA on each write, retries a concurrent update, skips an identical formula,
and refuses a downgrade. A retry of the same version with different bytes
is an error. Tokens stay in the environment, never in clone URLs or files.

Merge the tap's initial formula before releasing this workflow. To generate
an initial formula or inspect one locally, use a checkout whose binary
manifest matches that release (normally the tag that carries these scripts):

```sh
gh release download v1.1.0 --repo stealth-factory/opscope \
  --pattern '*apple-darwin.tar.gz*' --dir homebrew-artifacts
node homebrew/formula.js 1.1.0 homebrew-artifacts /tmp/opscope.rb
node --test homebrew/test.js
```

A failed Homebrew job leaves the GitHub/npm release published and the tap
at its previous version. Fix the credential or packaging failure and
re-dispatch that tag's release workflow; published npm versions and the
GitHub release are already handled as retries. The release job cannot pick
up script fixes that exist only on main: those require a new release.

## Debian and the shared APT repository

On a `vX.Y.Z` tag, the Linux build runs `debian/package.js` against the same
manifest-declared binaries that go into the tarball. It verifies ELF amd64
headers and identical version/commit/date stamps, keeps all binaries under
`/usr/libexec/opscope`, and adds a single exec wrapper at `/usr/bin/opscope`.
`dpkg-deb --root-owner-group` produces `opscope_X.Y.Z_amd64.deb`; a SHA-256
file is generated alongside it. Ownership is root, permissions are fixed,
and the commit timestamp supplies `SOURCE_DATE_EPOCH`.

The control metadata declares amd64, `libc6 (>= 2.35)` and `libgcc-s1`.
The artifact upload and GitHub release asset list explicitly include both
`.deb` and `.deb.sha256`. The tagged `debian-lifecycle` job verifies their
checksum and tests Ubuntu 22.04, Debian 12, and dependency refusal on Debian 11
before `publish` can run. macOS skips packaging. Pull-request release builds
skip the tagged package step; ordinary CI separately exercises the packer with
the manifest version in a disposable test artifact, never a `dev` version.

After `publish` succeeds, the gated `apt-repository` job sends the exact source,
release tag and commit to the shared [APT repository](https://github.com/stealth-factory/apt-repo)
through `repository_dispatch`. It reuses `secrets.GH_TOKEN || secrets.RELEASE_TOKEN`,
the Homebrew bot credential already configured here. The endpoint needs Contents
write in the destination for a fine-grained token, not Actions write. No secret
travels in the event and no new credential or account grant is configured.

The receiver verifies the tag's commit, `.deb` checksum and metadata, retains
immutable versions, rejects stale updates, signs indexes, tests APT installation,
and publishes a complete Pages snapshot. A twice-weekly job refreshes metadata
for existing packages before its 14-day expiry; it never polls for new releases.
Enable `APT_PUBLISH_ENABLED` in both repositories only with activation approval.
Signing credentials and Pages activation are managed in that repository and
were not configured by these changes. Its operations guide documents the exact
approval requirements, scoped key trust, expiration, rotation, and recovery.

A failed APT import leaves GitHub/npm and the downloadable `.deb` available,
with the APT site on its previous snapshot. Fix the repository problem and
retry its workflow on main. If the package itself is wrong, cut a new Opscope
version; do not replace an existing version's bytes. A published old release
without Debian assets is not backfilled automatically. The existing release
retry behavior still skips an already published GitHub release.

See [Debian installation](debian.md) for direct package use. Public tagged
asset delivery and signed repository installation remain acceptance checks
for the first approved activation/release, not claims made by PR-only tests.

## If something goes wrong

**A tag exists but has no release.** The build failed after tagging. Read
`release.yml`'s log. If npm published nothing, delete the tag and re-push
it after the fix is on main. Re-dispatching the same tag checks out
that tag's tree, so a fix that only exists on main will not run. If any
of the four packages already landed on npm, that version is frozen —
cut the next one. Re-dispatching a moved tag after a different commit
will refuse to skip a package whose `gitHead` is not this run, and so
will a package whose `gitHead` is missing or unreadable.

**The release PR is not updating.** Check `release-pr.yml`'s last run. It
exits quietly when nothing releasable has landed, which looks identical to
being broken; the log says which.

**A version was cut by mistake.** Nothing is unpublishable, so do not delete
the release — cut the next one. Deleting a tag that people may already have
fetched trades a small mistake for a confusing one.

**npm publish failed and there is no GitHub release.** Look at all
four packages on npm (`opscope` and the three platform packages)
before retrying the tag — the four publishes are not atomic, so a
later package can fail after earlier ones have landed. If none of
them have this version, trusted publishing is likely not configured
and nothing was published on either side: fix the publisher, then
re-dispatch `release.yml` at that tag. A version already on npm from
this same commit — proven by a 40-hex `gitHead` that matches
`GITHUB_SHA` — is skipped rather than republished, so a retry after
npm succeeded and the GitHub step failed will finish the release. A
package already on npm from a different commit, or one whose
`gitHead` cannot be read, is an error: npm cannot replace it, and
skipping would mix two commits under one version.

`gh release create` deletes its own leftover draft if the upload fails;
if a draft is still there it blocks the retry — delete it and
re-dispatch. A release that already exists and is published is walked
past rather than failed on, so a re-dispatch reaches the jobs after it.

**`smoke-npm` went red.** `latest` already names this version and npm
could not install it from a clean runner within eight minutes, so
`npx opscope` is pointing at something that does not work. Nothing in
the pipeline can take that back. Cut the next version — which means
landing the `fix:` that makes a release worth cutting, since a version
whose binaries are byte-for-byte the last one's is the noise this
pipeline declines to make — and do not unpublish:
a version withdrawn from under somebody who installed it by number is
a worse problem than one that is merely broken.

**Re-dispatching an older tag.** Publish refuses to write if
`latest` already names a newer version on any of the four packages,
so a queued older run cannot roll `npx opscope` backwards.
Re-dispatch to finish the release that is newest. A re-dispatch of
a fully published older tag writes nothing and moves nothing.
A *partly* published older tag whose `latest` is still that older
version can still finish and keep `latest` there — cut the next
version instead.
