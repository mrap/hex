# Hex Versioning

## Source of Truth

`system/harness/Cargo.toml` (in hex-foundation) is the single source of truth for the hex version. Everything derives from this file:

- **Rust binary** — `build.rs` injects only the git SHA. `main.rs` reads `env!("CARGO_PKG_VERSION")` at compile time. The binary prints `hex 0.13.3 (abc1234)`.
- **git tag** — must match `v$(Cargo.toml version)`. Enforced by `hex release cut`, which bumps Cargo.toml and tags the merge commit in one ceremony.
- **hex VERSIONS file** — `HEX_FOUNDATION_VERSION` is pinned by `/hex-upgrade` from foundation's Cargo.toml at the pulled tag.
- **`.hex/version.txt`** — plain-text semver synced from foundation during `/hex-upgrade`; read by `extension-validate.py` for local tooling checks.
- **`hex version`** — reads the compiled-in `CARGO_PKG_VERSION` + `HEX_GIT_SHA`.
- **`hex --version`** — Clap built-in, reads `CARGO_PKG_VERSION`.

## Version Flow

```
system/harness/Cargo.toml (source of truth)
    ├── env!("CARGO_PKG_VERSION") → embedded in binary at compile time
    ├── git tag must match (enforced by hex release cut)
    └── hex VERSIONS pinned by /hex-upgrade
```

## Releasing a New Version

Releases follow **GitFlow**: feature branches merge to `develop`; a release is cut
from `develop`, merged `--no-ff` to `main`, tagged, and back-merged to `develop`.
Hotfixes cut from `main` directly.

**Releases are owned by the `oss-releaser` agent.** Pushing a `release/X.Y.Z`
(or `hotfix/X.Y.Z`) branch to origin IS the release request — the watcher picks
it up and finishes it. The releaser also owns pushing `develop`: contributors
merge to `develop` locally and STOP; develop-sync pushes it on the next watch
tick. Running `hex release cut` by hand is the exception, not the rule, and
requires a stated reason (watcher down, `--dry-run`, deliberate override).

The whole ceremony is one command (or one trigger):

```sh
hex release cut --level patch            # or minor | major, or --version X.Y.Z
hex release cut --hotfix                 # hotfix/X.Y.Z from main instead of develop
hex release cut --finish release/X.Y.Z   # complete a pre-existing release/hotfix branch
hex release cut --dry-run                # run the gate battery and stop
```

Both agent surfaces are handled by the `oss-releaser` worker, which spawns the
ceremony as a detached child:

- **Branch watch** (the default path) — a 5-minute cron `git ls-remote`s the
  `release/*` and `hotfix/*` heads of every watched releases.toml profile
  (`watch = true` + `repo_dir`) and runs `hex release cut --finish <branch>`
  for any head it has not seen before. Last-seen SHAs persist per
  (profile, branch) in the harness state db, so one observed head spawns at
  most one ceremony across ticks and restarts; while a repo's ceremony lock is
  held its poll is deferred entirely, so the watch never races a running cut.
- **`release.requested` event** — the manual escape hatch for cutting a fresh
  release by event instead of by hand.

**Develop-sync:** every watch tick also compares each watched repo's local
`develop` against origin. Strictly ahead → fast-forward push through the same
audited git-guard path as the ceremony (`HEX_RELEASE_PIPELINE=1` + post-push
SHA verify). Diverged (or origin branch missing) → loud operator alert, NEVER
auto-resolved (no pull/rebase/reset/force-push). In sync or behind-only →
nothing.

`hex release cut` will:
1. Take an exclusive lock and pin the `develop` SHA (`main` for `--hotfix`)
2. Run the full gate battery (clean tree, Docker E2E, sanitize, codex-parity, autonomy)
3. Compute the next semver (refusing anything not greater than the latest tag)
4. Branch `release/X.Y.Z`, bump `system/harness/Cargo.toml` + `system/version.txt`,
   rebuild so `Cargo.lock` updates, commit `bump: vX.Y.Z`
5. Merge `--no-ff` to `main` and tag `vX.Y.Z`
6. Reconcile `develop` with `origin/develop` (fast-forward when behind, a real
   merge when diverged; a conflict aborts before any push), then back-merge
   `main` to `develop`
7. Push `main`, the tag, and `develop` in ONE atomic push (origin holds all three
   or none), verify each ref, then create the GitHub release if the repo profile
   enables it

**If origin rejects the push:** nothing is on origin. When `origin/develop` moved
again between the reconcile and the push, the ceremony reconciles once more and
retries once; a second rejection aborts with `Nothing was pushed` and numbered
by-hand commands (merge `origin/develop`, merge `main`, one atomic push, delete
the release branch; plus an unwind that resets `main` and `develop` to their
pre-cut commits).

**If git fails without a rejection from origin** (connection lost after send,
git killed, a client-side hook): the outcome is unknown, so the ceremony asks
origin. If every ref is exactly where it was before the push, that is a verified
`Nothing was pushed` with the same by-hand commands. If all three refs are at
the expected commits, the release completes normally. Anything else is the
publish-state path below.

**If verification disagrees with the push:** the ceremony prints a `PUBLISH
STATE` block with one line per ref: `on origin at <sha>`, `on origin at <sha>
(expected <sha>)`, `not on origin`, or `could not verify (<error>)`. It creates
the GitHub release only when the tag is on origin at the expected commit, runs
cleanup, and exits non-zero with a push command for refs origin confirmed absent
and an `ls-remote` command for refs it could not confirm.

If `develop` does not exist yet, the command refuses and prints the bootstrap
one-liner: `git branch develop main && git push origin develop`.

**Other repos:** the hex-foundation profile is built in. Additional repos load
from `$HEX_DIR/.hex/config/releases.toml` — see `system/templates/releases.toml.example`
(includes a `boi` profile) and the module docs in `system/harness/src/release.rs`.
Repos with no matching profile are refused.

Then in hex, run `/hex-upgrade` to pull the new release and pin VERSIONS.

## hex-doctor version-sync check

`hex-doctor` includes a `version-sync` check that asserts:
- Compiled binary version == Cargo.toml version (FAIL if mismatch)
- Latest git tag == Cargo.toml version (WARN if mismatch — Cargo.toml may be ahead between bumps)
