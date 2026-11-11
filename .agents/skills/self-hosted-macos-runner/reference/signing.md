# Signing on a self-hosted Mac

A hosted runner throws its keychain away with the VM. A self-hosted Mac keeps
everything, so each job builds its own keychain and removes it, even when the
job fails.

## The signing lock

All runner instances on the Mac run as one macOS user, so every signing job,
from every repo, shares one keychain search list, one login keychain and one
set of provisioning profiles. A `concurrency:` group does not help: it only
serialises runs within one repository. Two overlapping jobs can break each
other. One restores a search list without the other's keychain, or a cleanup
that revokes "our" development certificates revokes the one the other job is
still signing with.

So every signing job, with a `.p12` or an API key, holds a machine-wide lock
from before it touches the keychain until after its last cleanup step
(including any certificate revocation). The alternative is a separate macOS
user per runner instance, which costs a logged-in GUI session each.

```yaml
- name: Take signing lock
  run: |
    LOCK=/tmp/ci-signing.lock
    got=""
    for _ in $(seq 1 360); do   # wait up to 30 minutes
      if mkdir "$LOCK" 2>/dev/null; then got=1; break; fi
      sleep 5
    done
    # Never evict by age: a slow upload still holds the keychain. A lock can
    # only outlive its job if the runner itself died before the always()
    # release step, so a person clears it after checking nothing is signing.
    [ -n "$got" ] || {
      echo "::error::signing lock held by $(cat "$LOCK/owner" 2>/dev/null || echo unknown) for 30+ minutes." \
           "If that run has finished, remove $LOCK on the Mac and re-run."
      exit 1
    }
    echo "$GITHUB_RUN_ID-$GITHUB_JOB" > "$LOCK/owner"
    echo "CI_SIGNING_LOCK=$LOCK" >> "$GITHUB_ENV"

# … signing setup, archive, export, upload, cleanup …

- name: Release signing lock      # the LAST step of the job
  if: always()
  run: |
    if [ -n "${CI_SIGNING_LOCK:-}" ] &&
       [ "$(cat "$CI_SIGNING_LOCK/owner" 2>/dev/null)" = "$GITHUB_RUN_ID-$GITHUB_JOB" ]; then
      rm -rf "$CI_SIGNING_LOCK"
    fi
```

## Certificate (.p12) signing

Between the two lock steps:

```yaml
- name: Temporary keychain
  env:
    P12_BASE64: ${{ secrets.DIST_CERT_P12_BASE64 }}
    P12_PASSWORD: ${{ secrets.DIST_CERT_P12_PASSWORD }}
  run: |
    KEYCHAIN="$RUNNER_TEMP/ci-$GITHUB_RUN_ID.keychain-db"
    KEYCHAIN_PASSWORD=$(uuidgen)
    echo "CI_KEYCHAIN=$KEYCHAIN" >> "$GITHUB_ENV"
    # Remember the user's search list, one path per line, so cleanup can
    # put it back. Paths may contain spaces, so never word-split them.
    security list-keychains -d user | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//' \
      > "$RUNNER_TEMP/keychains.before"
    security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
    security set-keychain-settings -lut 21600 "$KEYCHAIN"
    security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
    echo "$P12_BASE64" | base64 --decode > "$RUNNER_TEMP/cert.p12"
    security import "$RUNNER_TEMP/cert.p12" -P "$P12_PASSWORD" -A -t cert -f pkcs12 -k "$KEYCHAIN"
    rm -f "$RUNNER_TEMP/cert.p12"
    security set-key-partition-list -S apple-tool:,apple: -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" > /dev/null
    set -- "$KEYCHAIN"
    while IFS= read -r k; do [ -n "$k" ] && set -- "$@" "$k"; done < "$RUNNER_TEMP/keychains.before"
    security list-keychains -d user -s "$@"

# … archive and export, passing OTHER_CODE_SIGN_FLAGS="--keychain $CI_KEYCHAIN" if needed …

- name: Remove keychain
  if: always()
  run: |
    if [ -f "$RUNNER_TEMP/keychains.before" ]; then
      set --
      while IFS= read -r k; do [ -n "$k" ] && set -- "$@" "$k"; done < "$RUNNER_TEMP/keychains.before"
      security list-keychains -d user -s "$@"
    fi
    [ -n "${CI_KEYCHAIN:-}" ] && security delete-keychain "$CI_KEYCHAIN" || true
```

## App Store Connect API key (automatic signing)

Also between the two lock steps:

```yaml
- name: API key
  env:
    ASC_KEY_P8: ${{ secrets.ASC_KEY_P8 }}
  run: |
    printf '%s' "$ASC_KEY_P8" > "$RUNNER_TEMP/AuthKey.p8"
    chmod 600 "$RUNNER_TEMP/AuthKey.p8"

# xcodebuild archive … -allowProvisioningUpdates \
#   -authenticationKeyPath "$RUNNER_TEMP/AuthKey.p8" \
#   -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID"

- name: Remove API key
  if: always()
  run: rm -f "$RUNNER_TEMP/AuthKey.p8"
```

Automatic signing can create and download certificates and profiles into the
runner user's login keychain and
`~/Library/Developer/Xcode/UserData/Provisioning Profiles`. Those are shared
by every job on the Mac. Inside the lock, take a baseline of both when the
lock is acquired; at cleanup, delete the profiles and keychain identities that
are not in the baseline, and revoke only certificates this job created. Never
revoke every matching certificate in the keychain.

Every cleanup step that touches shared state (revoking, deleting identities
or profiles, restoring the search list) runs only when this job holds the
lock. A job whose lock wait timed out still runs its `if: always()` steps, and
without this guard it would clean up the job that does hold the lock:

```yaml
- name: Revoke this job's certificates
  if: always() && env.CI_SIGNING_LOCK != ''
  run: …
```

`CI_SIGNING_LOCK` is only set after the lock is taken, so the condition is
false for a job that never got it.

The lock is per Mac. It does not cover a certificate prune run from another
machine or a hosted runner (for example a scheduled job that revokes old
certificates for the Apple team). Run a prune only while no signing job is
running anywhere for that team, and hold new ones until it finishes.

On a Mac that also serves as someone's workstation, run the runner as its own
macOS user so these stay out of the person's account.
