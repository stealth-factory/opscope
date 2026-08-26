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
                                      tarballed with checksums, attached
                                      to a GitHub Release
```

## What decides the version

The commit subjects since the last tag, read by `git-cliff` under the rules
in `cliff.toml`. Squash merge means **the pull request title is the commit
subject**, so in practice the version is decided by PR titles.

| Title starts with | Effect |
|---|---|
| `feat:` / `feat(scope):` | minor — `0.1.0` → `0.2.0` |
| `fix:`, `perf:`, `refactor:` … | patch — `0.1.0` → `0.1.1` |
| `chore:`, `docs:`, `style:` … | appears in the changelog, moves nothing |
| `release:` | skipped entirely — these are the machinery's own commits |

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
only chores, or nothing at all.

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
