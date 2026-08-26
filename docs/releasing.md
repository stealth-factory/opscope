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
                                      same tarballs so
                                      `npx @stealth-factory/terminal-toys`
                                      is this version, then attached
                                      to a GitHub Release
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

**The release PR arrives with no CI, and that is expected.** It is opened by
`GITHUB_TOKEN`, and GitHub holds workflow runs on such pull requests for
manual approval. Rather than depend on someone approving them,
`release-pr.yml` runs `cargo metadata --locked` on the bumped tree itself —
the one thing that step can get wrong is leaving the lock disagreeing with
the manifests, and that is checked before the PR is offered.

**`RELEASE_TOKEN` is optional.** If set to a fine-grained PAT with Contents
and Pull requests read/write, the release PR is opened as that user and gets
ordinary CI. Nothing else depends on it: a tag pushed with `GITHUB_TOKEN`
triggers no workflow, so `tag-release.yml` starts the build with an explicit
`workflow_dispatch` at the tag — the documented exception to that rule —
rather than relying on the push to do it.

**npm publish uses trusted publishing, with `NPM_TOKEN` as a fallback.**
Create the `@stealth-factory` org on npm (the unscoped name `terminal-toys`
is a different package and cannot be this one). Then either configure each
of the four packages as a trusted publisher for this repository's
`release.yml` — no token; the job has `id-token: write` and publishes with
provenance — or store a granular access token with permission to publish
under that org as the `NPM_TOKEN` repository secret. Classic automation
tokens were revoked in November 2025 and will not work.

The job does not fail closed on an empty secret: that would block the
OIDC path. Publish itself fails if neither trusted publishing nor the
token is set, and the GitHub release is then not created.

The four packages — the launcher and one optional dependency per
platform — are generated by `npm/pack.js` from the tarballs on the
release, not maintained in git. Their version is the tag. A Mac never
downloads the Linux binaries; an unsupported platform fails at install
with a sentence naming the three that exist.

**Both macOS architectures are built on one Apple Silicon runner.**
`macos-13` is retired, so a job asking for it queues forever; the Intel
images that replaced it are `-large` runners and bill even on a public
repository. Apple's toolchain cross-compiles `x86_64` natively at no cost.
What it cannot do is *run* the result, so that leg builds and packages but
does not execute its tests — the same sources are tested on the native arm64
leg in the same run.

## If something goes wrong

**A tag exists but has no release.** The build failed after tagging. Read
`release.yml`'s log, fix it on main, then delete the tag and re-push it —
or re-run the dispatch at that tag once the fix is in.

**The release PR is not updating.** Check `release-pr.yml`'s last run. It
exits quietly when nothing releasable has landed, which looks identical to
being broken; the log says which.

**A version was cut by mistake.** Nothing is unpublishable, so do not delete
the release — cut the next one. Deleting a tag that people may already have
fetched trades a small mistake for a confusing one.

**npm publish failed and there is no GitHub release.** That is the
intended failure: trusted publishing is not configured and `NPM_TOKEN`
is missing, or the `@stealth-factory` org does not exist, and nothing
was published on either side. Fix the publisher or the secret, then
re-dispatch `release.yml` at that tag. A version already on npm is
skipped rather than republished, so a retry after npm succeeded and
the GitHub step failed will finish the release.
